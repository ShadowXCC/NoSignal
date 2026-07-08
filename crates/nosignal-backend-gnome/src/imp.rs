use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use nosignal_core::{
    ApplyMode, BackendError, Capabilities, DisplayBackend, EdidId, LayoutPlan, Mode, Output,
    OutputIdentity, Topology, TopologyEvent, Transform, topology::connector_is_builtin,
};
use std::collections::HashMap;
use zbus::zvariant::{OwnedValue, Value};
use zbus::{Connection, proxy};

/// Monitor spec tuple: (connector, vendor, product, serial).
type MonitorSpec = (String, String, String, String);
/// Mode tuple: (id, width, height, refresh, preferred_scale, supported_scales, properties).
type MonitorMode = (
    String,
    i32,
    i32,
    f64,
    f64,
    Vec<f64>,
    HashMap<String, OwnedValue>,
);
/// Monitor tuple: (spec, modes, properties).
type Monitor = (MonitorSpec, Vec<MonitorMode>, HashMap<String, OwnedValue>);
/// Logical monitor tuple: (x, y, scale, transform, primary, monitor specs, properties).
type LogicalMonitor = (
    i32,
    i32,
    f64,
    u32,
    bool,
    Vec<MonitorSpec>,
    HashMap<String, OwnedValue>,
);
type CurrentState = (
    u32,
    Vec<Monitor>,
    Vec<LogicalMonitor>,
    HashMap<String, OwnedValue>,
);

/// Monitor assignment for apply: (connector, mode_id, properties).
type ApplyAssignment<'a> = (String, String, HashMap<String, Value<'a>>);
/// Logical monitor for apply: (x, y, scale, transform, primary, assignments).
type ApplyLogical<'a> = (i32, i32, f64, u32, bool, Vec<ApplyAssignment<'a>>);

#[proxy(
    interface = "org.gnome.Mutter.DisplayConfig",
    default_service = "org.gnome.Mutter.DisplayConfig",
    default_path = "/org/gnome/Mutter/DisplayConfig"
)]
trait MutterDisplayConfig {
    fn get_current_state(&self) -> zbus::Result<CurrentState>;

    fn apply_monitors_config(
        &self,
        serial: u32,
        method: u32,
        logical_monitors: Vec<ApplyLogical<'_>>,
        properties: HashMap<String, Value<'_>>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    fn monitors_changed(&self) -> zbus::Result<()>;
}

pub struct GnomeBackend {
    proxy: MutterDisplayConfigProxy<'static>,
    connection: Connection,
}

impl GnomeBackend {
    /// Connect to the session bus and verify Mutter's DisplayConfig is there.
    pub async fn new() -> Result<Self, BackendError> {
        let connection = Connection::session()
            .await
            .map_err(|e| BackendError::Unavailable(format!("no session bus: {e}")))?;
        let proxy = MutterDisplayConfigProxy::new(&connection)
            .await
            .map_err(|e| BackendError::Unavailable(format!("no Mutter DisplayConfig: {e}")))?;
        // Probe: fails fast when not running under GNOME.
        proxy.get_current_state().await.map_err(|e| {
            BackendError::Unavailable(format!("Mutter DisplayConfig not answering: {e}"))
        })?;
        Ok(Self { proxy, connection })
    }

    async fn current_state(&self) -> Result<CurrentState, BackendError> {
        self.proxy
            .get_current_state()
            .await
            .map_err(|e| BackendError::Server(format!("GetCurrentState: {e}")))
    }
}

fn prop_bool(props: &HashMap<String, OwnedValue>, key: &str) -> bool {
    props
        .get(key)
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(false)
}

fn prop_string(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    props
        .get(key)
        .and_then(|v| <&str>::try_from(v).ok().map(String::from))
}

fn refresh_to_mhz(refresh: f64) -> u32 {
    (refresh * 1000.0).round() as u32
}

fn spec_to_identity(spec: &MonitorSpec) -> OutputIdentity {
    let (connector, vendor, product, serial) = spec;
    let unknown = |s: &String| s.is_empty() || s == "unknown";
    let edid = if unknown(vendor) && unknown(product) && unknown(serial) {
        None
    } else {
        Some(EdidId {
            vendor: vendor.clone(),
            product: product.clone(),
            serial: serial.clone(),
        })
    };
    OutputIdentity::new(connector.clone(), edid)
}

fn state_to_topology(state: &CurrentState) -> Topology {
    let (serial, monitors, logical_monitors, _props) = state;
    let outputs = monitors
        .iter()
        .map(|(spec, modes, props)| {
            let identity = spec_to_identity(spec);
            // Which logical monitor (if any) drives this connector?
            let logical = logical_monitors
                .iter()
                .find(|lm| lm.5.iter().any(|s| s.0 == spec.0));

            let mut current = None;
            let mut preferred = None;
            let mut all = Vec::with_capacity(modes.len());
            for (_id, w, h, refresh, _pref_scale, _scales, mprops) in modes {
                let mode = Mode {
                    width: *w as u32,
                    height: *h as u32,
                    refresh_mhz: refresh_to_mhz(*refresh),
                };
                all.push(mode);
                if prop_bool(mprops, "is-current") {
                    current = Some(mode);
                }
                if prop_bool(mprops, "is-preferred") {
                    preferred = Some(mode);
                }
            }

            Output {
                builtin: prop_bool(props, "is-builtin")
                    || connector_is_builtin(&identity.connector),
                display_name: prop_string(props, "display-name")
                    .unwrap_or_else(|| identity.connector.clone()),
                identity,
                alias: None,
                enabled: logical.is_some(),
                mode: current,
                preferred_mode: preferred,
                modes: all,
                position: logical.map(|lm| (lm.0, lm.1)).unwrap_or((0, 0)),
                scale: logical.map(|lm| lm.2).unwrap_or(1.0),
                transform: logical
                    .and_then(|lm| Transform::from_u8(lm.3 as u8))
                    .unwrap_or_default(),
                primary: logical.map(|lm| lm.4).unwrap_or(false),
            }
        })
        .collect();

    Topology {
        serial: serial.to_string(),
        outputs,
    }
}

/// Find the Mutter mode id for a planned mode on a monitor: exact
/// (width, height, mHz) first, then closest refresh at the same resolution.
fn find_mode_id(modes: &[MonitorMode], wanted: Mode) -> Option<(String, f64, Vec<f64>)> {
    let same_res = || {
        modes
            .iter()
            .filter(|m| m.1 as u32 == wanted.width && m.2 as u32 == wanted.height)
    };
    same_res()
        .find(|m| refresh_to_mhz(m.3) == wanted.refresh_mhz)
        .or_else(|| {
            same_res().min_by_key(|m| {
                (refresh_to_mhz(m.3) as i64 - i64::from(wanted.refresh_mhz)).unsigned_abs()
            })
        })
        .map(|m| (m.0.clone(), m.4, m.5.clone()))
}

/// Mutter rejects scales it didn't offer; snap to the closest supported one.
fn snap_scale(wanted: f64, preferred: f64, supported: &[f64]) -> f64 {
    if supported.is_empty() {
        return if wanted > 0.0 { wanted } else { preferred };
    }
    supported
        .iter()
        .copied()
        .min_by(|a, b| {
            (a - wanted)
                .abs()
                .partial_cmp(&(b - wanted).abs())
                .expect("scales are finite")
        })
        .expect("non-empty")
}

fn build_logical_monitors<'a>(
    plan: &LayoutPlan,
    monitors: &[Monitor],
) -> Result<Vec<ApplyLogical<'a>>, BackendError> {
    // Group enabled outputs by position: outputs sharing a position form one
    // logical monitor (that is how Mutter represents mirroring).
    let mut groups: Vec<(i32, i32, Vec<&nosignal_core::PlannedOutput>)> = Vec::new();
    for planned in plan.outputs.iter().filter(|p| p.enabled) {
        match groups
            .iter_mut()
            .find(|(x, y, _)| (*x, *y) == planned.position)
        {
            Some((_, _, members)) => members.push(planned),
            None => groups.push((planned.position.0, planned.position.1, vec![planned])),
        }
    }

    let mut result = Vec::with_capacity(groups.len());
    for (x, y, members) in groups {
        let head = members[0];
        let mut assignments = Vec::with_capacity(members.len());
        let mut scale = None;
        for planned in &members {
            let connector = &planned.identity.connector;
            let monitor = monitors
                .iter()
                .find(|m| &m.0.0 == connector)
                .ok_or_else(|| BackendError::UnknownOutput(connector.clone()))?;
            let wanted_mode = planned.mode.ok_or_else(|| {
                BackendError::InvalidLayout(format!("enabled output {connector} has no mode"))
            })?;
            let (mode_id, preferred_scale, supported_scales) =
                find_mode_id(&monitor.1, wanted_mode).ok_or_else(|| {
                    BackendError::InvalidLayout(format!(
                        "mode {wanted_mode} not available on {connector}"
                    ))
                })?;
            scale.get_or_insert(snap_scale(
                planned.scale,
                preferred_scale,
                &supported_scales,
            ));
            assignments.push((connector.clone(), mode_id, HashMap::new()));
        }
        result.push((
            x,
            y,
            scale.unwrap_or(1.0),
            u32::from(head.transform.to_u8()),
            head.primary,
            assignments,
        ));
    }
    Ok(result)
}

#[async_trait]
impl DisplayBackend for GnomeBackend {
    fn name(&self) -> &'static str {
        "gnome"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            // method=2 writes ~/.config/monitors.xml — GNOME itself keeps the
            // layout across reboots for this hardware combination.
            native_persistence: true,
            events: true,
        }
    }

    async fn snapshot(&self) -> Result<Topology, BackendError> {
        Ok(state_to_topology(&self.current_state().await?))
    }

    async fn apply(&self, plan: &LayoutPlan, mode: ApplyMode) -> Result<(), BackendError> {
        // Re-read state: apply needs the *current* serial and mode ids.
        let state = self.current_state().await?;
        if plan.serial != state.0.to_string() {
            return Err(BackendError::StaleSerial);
        }

        let logical_monitors = build_logical_monitors(plan, &state.1)?;
        let method = match mode {
            ApplyMode::Verify => 0,
            ApplyMode::Temporary => 1,
            ApplyMode::Persistent => 2,
        };
        tracing::debug!(
            method,
            logical_monitors = logical_monitors.len(),
            "ApplyMonitorsConfig"
        );
        self.proxy
            .apply_monitors_config(state.0, method, logical_monitors, HashMap::new())
            .await
            .map_err(|e| BackendError::Server(format!("ApplyMonitorsConfig: {e}")))
    }

    async fn watch(&self) -> Result<BoxStream<'static, TopologyEvent>, BackendError> {
        let proxy = MutterDisplayConfigProxy::new(&self.connection)
            .await
            .map_err(|e| BackendError::Server(format!("proxy for signals: {e}")))?;
        let stream = proxy
            .receive_monitors_changed()
            .await
            .map_err(|e| BackendError::Server(format!("subscribe MonitorsChanged: {e}")))?;
        Ok(stream.map(|_| TopologyEvent::Changed).boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode_tuple(id: &str, w: i32, h: i32, hz: f64, current: bool) -> MonitorMode {
        let mut props = HashMap::new();
        if current {
            props.insert("is-current".to_string(), OwnedValue::from(true));
        }
        (id.into(), w, h, hz, 1.0, vec![1.0, 2.0], props)
    }

    fn sample_state() -> CurrentState {
        let dp: Monitor = (
            (
                "DP-1".into(),
                "DEL".into(),
                "DELL U2723QE".into(),
                "ABC123".into(),
            ),
            vec![
                mode_tuple("3840x2160@59.997", 3840, 2160, 59.997, true),
                mode_tuple("1920x1080@60.000", 1920, 1080, 60.0, false),
            ],
            HashMap::new(),
        );
        let tv: Monitor = (
            (
                "HDMI-A-1".into(),
                "SAM".into(),
                "SAMSUNG".into(),
                "0x01000000".into(),
            ),
            vec![
                mode_tuple("3840x2160@60.000", 3840, 2160, 60.0, true),
                mode_tuple("3840x2160@119.880", 3840, 2160, 119.88, false),
            ],
            HashMap::new(),
        );
        // Only DP-1 is in a logical monitor; the TV is disabled.
        let logical: LogicalMonitor = (
            0,
            0,
            1.0,
            0,
            true,
            vec![(
                "DP-1".into(),
                "DEL".into(),
                "DELL U2723QE".into(),
                "ABC123".into(),
            )],
            HashMap::new(),
        );
        (7, vec![dp, tv], vec![logical], HashMap::new())
    }

    #[test]
    fn state_maps_to_topology_with_disabled_monitor() {
        let topo = state_to_topology(&sample_state());
        assert_eq!(topo.serial, "7");
        assert_eq!(topo.outputs.len(), 2);

        let dp = topo.find_connector("DP-1").unwrap();
        assert!(dp.enabled);
        assert!(dp.primary);
        assert_eq!(dp.mode.unwrap().refresh_mhz, 59_997);
        assert_eq!(dp.identity.edid.as_ref().unwrap().vendor, "DEL");

        let tv = topo.find_connector("HDMI-A-1").unwrap();
        assert!(!tv.enabled, "monitor absent from logical monitors = off");
        assert_eq!(tv.modes.len(), 2);
    }

    #[test]
    fn plan_omits_disabled_outputs_from_logical_monitors() {
        let state = sample_state();
        let topo = state_to_topology(&state);
        let plan = LayoutPlan::from_topology(&topo);
        let logical = build_logical_monitors(&plan, &state.1).unwrap();
        // TV is disabled → only one logical monitor, for DP-1.
        assert_eq!(logical.len(), 1);
        assert_eq!(logical[0].5[0].0, "DP-1");
        assert_eq!(logical[0].5[0].1, "3840x2160@59.997");
    }

    #[test]
    fn enabling_uses_closest_mode_id_and_supported_scale() {
        let state = sample_state();
        let topo = state_to_topology(&state);
        let mut plan = LayoutPlan::from_topology(&topo);
        {
            let tv = plan.find_connector_mut("HDMI-A-1").unwrap();
            tv.enabled = true;
            tv.mode = Some(Mode {
                width: 3840,
                height: 2160,
                refresh_mhz: 120_000, // asks for 120, closest is 119.88
            });
            tv.position = (3840, 0);
            tv.scale = 1.7; // unsupported → snaps to 2.0
        }
        let logical = build_logical_monitors(&plan, &state.1).unwrap();
        let tv_lm = logical.iter().find(|lm| lm.5[0].0 == "HDMI-A-1").unwrap();
        assert_eq!(tv_lm.5[0].1, "3840x2160@119.880");
        assert_eq!(tv_lm.2, 2.0);
    }

    #[test]
    fn mirrored_outputs_share_one_logical_monitor() {
        let state = sample_state();
        let topo = state_to_topology(&state);
        let mut plan = LayoutPlan::from_topology(&topo);
        {
            let tv = plan.find_connector_mut("HDMI-A-1").unwrap();
            tv.enabled = true;
            tv.mode = Some(Mode {
                width: 3840,
                height: 2160,
                refresh_mhz: 60_000,
            });
            tv.position = (0, 0); // same position as DP-1 = mirror
        }
        let logical = build_logical_monitors(&plan, &state.1).unwrap();
        assert_eq!(logical.len(), 1);
        assert_eq!(logical[0].5.len(), 2);
    }
}
