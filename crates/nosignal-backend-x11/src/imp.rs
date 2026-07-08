use async_trait::async_trait;
use futures::StreamExt;
use futures::channel::mpsc;
use futures::stream::BoxStream;
use nosignal_core::{
    ApplyMode, BackendError, Capabilities, DisplayBackend, LayoutPlan, Mode, Output,
    OutputIdentity, PlannedOutput, Topology, TopologyEvent, Transform,
    topology::connector_is_builtin,
};
use std::collections::HashMap;
use x11rb::connection::Connection;
use x11rb::protocol::randr::{
    self, ConnectionExt as RandrExt, GetScreenResourcesCurrentReply, ModeFlag, ModeInfo,
    NotifyMask, Rotation,
};
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as XprotoExt, Window};
use x11rb::rust_connection::RustConnection;

pub struct X11Backend {
    conn: RustConnection,
    root: Window,
}

impl X11Backend {
    pub async fn new() -> Result<Self, BackendError> {
        if std::env::var_os("DISPLAY").is_none() {
            return Err(BackendError::Unavailable("DISPLAY is not set".into()));
        }
        let (conn, screen_num) = x11rb::connect(None)
            .map_err(|e| BackendError::Unavailable(format!("cannot connect to X11: {e}")))?;
        let root = conn.setup().roots[screen_num].root;
        let version = conn
            .randr_query_version(1, 5)
            .map_err(|e| BackendError::Unavailable(format!("RandR: {e}")))?
            .reply()
            .map_err(|e| BackendError::Unavailable(format!("RandR: {e}")))?;
        if (version.major_version, version.minor_version) < (1, 3) {
            return Err(BackendError::Unavailable(format!(
                "RandR {}.{} too old (need 1.3+)",
                version.major_version, version.minor_version
            )));
        }
        Ok(Self { conn, root })
    }

    fn resources(&self) -> Result<GetScreenResourcesCurrentReply, BackendError> {
        self.conn
            .randr_get_screen_resources_current(self.root)
            .map_err(server_err)?
            .reply()
            .map_err(server_err)
    }

    fn output_edid(&self, output: randr::Output) -> Option<nosignal_core::EdidId> {
        let atom = self
            .conn
            .intern_atom(true, b"EDID")
            .ok()?
            .reply()
            .ok()?
            .atom;
        let prop = self
            .conn
            .randr_get_output_property(output, atom, u32::from(AtomEnum::ANY), 0, 256, false, false)
            .ok()?
            .reply()
            .ok()?;
        nosignal_core::edid::parse(&prop.data)
    }
}

fn server_err(e: impl std::fmt::Display) -> BackendError {
    BackendError::Server(format!("X11: {e}"))
}

/// Refresh rate of a RandR mode in millihertz, with the interlace/doublescan
/// adjustments xrandr applies.
fn mode_refresh_mhz(mode: &ModeInfo) -> u32 {
    let mut vtotal = f64::from(mode.vtotal);
    if mode.mode_flags.contains(ModeFlag::DOUBLE_SCAN) {
        vtotal *= 2.0;
    }
    if mode.mode_flags.contains(ModeFlag::INTERLACE) {
        vtotal /= 2.0;
    }
    let denom = f64::from(mode.htotal) * vtotal;
    if denom <= 0.0 {
        return 0;
    }
    (f64::from(mode.dot_clock) / denom * 1000.0).round() as u32
}

fn mode_of(info: &ModeInfo) -> Mode {
    Mode {
        width: u32::from(info.width),
        height: u32::from(info.height),
        refresh_mhz: mode_refresh_mhz(info),
    }
}

fn rotation_to_transform(r: Rotation) -> Transform {
    let reflected = r.contains(Rotation::REFLECT_X) || r.contains(Rotation::REFLECT_Y);
    let base = if r.contains(Rotation::ROTATE90) {
        1
    } else if r.contains(Rotation::ROTATE180) {
        2
    } else if r.contains(Rotation::ROTATE270) {
        3
    } else {
        0
    };
    Transform::from_u8(base + if reflected { 4 } else { 0 }).unwrap_or_default()
}

fn transform_to_rotation(t: Transform) -> Rotation {
    match t {
        Transform::Normal => Rotation::ROTATE0,
        Transform::Rot90 => Rotation::ROTATE90,
        Transform::Rot180 => Rotation::ROTATE180,
        Transform::Rot270 => Rotation::ROTATE270,
        Transform::Flipped => Rotation::ROTATE0 | Rotation::REFLECT_X,
        Transform::FlippedRot90 => Rotation::ROTATE90 | Rotation::REFLECT_X,
        Transform::FlippedRot180 => Rotation::ROTATE180 | Rotation::REFLECT_X,
        Transform::FlippedRot270 => Rotation::ROTATE270 | Rotation::REFLECT_X,
    }
}

/// Size an output occupies on screen (rotation swaps the mode dimensions;
/// X11 has no fractional per-output scale).
fn planned_size(p: &PlannedOutput) -> (u16, u16) {
    let Some(mode) = p.mode else { return (0, 0) };
    match p.transform {
        Transform::Rot90
        | Transform::Rot270
        | Transform::FlippedRot90
        | Transform::FlippedRot270 => (mode.height as u16, mode.width as u16),
        _ => (mode.width as u16, mode.height as u16),
    }
}

/// Bounding box of the enabled outputs → required screen size.
fn screen_bounds(plan: &LayoutPlan) -> (u16, u16) {
    let mut w = 0i32;
    let mut h = 0i32;
    for p in plan.outputs.iter().filter(|p| p.enabled) {
        let (pw, ph) = planned_size(p);
        w = w.max(p.position.0 + i32::from(pw));
        h = h.max(p.position.1 + i32::from(ph));
    }
    (w.max(1) as u16, h.max(1) as u16)
}

struct OutputSnapshot {
    output: randr::Output,
    crtc: randr::Crtc,
    candidate_crtcs: Vec<randr::Crtc>,
    mode_ids: Vec<(randr::Mode, Mode)>,
}

#[async_trait]
impl DisplayBackend for X11Backend {
    fn name(&self) -> &'static str {
        "x11"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            native_persistence: false,
            events: true,
        }
    }

    async fn snapshot(&self) -> Result<Topology, BackendError> {
        let res = self.resources()?;
        let primary = self
            .conn
            .randr_get_output_primary(self.root)
            .map_err(server_err)?
            .reply()
            .map_err(server_err)?
            .output;
        let mode_table: HashMap<u32, &ModeInfo> = res.modes.iter().map(|m| (m.id, m)).collect();

        let mut outputs = Vec::new();
        for &output in &res.outputs {
            let info = self
                .conn
                .randr_get_output_info(output, res.config_timestamp)
                .map_err(server_err)?
                .reply()
                .map_err(server_err)?;
            if info.connection != randr::Connection::CONNECTED {
                continue;
            }
            let name = String::from_utf8_lossy(&info.name).into_owned();
            let edid = self.output_edid(output);

            let modes: Vec<Mode> = info
                .modes
                .iter()
                .filter_map(|id| mode_table.get(id).map(|m| mode_of(m)))
                .collect();
            let preferred = info
                .modes
                .first()
                .filter(|_| info.num_preferred > 0)
                .and_then(|id| mode_table.get(id).map(|m| mode_of(m)));

            let (enabled, mode, position, transform) = if info.crtc != 0 {
                let crtc = self
                    .conn
                    .randr_get_crtc_info(info.crtc, res.config_timestamp)
                    .map_err(server_err)?
                    .reply()
                    .map_err(server_err)?;
                (
                    true,
                    mode_table.get(&crtc.mode).map(|m| mode_of(m)),
                    (i32::from(crtc.x), i32::from(crtc.y)),
                    rotation_to_transform(crtc.rotation),
                )
            } else {
                (false, None, (0, 0), Transform::Normal)
            };

            outputs.push(Output {
                builtin: connector_is_builtin(&name),
                display_name: edid
                    .as_ref()
                    .map(|e| format!("{} {}", e.vendor, e.product))
                    .unwrap_or_else(|| name.clone()),
                identity: OutputIdentity::new(name, edid),
                alias: None,
                enabled,
                mode,
                preferred_mode: preferred,
                modes,
                position,
                scale: 1.0,
                transform,
                primary: output == primary,
            });
        }

        Ok(Topology {
            serial: format!("{}:{}", res.timestamp, res.config_timestamp),
            outputs,
        })
    }

    async fn apply(&self, plan: &LayoutPlan, mode: ApplyMode) -> Result<(), BackendError> {
        let res = self.resources()?;
        if plan.serial != format!("{}:{}", res.timestamp, res.config_timestamp) {
            return Err(BackendError::StaleSerial);
        }
        let mode_table: HashMap<u32, &ModeInfo> = res.modes.iter().map(|m| (m.id, m)).collect();

        // Gather per-output info for the plan.
        let mut snapshots: HashMap<String, OutputSnapshot> = HashMap::new();
        for &output in &res.outputs {
            let info = self
                .conn
                .randr_get_output_info(output, res.config_timestamp)
                .map_err(server_err)?
                .reply()
                .map_err(server_err)?;
            if info.connection != randr::Connection::CONNECTED {
                continue;
            }
            let name = String::from_utf8_lossy(&info.name).into_owned();
            snapshots.insert(
                name,
                OutputSnapshot {
                    output,
                    crtc: info.crtc,
                    candidate_crtcs: info.crtcs.clone(),
                    mode_ids: info
                        .modes
                        .iter()
                        .filter_map(|id| mode_table.get(id).map(|m| (*id, mode_of(m))))
                        .collect(),
                },
            );
        }

        // Assign CRTCs and mode ids for enabled outputs.
        struct Assignment {
            crtc: randr::Crtc,
            output: randr::Output,
            mode_id: randr::Mode,
            x: i16,
            y: i16,
            rotation: Rotation,
        }
        let mut used_crtcs: Vec<randr::Crtc> = Vec::new();
        let mut assignments = Vec::new();
        for planned in plan.outputs.iter().filter(|p| p.enabled) {
            let name = &planned.identity.connector;
            let snap = snapshots
                .get(name)
                .ok_or_else(|| BackendError::UnknownOutput(name.clone()))?;
            let wanted = planned.mode.ok_or_else(|| {
                BackendError::InvalidLayout(format!("enabled output {name} has no mode"))
            })?;
            let mode_id = snap
                .mode_ids
                .iter()
                .find(|(_, m)| *m == wanted)
                .or_else(|| {
                    snap.mode_ids
                        .iter()
                        .filter(|(_, m)| m.width == wanted.width && m.height == wanted.height)
                        .min_by_key(|(_, m)| {
                            (i64::from(m.refresh_mhz) - i64::from(wanted.refresh_mhz))
                                .unsigned_abs()
                        })
                })
                .map(|(id, _)| *id)
                .ok_or_else(|| {
                    BackendError::InvalidLayout(format!("mode {wanted} not available on {name}"))
                })?;
            let crtc = if snap.crtc != 0 && !used_crtcs.contains(&snap.crtc) {
                snap.crtc
            } else {
                *snap
                    .candidate_crtcs
                    .iter()
                    .find(|c| !used_crtcs.contains(c))
                    .ok_or_else(|| {
                        BackendError::InvalidLayout(format!("no free CRTC for {name}"))
                    })?
            };
            used_crtcs.push(crtc);
            assignments.push(Assignment {
                crtc,
                output: snap.output,
                mode_id,
                x: planned.position.0 as i16,
                y: planned.position.1 as i16,
                rotation: transform_to_rotation(planned.transform),
            });
        }

        if mode == ApplyMode::Verify {
            return Ok(());
        }

        let (new_w, new_h) = screen_bounds(plan);
        let screen = &self.conn.setup().roots[0];
        // Preserve DPI: scale the physical size with the pixel size.
        let mm_w = (f64::from(new_w) * f64::from(screen.width_in_millimeters)
            / f64::from(screen.width_in_pixels))
        .round() as u16;
        let mm_h = (f64::from(new_h) * f64::from(screen.height_in_millimeters)
            / f64::from(screen.height_in_pixels))
        .round() as u16;

        self.conn.grab_server().map_err(server_err)?;
        let result: Result<(), BackendError> = (|| {
            // 1. Detach CRTCs that are active but unused or repositioned.
            for &crtc in &res.crtcs {
                let info = self
                    .conn
                    .randr_get_crtc_info(crtc, res.config_timestamp)
                    .map_err(server_err)?
                    .reply()
                    .map_err(server_err)?;
                if info.mode == 0 {
                    continue;
                }
                let keep = assignments.iter().any(|a| {
                    a.crtc == crtc
                        && a.mode_id == info.mode
                        && a.x == info.x
                        && a.y == info.y
                        && a.rotation == info.rotation
                });
                if !keep {
                    self.conn
                        .randr_set_crtc_config(
                            crtc,
                            x11rb::CURRENT_TIME,
                            res.config_timestamp,
                            0,
                            0,
                            0,
                            Rotation::ROTATE0,
                            &[],
                        )
                        .map_err(server_err)?
                        .reply()
                        .map_err(server_err)?;
                }
            }

            // 2. Resize the screen to the new bounding box.
            self.conn
                .randr_set_screen_size(self.root, new_w, new_h, mm_w.into(), mm_h.into())
                .map_err(server_err)?
                .check()
                .map_err(server_err)?;

            // 3. Configure CRTCs for enabled outputs.
            for a in &assignments {
                self.conn
                    .randr_set_crtc_config(
                        a.crtc,
                        x11rb::CURRENT_TIME,
                        res.config_timestamp,
                        a.x,
                        a.y,
                        a.mode_id,
                        a.rotation,
                        &[a.output],
                    )
                    .map_err(server_err)?
                    .reply()
                    .map_err(server_err)?;
            }

            // 4. Primary output.
            let primary = plan
                .outputs
                .iter()
                .find(|p| p.enabled && p.primary)
                .and_then(|p| snapshots.get(&p.identity.connector))
                .map(|s| s.output)
                .unwrap_or(0);
            self.conn
                .randr_set_output_primary(self.root, primary)
                .map_err(server_err)?
                .check()
                .map_err(server_err)?;
            Ok(())
        })();
        let _ = self.conn.ungrab_server();
        let _ = self.conn.flush();
        result
    }

    async fn watch(&self) -> Result<BoxStream<'static, TopologyEvent>, BackendError> {
        let (tx, rx) = mpsc::unbounded();
        // Dedicated blocking connection: RandR events push into the channel.
        std::thread::Builder::new()
            .name("nosignal-x11-events".into())
            .spawn(move || {
                let Ok((conn, screen_num)) = x11rb::connect(None) else {
                    return;
                };
                let root = conn.setup().roots[screen_num].root;
                let selected = conn.randr_select_input(
                    root,
                    NotifyMask::SCREEN_CHANGE | NotifyMask::CRTC_CHANGE | NotifyMask::OUTPUT_CHANGE,
                );
                if !matches!(selected.map(|c| c.check()), Ok(Ok(()))) {
                    return;
                }
                let _ = conn.flush();
                while let Ok(_event) = conn.wait_for_event() {
                    if tx.unbounded_send(TopologyEvent::Changed).is_err() {
                        return;
                    }
                }
            })
            .map_err(|e| BackendError::Server(format!("event thread: {e}")))?;
        Ok(rx.boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nosignal_core::identity::OutputIdentity;

    fn mode_info(id: u32, w: u16, h: u16, clock: u32, ht: u16, vt: u16) -> ModeInfo {
        ModeInfo {
            id,
            width: w,
            height: h,
            dot_clock: clock,
            hsync_start: 0,
            hsync_end: 0,
            htotal: ht,
            hskew: 0,
            vsync_start: 0,
            vsync_end: 0,
            vtotal: vt,
            name_len: 0,
            mode_flags: ModeFlag::default(),
        }
    }

    #[test]
    fn refresh_math_matches_xrandr() {
        // 1920x1080@60: 148.5 MHz / (2200 * 1125) = 60.000
        let m = mode_info(1, 1920, 1080, 148_500_000, 2200, 1125);
        assert_eq!(mode_refresh_mhz(&m), 60_000);

        let mut interlaced = mode_info(2, 1920, 1080, 74_250_000, 2200, 1125);
        interlaced.mode_flags = ModeFlag::INTERLACE;
        assert_eq!(mode_refresh_mhz(&interlaced), 60_000);
    }

    #[test]
    fn rotation_mapping_round_trips() {
        for t in [
            Transform::Normal,
            Transform::Rot90,
            Transform::Rot180,
            Transform::Rot270,
        ] {
            assert_eq!(rotation_to_transform(transform_to_rotation(t)), t);
        }
    }

    #[test]
    fn screen_bounds_account_for_rotation() {
        let plan = LayoutPlan {
            serial: "s".into(),
            outputs: vec![
                PlannedOutput {
                    identity: OutputIdentity::new("DP-1", None),
                    enabled: true,
                    mode: Some(Mode {
                        width: 1920,
                        height: 1080,
                        refresh_mhz: 60_000,
                    }),
                    position: (0, 0),
                    scale: 1.0,
                    transform: Transform::Rot90,
                    primary: true,
                },
                PlannedOutput {
                    identity: OutputIdentity::new("DP-2", None),
                    enabled: false,
                    mode: None,
                    position: (5000, 0),
                    scale: 1.0,
                    transform: Transform::Normal,
                    primary: false,
                },
            ],
        };
        // Rotated: width becomes 1080; the disabled output is ignored.
        assert_eq!(screen_bounds(&plan), (1080, 1920));
    }
}
