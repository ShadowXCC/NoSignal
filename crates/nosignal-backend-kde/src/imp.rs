use async_trait::async_trait;
use futures::StreamExt;
use futures::channel::mpsc;
use futures::stream::BoxStream;
use nosignal_core::{
    ApplyMode, BackendError, Capabilities, DisplayBackend, EdidId, LayoutPlan, Mode, Output,
    OutputIdentity, Topology, TopologyEvent, Transform, topology::connector_is_builtin,
};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use std::time::Duration;

const POLL_INTERVAL: Duration = Duration::from_secs(2);

pub struct KdeBackend {
    watchers: Mutex<Vec<mpsc::UnboundedSender<TopologyEvent>>>,
}

impl KdeBackend {
    /// Probe `kscreen-doctor -j`; unavailable outside a KScreen session.
    pub async fn new() -> Result<Self, BackendError> {
        let json = run_kscreen_doctor(&["-j"]).await?;
        parse_topology(&json)?;
        let backend = Self {
            watchers: Mutex::new(Vec::new()),
        };
        Ok(backend)
    }

    fn spawn_poller(&self, initial_serial: String) {
        let watchers = {
            let guard = self.watchers.lock().unwrap();
            guard.clone()
        };
        tokio::spawn(async move {
            let mut last = initial_serial;
            let mut watchers = watchers;
            loop {
                tokio::time::sleep(POLL_INTERVAL).await;
                let Ok(json) = run_kscreen_doctor(&["-j"]).await else {
                    continue;
                };
                let serial = serial_of(&json);
                if serial != last {
                    last = serial;
                    watchers.retain(|tx| tx.unbounded_send(TopologyEvent::Changed).is_ok());
                    if watchers.is_empty() {
                        return;
                    }
                }
            }
        });
    }
}

async fn run_kscreen_doctor(args: &[&str]) -> Result<String, BackendError> {
    let output = tokio::process::Command::new("kscreen-doctor")
        .args(args)
        .output()
        .await
        .map_err(|e| BackendError::Unavailable(format!("kscreen-doctor not runnable: {e}")))?;
    if !output.status.success() {
        return Err(BackendError::Server(format!(
            "kscreen-doctor {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Configuration serial: a hash of the JSON snapshot. KScreen has no serial
/// concept; a changed snapshot means outstanding plans are stale.
fn serial_of(json: &str) -> String {
    let mut hasher = DefaultHasher::new();
    json.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

fn parse_topology(json: &str) -> Result<Topology, BackendError> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| BackendError::Server(format!("kscreen-doctor JSON: {e}")))?;
    let outputs_json = value
        .get("outputs")
        .and_then(|o| o.as_array())
        .ok_or_else(|| BackendError::Server("kscreen-doctor JSON: no outputs".into()))?;

    let mut outputs = Vec::new();
    for o in outputs_json {
        if !o.get("connected").and_then(|v| v.as_bool()).unwrap_or(true) {
            continue;
        }
        let name = o
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        // Plasma 6 exposes vendor/model/serialNumber; older versions don't —
        // connector identity is the fallback (documented KDE limitation).
        let vendor = o.get("vendor").and_then(|v| v.as_str()).unwrap_or("");
        let model = o.get("model").and_then(|v| v.as_str()).unwrap_or("");
        let serial = o.get("serialNumber").and_then(|v| v.as_str()).unwrap_or("");
        let edid =
            (!vendor.is_empty() || !model.is_empty() || !serial.is_empty()).then(|| EdidId {
                vendor: vendor.to_string(),
                product: model.to_string(),
                serial: serial.to_string(),
            });

        let enabled = o.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
        let current_mode_id = o.get("currentModeId").and_then(|v| v.as_str());
        let preferred_ids: Vec<&str> = o
            .get("preferredModes")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
            .unwrap_or_default();

        let mut modes = Vec::new();
        let mut current = None;
        let mut preferred = None;
        for m in o
            .get("modes")
            .and_then(|v| v.as_array())
            .unwrap_or(&Vec::new())
        {
            let (Some(w), Some(h)) = (
                m.pointer("/size/width").and_then(|v| v.as_u64()),
                m.pointer("/size/height").and_then(|v| v.as_u64()),
            ) else {
                continue;
            };
            let refresh = m
                .get("refreshRate")
                .and_then(|v| v.as_f64())
                .unwrap_or(60.0);
            let mode = Mode {
                width: w as u32,
                height: h as u32,
                refresh_mhz: (refresh * 1000.0).round() as u32,
            };
            modes.push(mode);
            let id = m.get("id").and_then(|v| v.as_str());
            if id.is_some() && id == current_mode_id {
                current = Some(mode);
            }
            if let Some(id) = id
                && preferred_ids.contains(&id)
                && preferred.is_none()
            {
                preferred = Some(mode);
            }
        }

        outputs.push(Output {
            builtin: o
                .get("type")
                .and_then(|v| v.as_str())
                .is_some_and(|t| t.eq_ignore_ascii_case("panel"))
                || connector_is_builtin(&name),
            display_name: if model.is_empty() {
                name.clone()
            } else {
                format!("{vendor} {model}").trim().to_string()
            },
            identity: OutputIdentity::new(name, edid),
            alias: None,
            enabled,
            mode: current,
            preferred_mode: preferred,
            modes,
            position: (
                o.pointer("/pos/x").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
                o.pointer("/pos/y").and_then(|v| v.as_i64()).unwrap_or(0) as i32,
            ),
            scale: o.get("scale").and_then(|v| v.as_f64()).unwrap_or(1.0),
            transform: kscreen_rotation_to_transform(
                o.get("rotation").and_then(|v| v.as_u64()).unwrap_or(1),
            ),
            primary: o.get("priority").and_then(|v| v.as_u64()) == Some(1),
        });
    }

    Ok(Topology {
        serial: serial_of(json),
        outputs,
    })
}

/// KScreen rotation flags: 1 = none, 2 = left (90°), 4 = inverted (180°),
/// 8 = right (270°).
fn kscreen_rotation_to_transform(rotation: u64) -> Transform {
    match rotation {
        2 => Transform::Rot90,
        4 => Transform::Rot180,
        8 => Transform::Rot270,
        _ => Transform::Normal,
    }
}

fn transform_to_kscreen_rotation(t: Transform) -> &'static str {
    match t {
        Transform::Rot90 | Transform::FlippedRot90 => "left",
        Transform::Rot180 | Transform::FlippedRot180 => "inverted",
        Transform::Rot270 | Transform::FlippedRot270 => "right",
        _ => "none",
    }
}

/// Build the kscreen-doctor setter arguments for a whole-layout plan.
fn build_args(plan: &LayoutPlan, live: &Topology) -> Result<Vec<String>, BackendError> {
    let mut args = Vec::new();
    let mut priority = 2u32;
    for planned in &plan.outputs {
        let name = &planned.identity.connector;
        let output = live
            .find_connector(name)
            .ok_or_else(|| BackendError::UnknownOutput(name.clone()))?;
        if !planned.enabled {
            args.push(format!("output.{name}.disable"));
            continue;
        }
        let mode = planned.mode.ok_or_else(|| {
            BackendError::InvalidLayout(format!("enabled output {name} has no mode"))
        })?;
        if !output.modes.contains(&mode) {
            return Err(BackendError::InvalidLayout(format!(
                "mode {mode} not available on {name}"
            )));
        }
        args.push(format!("output.{name}.enable"));
        args.push(format!(
            "output.{name}.mode.{}x{}@{}",
            mode.width,
            mode.height,
            (f64::from(mode.refresh_mhz) / 1000.0).round() as u32
        ));
        args.push(format!(
            "output.{name}.position.{},{}",
            planned.position.0, planned.position.1
        ));
        args.push(format!("output.{name}.scale.{}", planned.scale));
        args.push(format!(
            "output.{name}.rotation.{}",
            transform_to_kscreen_rotation(planned.transform)
        ));
        if planned.primary {
            args.push(format!("output.{name}.priority.1"));
        } else {
            args.push(format!("output.{name}.priority.{priority}"));
            priority += 1;
        }
    }
    Ok(args)
}

#[async_trait]
impl DisplayBackend for KdeBackend {
    fn name(&self) -> &'static str {
        "kde"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            // KScreen remembers configurations per hardware combination.
            native_persistence: true,
            // Poll-based internally, surfaced as events.
            events: true,
        }
    }

    async fn snapshot(&self) -> Result<Topology, BackendError> {
        let json = run_kscreen_doctor(&["-j"]).await?;
        parse_topology(&json)
    }

    async fn apply(&self, plan: &LayoutPlan, mode: ApplyMode) -> Result<(), BackendError> {
        let json = run_kscreen_doctor(&["-j"]).await?;
        let live = parse_topology(&json)?;
        if plan.serial != live.serial {
            return Err(BackendError::StaleSerial);
        }
        let args = build_args(plan, &live)?;
        if mode == ApplyMode::Verify {
            return Ok(());
        }
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        run_kscreen_doctor(&arg_refs).await?;
        let watchers = self.watchers.lock().unwrap().clone();
        for tx in watchers {
            let _ = tx.unbounded_send(TopologyEvent::Changed);
        }
        Ok(())
    }

    async fn watch(&self) -> Result<BoxStream<'static, TopologyEvent>, BackendError> {
        let (tx, rx) = mpsc::unbounded();
        let start_poller = {
            let mut watchers = self.watchers.lock().unwrap();
            watchers.push(tx);
            watchers.len() == 1
        };
        if start_poller {
            let json = run_kscreen_doctor(&["-j"]).await?;
            self.spawn_poller(serial_of(&json));
        }
        Ok(rx.boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "outputs": [
        {
          "id": 1, "name": "DP-1", "type": "DisplayPort", "connected": true,
          "enabled": true, "priority": 1,
          "pos": {"x": 0, "y": 0}, "scale": 1, "rotation": 1,
          "currentModeId": "0", "preferredModes": ["0"],
          "vendor": "Dell Inc.", "model": "U2723QE", "serialNumber": "ABC123",
          "modes": [
            {"id": "0", "name": "3840x2160@60", "refreshRate": 59.997,
             "size": {"width": 3840, "height": 2160}},
            {"id": "1", "name": "1920x1080@60", "refreshRate": 60.0,
             "size": {"width": 1920, "height": 1080}}
          ]
        },
        {
          "id": 2, "name": "HDMI-A-1", "type": "HDMI", "connected": true,
          "enabled": false, "priority": 0,
          "pos": {"x": 3840, "y": 0}, "scale": 2, "rotation": 1,
          "modes": [
            {"id": "5", "name": "3840x2160@60", "refreshRate": 60.0,
             "size": {"width": 3840, "height": 2160}}
          ]
        },
        {
          "id": 3, "name": "DP-9", "connected": false, "enabled": false,
          "modes": []
        }
      ]
    }"#;

    #[test]
    fn parses_kscreen_json() {
        let topo = parse_topology(SAMPLE).unwrap();
        assert_eq!(topo.outputs.len(), 2, "disconnected outputs are skipped");

        let dp = topo.find_connector("DP-1").unwrap();
        assert!(dp.enabled);
        assert!(dp.primary);
        assert_eq!(dp.mode.unwrap().refresh_mhz, 59_997);
        let edid = dp.identity.edid.as_ref().unwrap();
        assert_eq!(edid.serial, "ABC123");

        let tv = topo.find_connector("HDMI-A-1").unwrap();
        assert!(!tv.enabled);
        assert!(tv.identity.edid.is_none(), "no vendor fields = no EDID id");
    }

    #[test]
    fn builds_apply_args_for_disable_and_enable() {
        let topo = parse_topology(SAMPLE).unwrap();
        let mut plan = LayoutPlan::from_topology(&topo);
        {
            let tv = plan.find_connector_mut("HDMI-A-1").unwrap();
            tv.enabled = true;
            tv.mode = Some(Mode {
                width: 3840,
                height: 2160,
                refresh_mhz: 60_000,
            });
        }
        plan.set_enabled("DP-1", false);
        nosignal_core::layout::normalize(&mut plan);

        let args = build_args(&plan, &topo).unwrap();
        assert!(args.contains(&"output.DP-1.disable".to_string()));
        assert!(args.contains(&"output.HDMI-A-1.enable".to_string()));
        assert!(args.contains(&"output.HDMI-A-1.mode.3840x2160@60".to_string()));
        assert!(args.contains(&"output.HDMI-A-1.priority.1".to_string()));
    }

    #[test]
    fn unavailable_mode_is_invalid_layout() {
        let topo = parse_topology(SAMPLE).unwrap();
        let mut plan = LayoutPlan::from_topology(&topo);
        {
            let dp = plan.find_connector_mut("DP-1").unwrap();
            dp.mode = Some(Mode {
                width: 2560,
                height: 1440,
                refresh_mhz: 144_000,
            });
        }
        let err = build_args(&plan, &topo).unwrap_err();
        assert!(matches!(err, BackendError::InvalidLayout(_)));
    }

    #[test]
    fn rotation_mapping_round_trips_the_basics() {
        assert_eq!(kscreen_rotation_to_transform(1), Transform::Normal);
        assert_eq!(kscreen_rotation_to_transform(2), Transform::Rot90);
        assert_eq!(transform_to_kscreen_rotation(Transform::Rot270), "right");
    }
}
