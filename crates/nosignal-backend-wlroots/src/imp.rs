use async_trait::async_trait;
use futures::StreamExt;
use futures::channel::mpsc as fmpsc;
use futures::stream::BoxStream;
use nosignal_core::{
    ApplyMode, BackendError, Capabilities, DisplayBackend, EdidId, LayoutPlan, Mode, Output,
    OutputIdentity, Topology, TopologyEvent, Transform, topology::connector_is_builtin,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use wayland_client::backend::ObjectId;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum, event_created_child};
use wayland_protocols_wlr::output_management::v1::client::{
    zwlr_output_configuration_head_v1::ZwlrOutputConfigurationHeadV1,
    zwlr_output_configuration_v1::{self, ZwlrOutputConfigurationV1},
    zwlr_output_head_v1::{self, ZwlrOutputHeadV1},
    zwlr_output_manager_v1::{self, ZwlrOutputManagerV1},
    zwlr_output_mode_v1::{self, ZwlrOutputModeV1},
};

#[derive(Default)]
struct HeadMirror {
    proxy: Option<ZwlrOutputHeadV1>,
    name: String,
    description: String,
    make: String,
    model: String,
    serial_number: String,
    enabled: bool,
    current_mode: Option<ObjectId>,
    position: (i32, i32),
    transform: Transform,
    scale: f64,
    modes: Vec<ObjectId>,
}

#[derive(Default)]
struct ModeMirror {
    proxy: Option<ZwlrOutputModeV1>,
    width: u32,
    height: u32,
    refresh_mhz: u32,
    preferred: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigResult {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Default)]
struct Shared {
    serial: u32,
    initial_done: bool,
    heads: HashMap<ObjectId, HeadMirror>,
    modes: HashMap<ObjectId, ModeMirror>,
    pending_configs: HashMap<usize, mpsc::Sender<ConfigResult>>,
    watchers: Vec<fmpsc::UnboundedSender<TopologyEvent>>,
}

struct State {
    shared: Arc<Mutex<Shared>>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrOutputManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrOutputManagerV1,
        event: zwlr_output_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut shared = state.shared.lock().unwrap();
        match event {
            zwlr_output_manager_v1::Event::Head { head } => {
                shared.heads.insert(
                    head.id(),
                    HeadMirror {
                        proxy: Some(head),
                        scale: 1.0,
                        ..HeadMirror::default()
                    },
                );
            }
            zwlr_output_manager_v1::Event::Done { serial } => {
                shared.serial = serial;
                let notify = shared.initial_done;
                shared.initial_done = true;
                if notify {
                    shared
                        .watchers
                        .retain(|tx| tx.unbounded_send(TopologyEvent::Changed).is_ok());
                }
            }
            zwlr_output_manager_v1::Event::Finished => {}
            _ => {}
        }
    }

    event_created_child!(State, ZwlrOutputManagerV1, [
        zwlr_output_manager_v1::EVT_HEAD_OPCODE => (ZwlrOutputHeadV1, ()),
    ]);
}

impl Dispatch<ZwlrOutputHeadV1, ()> for State {
    fn event(
        state: &mut Self,
        head: &ZwlrOutputHeadV1,
        event: zwlr_output_head_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut shared = state.shared.lock().unwrap();
        let id = head.id();
        if let zwlr_output_head_v1::Event::Finished = event {
            if let Some(h) = shared.heads.remove(&id) {
                for m in h.modes {
                    shared.modes.remove(&m);
                }
            }
            return;
        }
        let Some(mirror) = shared.heads.get_mut(&id) else {
            return;
        };
        match event {
            zwlr_output_head_v1::Event::Name { name } => mirror.name = name,
            zwlr_output_head_v1::Event::Description { description } => {
                mirror.description = description;
            }
            zwlr_output_head_v1::Event::Mode { mode } => {
                let mode_id = mode.id();
                mirror.modes.push(mode_id.clone());
                shared.modes.insert(
                    mode_id,
                    ModeMirror {
                        proxy: Some(mode),
                        ..ModeMirror::default()
                    },
                );
            }
            zwlr_output_head_v1::Event::Enabled { enabled } => {
                mirror.enabled = enabled != 0;
                if enabled == 0 {
                    mirror.current_mode = None;
                }
            }
            zwlr_output_head_v1::Event::CurrentMode { mode } => {
                mirror.current_mode = Some(mode.id());
            }
            zwlr_output_head_v1::Event::Position { x, y } => mirror.position = (x, y),
            zwlr_output_head_v1::Event::Transform {
                transform: WEnum::Value(t),
            } => {
                mirror.transform = Transform::from_u8(t as u8).unwrap_or_default();
            }
            zwlr_output_head_v1::Event::Scale { scale } => mirror.scale = scale,
            zwlr_output_head_v1::Event::Make { make } => mirror.make = make,
            zwlr_output_head_v1::Event::Model { model } => mirror.model = model,
            zwlr_output_head_v1::Event::SerialNumber { serial_number } => {
                mirror.serial_number = serial_number;
            }
            _ => {}
        }
    }

    event_created_child!(State, ZwlrOutputHeadV1, [
        zwlr_output_head_v1::EVT_MODE_OPCODE => (ZwlrOutputModeV1, ()),
    ]);
}

impl Dispatch<ZwlrOutputModeV1, ()> for State {
    fn event(
        state: &mut Self,
        mode: &ZwlrOutputModeV1,
        event: zwlr_output_mode_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut shared = state.shared.lock().unwrap();
        let id = mode.id();
        if let zwlr_output_mode_v1::Event::Finished = event {
            shared.modes.remove(&id);
            return;
        }
        let Some(mirror) = shared.modes.get_mut(&id) else {
            return;
        };
        match event {
            zwlr_output_mode_v1::Event::Size { width, height } => {
                mirror.width = width.max(0) as u32;
                mirror.height = height.max(0) as u32;
            }
            zwlr_output_mode_v1::Event::Refresh { refresh } => {
                mirror.refresh_mhz = refresh.max(0) as u32;
            }
            zwlr_output_mode_v1::Event::Preferred => mirror.preferred = true,
            _ => {}
        }
    }
}

impl Dispatch<ZwlrOutputConfigurationV1, usize> for State {
    fn event(
        state: &mut Self,
        config: &ZwlrOutputConfigurationV1,
        event: zwlr_output_configuration_v1::Event,
        token: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let result = match event {
            zwlr_output_configuration_v1::Event::Succeeded => ConfigResult::Succeeded,
            zwlr_output_configuration_v1::Event::Failed => ConfigResult::Failed,
            zwlr_output_configuration_v1::Event::Cancelled => ConfigResult::Cancelled,
            _ => return,
        };
        config.destroy();
        let mut shared = state.shared.lock().unwrap();
        if let Some(tx) = shared.pending_configs.remove(token) {
            let _ = tx.send(result);
        }
    }
}

impl Dispatch<ZwlrOutputConfigurationHeadV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwlrOutputConfigurationHeadV1,
        _: <ZwlrOutputConfigurationHeadV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

pub struct WlrootsBackend {
    connection: Connection,
    manager: ZwlrOutputManagerV1,
    qh: QueueHandle<State>,
    shared: Arc<Mutex<Shared>>,
    next_token: AtomicUsize,
}

impl WlrootsBackend {
    pub async fn new() -> Result<Self, BackendError> {
        if std::env::var_os("WAYLAND_DISPLAY").is_none() {
            return Err(BackendError::Unavailable(
                "WAYLAND_DISPLAY is not set".into(),
            ));
        }
        let connection = Connection::connect_to_env()
            .map_err(|e| BackendError::Unavailable(format!("wayland connect: {e}")))?;
        let (globals, mut queue) = registry_queue_init::<State>(&connection)
            .map_err(|e| BackendError::Unavailable(format!("wayland registry: {e}")))?;
        let qh = queue.handle();
        let manager: ZwlrOutputManagerV1 = globals.bind(&qh, 1..=4, ()).map_err(|e| {
            BackendError::Unavailable(format!(
                "compositor does not offer zwlr_output_manager_v1: {e}"
            ))
        })?;

        let shared = Arc::new(Mutex::new(Shared::default()));
        let mut state = State {
            shared: shared.clone(),
        };

        // Reader thread owns the event queue for the life of the backend.
        std::thread::Builder::new()
            .name("nosignal-wlroots-events".into())
            .spawn(move || {
                loop {
                    if queue.blocking_dispatch(&mut state).is_err() {
                        tracing::warn!("wayland connection lost");
                        return;
                    }
                }
            })
            .map_err(|e| BackendError::Server(format!("event thread: {e}")))?;

        // Wait for the initial Done.
        for _ in 0..100 {
            if shared.lock().unwrap().initial_done {
                return Ok(Self {
                    connection,
                    manager,
                    qh,
                    shared,
                    next_token: AtomicUsize::new(1),
                });
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Err(BackendError::Unavailable(
            "zwlr_output_manager_v1 sent no initial state".into(),
        ))
    }

    fn build_topology(shared: &Shared) -> Topology {
        let mut outputs: Vec<Output> = shared
            .heads
            .values()
            .map(|h| {
                let modes: Vec<Mode> = h
                    .modes
                    .iter()
                    .filter_map(|id| shared.modes.get(id))
                    .map(|m| Mode {
                        width: m.width,
                        height: m.height,
                        refresh_mhz: m.refresh_mhz,
                    })
                    .collect();
                let current = h
                    .current_mode
                    .as_ref()
                    .and_then(|id| shared.modes.get(id))
                    .map(|m| Mode {
                        width: m.width,
                        height: m.height,
                        refresh_mhz: m.refresh_mhz,
                    });
                let preferred = h
                    .modes
                    .iter()
                    .filter_map(|id| shared.modes.get(id))
                    .find(|m| m.preferred)
                    .map(|m| Mode {
                        width: m.width,
                        height: m.height,
                        refresh_mhz: m.refresh_mhz,
                    });
                let edid =
                    (!h.make.is_empty() || !h.model.is_empty() || !h.serial_number.is_empty())
                        .then(|| EdidId {
                            vendor: h.make.clone(),
                            product: h.model.clone(),
                            serial: h.serial_number.clone(),
                        });
                Output {
                    builtin: connector_is_builtin(&h.name),
                    display_name: if h.description.is_empty() {
                        h.name.clone()
                    } else {
                        h.description.clone()
                    },
                    identity: OutputIdentity::new(h.name.clone(), edid),
                    alias: None,
                    enabled: h.enabled,
                    mode: current,
                    preferred_mode: preferred,
                    modes,
                    position: h.position,
                    scale: if h.scale > 0.0 { h.scale } else { 1.0 },
                    transform: h.transform,
                    // wlroots has no primary-output concept; normalize() keeps
                    // exactly one primary for cross-backend consistency.
                    primary: false,
                }
            })
            .collect();
        outputs.sort_by(|a, b| a.identity.connector.cmp(&b.identity.connector));
        if let Some(first) = outputs.iter_mut().find(|o| o.enabled) {
            first.primary = true;
        }
        Topology {
            serial: shared.serial.to_string(),
            outputs,
        }
    }
}

fn wl_transform(t: Transform) -> wl_output::Transform {
    use wl_output::Transform as T;
    match t {
        Transform::Normal => T::Normal,
        Transform::Rot90 => T::_90,
        Transform::Rot180 => T::_180,
        Transform::Rot270 => T::_270,
        Transform::Flipped => T::Flipped,
        Transform::FlippedRot90 => T::Flipped90,
        Transform::FlippedRot180 => T::Flipped180,
        Transform::FlippedRot270 => T::Flipped270,
    }
}

#[async_trait]
impl DisplayBackend for WlrootsBackend {
    fn name(&self) -> &'static str {
        "wlroots"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            // wlroots compositors don't persist output config (kanshi's job);
            // the daemon re-assert loop provides persistence.
            native_persistence: false,
            events: true,
        }
    }

    async fn snapshot(&self) -> Result<Topology, BackendError> {
        let shared = self.shared.lock().unwrap();
        Ok(Self::build_topology(&shared))
    }

    async fn apply(&self, plan: &LayoutPlan, mode: ApplyMode) -> Result<(), BackendError> {
        let (serial, head_by_name, rx, token) = {
            let mut shared = self.shared.lock().unwrap();
            if plan.serial != shared.serial.to_string() {
                return Err(BackendError::StaleSerial);
            }
            let mut by_name: HashMap<String, (ZwlrOutputHeadV1, Vec<(ObjectId, Mode)>)> =
                HashMap::new();
            for h in shared.heads.values() {
                let Some(proxy) = &h.proxy else { continue };
                let modes = h
                    .modes
                    .iter()
                    .filter_map(|id| {
                        shared.modes.get(id).map(|m| {
                            (
                                id.clone(),
                                Mode {
                                    width: m.width,
                                    height: m.height,
                                    refresh_mhz: m.refresh_mhz,
                                },
                            )
                        })
                    })
                    .collect();
                by_name.insert(h.name.clone(), (proxy.clone(), modes));
            }
            let token = self.next_token.fetch_add(1, Ordering::SeqCst);
            let (tx, rx) = mpsc::channel();
            shared.pending_configs.insert(token, tx);
            (shared.serial, by_name, rx, token)
        };

        let config = self.manager.create_configuration(serial, &self.qh, token);
        for planned in &plan.outputs {
            let name = &planned.identity.connector;
            let Some((head, modes)) = head_by_name.get(name) else {
                config.destroy();
                self.shared.lock().unwrap().pending_configs.remove(&token);
                return Err(BackendError::UnknownOutput(name.clone()));
            };
            if !planned.enabled {
                config.disable_head(head);
                continue;
            }
            let wanted = planned.mode.ok_or_else(|| {
                BackendError::InvalidLayout(format!("enabled output {name} has no mode"))
            })?;
            let mode_id = modes
                .iter()
                .find(|(_, m)| *m == wanted)
                .or_else(|| {
                    modes
                        .iter()
                        .filter(|(_, m)| m.width == wanted.width && m.height == wanted.height)
                        .min_by_key(|(_, m)| {
                            (i64::from(m.refresh_mhz) - i64::from(wanted.refresh_mhz))
                                .unsigned_abs()
                        })
                })
                .map(|(id, _)| id.clone());
            let Some(mode_id) = mode_id else {
                config.destroy();
                self.shared.lock().unwrap().pending_configs.remove(&token);
                return Err(BackendError::InvalidLayout(format!(
                    "mode {wanted} not available on {name}"
                )));
            };
            let cfg_head = config.enable_head(head, &self.qh, ());
            if let Some(mode_mirror) = self.shared.lock().unwrap().modes.get(&mode_id)
                && let Some(proxy) = &mode_mirror.proxy
            {
                cfg_head.set_mode(proxy);
            }
            cfg_head.set_position(planned.position.0, planned.position.1);
            cfg_head.set_transform(wl_transform(planned.transform));
            cfg_head.set_scale(planned.scale);
        }

        match mode {
            ApplyMode::Verify => config.test(),
            ApplyMode::Temporary | ApplyMode::Persistent => config.apply(),
        }
        self.connection
            .flush()
            .map_err(|e| BackendError::Server(format!("wayland flush: {e}")))?;

        // The reader thread resolves the result via the config event.
        let result = tokio::task::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(10)))
            .await
            .map_err(|e| BackendError::Server(format!("join: {e}")))?;
        match result {
            Ok(ConfigResult::Succeeded) => Ok(()),
            Ok(ConfigResult::Failed) => Err(BackendError::InvalidLayout(
                "compositor rejected the configuration".into(),
            )),
            Ok(ConfigResult::Cancelled) => Err(BackendError::StaleSerial),
            Err(_) => {
                self.shared.lock().unwrap().pending_configs.remove(&token);
                Err(BackendError::Server(
                    "no response to output configuration".into(),
                ))
            }
        }
    }

    async fn watch(&self) -> Result<BoxStream<'static, TopologyEvent>, BackendError> {
        let (tx, rx) = fmpsc::unbounded();
        self.shared.lock().unwrap().watchers.push(tx);
        Ok(rx.boxed())
    }
}
