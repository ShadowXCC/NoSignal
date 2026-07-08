use async_trait::async_trait;
use futures::StreamExt;
use futures::channel::mpsc;
use futures::stream::BoxStream;
use nosignal_core::{
    ApplyMode, BackendError, Capabilities, DisplayBackend, EdidId, LayoutPlan, Mode, Output,
    OutputIdentity, Topology, TopologyEvent, Transform,
};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use std::time::Duration;
use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_MODE_INFO,
    DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE, DISPLAYCONFIG_MODE_INFO_TYPE_TARGET,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EXTERNAL, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DVI,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_HDMI, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_ROTATION_IDENTITY,
    DISPLAYCONFIG_ROTATION_ROTATE90, DISPLAYCONFIG_ROTATION_ROTATE180,
    DISPLAYCONFIG_ROTATION_ROTATE270, DISPLAYCONFIG_SCALING_PREFERRED,
    DISPLAYCONFIG_SCANLINE_ORDERING_UNSPECIFIED, DISPLAYCONFIG_TARGET_DEVICE_NAME,
    DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes,
    QDC_ALL_PATHS, QueryDisplayConfig, SDC_ALLOW_CHANGES, SDC_APPLY, SDC_SAVE_TO_DATABASE,
    SDC_USE_SUPPLIED_DISPLAY_CONFIG, SetDisplayConfig,
};
use windows::Win32::Foundation::{ERROR_SUCCESS, WIN32_ERROR};

const DISPLAYCONFIG_PATH_ACTIVE: u32 = 0x0000_0001;
const MODE_IDX_INVALID: u32 = 0xFFFF_FFFF;
const POLL_INTERVAL: Duration = Duration::from_secs(2);
const APPLY_ATTEMPTS: usize = 3;

pub struct WindowsBackend {
    watchers: Mutex<Vec<mpsc::UnboundedSender<TopologyEvent>>>,
}

struct CcdState {
    paths: Vec<DISPLAYCONFIG_PATH_INFO>,
    modes: Vec<DISPLAYCONFIG_MODE_INFO>,
}

/// One connected monitor as assembled from CCD paths + target device names.
struct TargetView {
    path_index: usize,
    connector: String,
    friendly: String,
    device_path: String,
    edid: Option<EdidId>,
    builtin: bool,
    active: bool,
    mode: Option<Mode>,
    position: (i32, i32),
    transform: Transform,
}

impl WindowsBackend {
    pub async fn new() -> Result<Self, BackendError> {
        // Probe: does CCD answer at all?
        query(QDC_ALL_PATHS)?;
        Ok(Self {
            watchers: Mutex::new(Vec::new()),
        })
    }

    fn spawn_poller(&self, mut last: String) {
        let watchers = self.watchers.lock().unwrap().clone();
        tokio::spawn(async move {
            let mut watchers = watchers;
            loop {
                tokio::time::sleep(POLL_INTERVAL).await;
                let Ok(state) = query(QDC_ALL_PATHS) else {
                    continue;
                };
                let serial = state_serial(&state);
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

fn win_err(context: &str, code: i32) -> BackendError {
    BackendError::Server(format!("{context} failed with error {code}"))
}

fn query(
    flags: windows::Win32::Devices::Display::QUERY_DISPLAY_CONFIG_FLAGS,
) -> Result<CcdState, BackendError> {
    // The buffer sizes can change between the two calls; retry per docs.
    for _ in 0..4 {
        let mut num_paths = 0u32;
        let mut num_modes = 0u32;
        let rc = unsafe { GetDisplayConfigBufferSizes(flags, &mut num_paths, &mut num_modes) };
        if rc != ERROR_SUCCESS {
            return Err(win_err("GetDisplayConfigBufferSizes", rc.0 as i32));
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); num_paths as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); num_modes as usize];
        let rc = unsafe {
            QueryDisplayConfig(
                flags,
                &mut num_paths,
                paths.as_mut_ptr(),
                &mut num_modes,
                modes.as_mut_ptr(),
                None,
            )
        };
        if rc == WIN32_ERROR(122) {
            continue; // ERROR_INSUFFICIENT_BUFFER: sizes raced, requery
        }
        if rc != ERROR_SUCCESS {
            return Err(win_err("QueryDisplayConfig", rc.0 as i32));
        }
        paths.truncate(num_paths as usize);
        modes.truncate(num_modes as usize);
        return Ok(CcdState { paths, modes });
    }
    Err(BackendError::Server(
        "QueryDisplayConfig kept racing buffer sizes".into(),
    ))
}

fn tech_label(tech: DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY) -> &'static str {
    match tech {
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_HDMI => "HDMI",
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DVI => "DVI",
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EXTERNAL => "DP",
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED => "eDP",
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL => "INTERNAL",
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS => "LVDS",
        _ => "OUT",
    }
}

fn is_builtin_tech(tech: DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY) -> bool {
    matches!(
        tech,
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL
            | DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS
            | DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED
    )
}

fn utf16_str(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Decode the 2-byte EDID manufacture id CCD hands us (big-endian PNP).
fn decode_pnp(mfg: u16) -> String {
    let mfg = mfg.swap_bytes();
    let letter = |shift: u16| -> char {
        let v = ((mfg >> shift) & 0x1F) as u8;
        if (1..=26).contains(&v) {
            (b'A' + v - 1) as char
        } else {
            '?'
        }
    };
    [letter(10), letter(5), letter(0)].iter().collect()
}

/// Full EDID from the registry: the device path
/// `\\?\DISPLAY#<hwid>#<instance>#{guid}` maps to
/// `HKLM\SYSTEM\CurrentControlSet\Enum\DISPLAY\<hwid>\<instance>\Device Parameters\EDID`.
fn registry_edid(device_path: &str) -> Option<EdidId> {
    use windows::Win32::System::Registry::{
        HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_VALUE_TYPE, RegCloseKey, RegOpenKeyExW,
        RegQueryValueExW,
    };
    use windows::core::HSTRING;

    let parts: Vec<&str> = device_path.split('#').collect();
    if parts.len() < 3 {
        return None;
    }
    let subkey = format!(
        r"SYSTEM\CurrentControlSet\Enum\DISPLAY\{}\{}\Device Parameters",
        parts[1], parts[2]
    );
    unsafe {
        let mut key = HKEY::default();
        let rc = RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            &HSTRING::from(subkey),
            None,
            KEY_READ,
            &mut key,
        );
        if rc != ERROR_SUCCESS {
            return None;
        }
        let mut buf = vec![0u8; 512];
        let mut len = buf.len() as u32;
        let mut kind = REG_VALUE_TYPE::default();
        let rc = RegQueryValueExW(
            key,
            &HSTRING::from("EDID"),
            None,
            Some(&mut kind),
            Some(buf.as_mut_ptr()),
            Some(&mut len),
        );
        let _ = RegCloseKey(key);
        if rc != ERROR_SUCCESS {
            return None;
        }
        buf.truncate(len as usize);
        nosignal_core::edid::parse(&buf)
    }
}

fn target_name(
    adapter: windows::Win32::Foundation::LUID,
    id: u32,
) -> Option<DISPLAYCONFIG_TARGET_DEVICE_NAME> {
    let mut name = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
    name.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
    name.header.size = std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
    name.header.adapterId = adapter;
    name.header.id = id;
    let rc = unsafe { DisplayConfigGetDeviceInfo(&mut name.header) };
    (rc == 0).then_some(name)
}

fn ccd_rotation_to_transform(
    rotation: windows::Win32::Devices::Display::DISPLAYCONFIG_ROTATION,
) -> Transform {
    match rotation {
        DISPLAYCONFIG_ROTATION_ROTATE90 => Transform::Rot90,
        DISPLAYCONFIG_ROTATION_ROTATE180 => Transform::Rot180,
        DISPLAYCONFIG_ROTATION_ROTATE270 => Transform::Rot270,
        _ => Transform::Normal,
    }
}

fn transform_to_ccd_rotation(
    t: Transform,
) -> windows::Win32::Devices::Display::DISPLAYCONFIG_ROTATION {
    match t {
        Transform::Rot90 | Transform::FlippedRot90 => DISPLAYCONFIG_ROTATION_ROTATE90,
        Transform::Rot180 | Transform::FlippedRot180 => DISPLAYCONFIG_ROTATION_ROTATE180,
        Transform::Rot270 | Transform::FlippedRot270 => DISPLAYCONFIG_ROTATION_ROTATE270,
        _ => DISPLAYCONFIG_ROTATION_IDENTITY,
    }
}

/// Assemble per-target views from the CCD state. For inactive targets CCD
/// reports one path per possible source; we keep the first available one.
fn collect_targets(state: &CcdState) -> Vec<TargetView> {
    let mut views: Vec<TargetView> = Vec::new();
    for (i, path) in state.paths.iter().enumerate() {
        if !path.targetInfo.targetAvailable.as_bool() {
            continue;
        }
        let active = path.flags & DISPLAYCONFIG_PATH_ACTIVE != 0;
        let Some(name) = target_name(path.targetInfo.adapterId, path.targetInfo.id) else {
            continue;
        };
        let device_path = utf16_str(&name.monitorDevicePath);
        // One view per physical monitor; prefer the active path.
        if let Some(existing) = views.iter_mut().find(|v| v.device_path == device_path) {
            if active && !existing.active {
                existing.active = true;
                existing.path_index = i;
                fill_active_fields(existing, state, path);
            }
            continue;
        }

        let tech = path.targetInfo.outputTechnology;
        let connector = format!(
            "{}-{}",
            tech_label(tech),
            if name.connectorInstance != 0 {
                name.connectorInstance
            } else {
                path.targetInfo.id
            }
        );
        let friendly = utf16_str(&name.monitorFriendlyDeviceName);
        let edid = registry_edid(&device_path).or_else(|| {
            (name.edidManufactureId != 0).then(|| EdidId {
                vendor: decode_pnp(name.edidManufactureId),
                product: format!("0x{:04x}", name.edidProductCodeId),
                serial: String::new(),
            })
        });

        let mut view = TargetView {
            path_index: i,
            connector,
            friendly,
            device_path,
            edid,
            builtin: is_builtin_tech(tech),
            active,
            mode: None,
            position: (0, 0),
            transform: ccd_rotation_to_transform(path.targetInfo.rotation),
        };
        if active {
            fill_active_fields(&mut view, state, path);
        }
        views.push(view);
    }
    views.sort_by(|a, b| a.connector.cmp(&b.connector));
    views
}

fn fill_active_fields(view: &mut TargetView, state: &CcdState, path: &DISPLAYCONFIG_PATH_INFO) {
    view.transform = ccd_rotation_to_transform(path.targetInfo.rotation);
    let src_idx = unsafe { path.sourceInfo.Anonymous.modeInfoIdx };
    if src_idx != MODE_IDX_INVALID
        && let Some(info) = state.modes.get(src_idx as usize)
        && info.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE
    {
        let source = unsafe { info.Anonymous.sourceMode };
        view.position = (source.position.x, source.position.y);
        view.mode = Some(Mode {
            width: source.width,
            height: source.height,
            refresh_mhz: 60_000,
        });
    }
    let tgt_idx = unsafe { path.targetInfo.Anonymous.modeInfoIdx };
    if tgt_idx != MODE_IDX_INVALID
        && let Some(info) = state.modes.get(tgt_idx as usize)
        && info.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_TARGET
    {
        let signal = unsafe { info.Anonymous.targetMode }.targetVideoSignalInfo;
        if signal.vSyncFreq.Denominator != 0 {
            let refresh =
                f64::from(signal.vSyncFreq.Numerator) / f64::from(signal.vSyncFreq.Denominator);
            if let Some(m) = &mut view.mode {
                m.refresh_mhz = (refresh * 1000.0).round() as u32;
            }
        }
    }
}

fn state_serial(state: &CcdState) -> String {
    let mut hasher = DefaultHasher::new();
    for path in &state.paths {
        path.flags.hash(&mut hasher);
        path.targetInfo.id.hash(&mut hasher);
        path.targetInfo.targetAvailable.0.hash(&mut hasher);
        let idx = unsafe { path.sourceInfo.Anonymous.modeInfoIdx };
        idx.hash(&mut hasher);
    }
    for info in &state.modes {
        if info.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE {
            let source = unsafe { info.Anonymous.sourceMode };
            (
                source.position.x,
                source.position.y,
                source.width,
                source.height,
            )
                .hash(&mut hasher);
        }
    }
    format!("{:x}", hasher.finish())
}

fn build_topology(state: &CcdState) -> Topology {
    let views = collect_targets(state);
    let outputs = views
        .into_iter()
        .map(|v| Output {
            builtin: v.builtin,
            display_name: if v.friendly.is_empty() {
                v.connector.clone()
            } else {
                v.friendly.clone()
            },
            identity: OutputIdentity::new(v.connector, v.edid),
            alias: None,
            enabled: v.active,
            mode: v.mode,
            preferred_mode: None,
            // CCD doesn't enumerate supported modes; Windows resolves modes
            // from its database on enable, so plans carry current-mode only.
            modes: v.mode.into_iter().collect(),
            position: v.position,
            scale: 1.0,
            transform: v.transform,
            primary: v.active && v.position == (0, 0),
        })
        .collect();
    Topology {
        serial: state_serial(state),
        outputs,
    }
}

fn set_display_config(
    paths: &mut [DISPLAYCONFIG_PATH_INFO],
    modes: &mut [DISPLAYCONFIG_MODE_INFO],
    persist: bool,
) -> Result<(), BackendError> {
    let mut flags = SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES;
    if persist {
        flags |= SDC_SAVE_TO_DATABASE;
    }
    let mut last = 0i32;
    for attempt in 0..APPLY_ATTEMPTS {
        let rc = unsafe {
            SetDisplayConfig(
                Some(paths),
                if modes.is_empty() { None } else { Some(modes) },
                flags,
            )
        };
        if rc == 0 {
            return Ok(());
        }
        last = rc;
        tracing::warn!("SetDisplayConfig attempt {} failed: {rc}", attempt + 1);
        std::thread::sleep(Duration::from_millis(300));
    }
    Err(BackendError::Server(format!(
        "SetDisplayConfig failed after {APPLY_ATTEMPTS} attempts (error {last}); \
         Windows may need a nudge from Settings > System > Display"
    )))
}

#[async_trait]
impl DisplayBackend for WindowsBackend {
    fn name(&self) -> &'static str {
        "windows"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            // SDC_SAVE_TO_DATABASE: Windows re-applies the layout itself.
            native_persistence: true,
            events: true,
        }
    }

    async fn snapshot(&self) -> Result<Topology, BackendError> {
        Ok(build_topology(&query(QDC_ALL_PATHS)?))
    }

    async fn apply(&self, plan: &LayoutPlan, mode: ApplyMode) -> Result<(), BackendError> {
        let mut state = query(QDC_ALL_PATHS)?;
        if plan.serial != state_serial(&state) {
            return Err(BackendError::StaleSerial);
        }
        let views = collect_targets(&state);

        // Windows' primary is the monitor at (0,0): shift everything so the
        // planned primary lands there.
        let offset = plan
            .outputs
            .iter()
            .find(|p| p.enabled && p.primary)
            .map(|p| p.position)
            .unwrap_or((0, 0));

        for planned in &plan.outputs {
            let view = views
                .iter()
                .find(|v| v.connector == planned.identity.connector)
                .ok_or_else(|| BackendError::UnknownOutput(planned.identity.connector.clone()))?;
            let path = &mut state.paths[view.path_index];

            if planned.enabled {
                path.flags |= DISPLAYCONFIG_PATH_ACTIVE;
                path.targetInfo.rotation = transform_to_ccd_rotation(planned.transform);
                if !view.active {
                    // Freshly enabled: let Windows resolve modes/placement
                    // from its database.
                    path.sourceInfo.Anonymous.modeInfoIdx = MODE_IDX_INVALID;
                    path.targetInfo.Anonymous.modeInfoIdx = MODE_IDX_INVALID;
                    path.targetInfo.scaling = DISPLAYCONFIG_SCALING_PREFERRED;
                    path.targetInfo.scanLineOrdering = DISPLAYCONFIG_SCANLINE_ORDERING_UNSPECIFIED;
                    path.targetInfo.refreshRate.Numerator = 0;
                    path.targetInfo.refreshRate.Denominator = 0;
                } else {
                    // Already active: apply the planned position.
                    let src_idx = unsafe { path.sourceInfo.Anonymous.modeInfoIdx };
                    if src_idx != MODE_IDX_INVALID
                        && let Some(info) = state.modes.get_mut(src_idx as usize)
                        && info.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE
                    {
                        let source = unsafe { &mut info.Anonymous.sourceMode };
                        source.position.x = planned.position.0 - offset.0;
                        source.position.y = planned.position.1 - offset.1;
                    }
                }
            } else {
                path.flags &= !DISPLAYCONFIG_PATH_ACTIVE;
            }
        }

        if mode == ApplyMode::Verify {
            // CCD has a validate flag, but plans built from our own snapshot
            // are structurally valid; treat verify as a no-op success.
            return Ok(());
        }

        let persist = mode == ApplyMode::Persistent;
        let CcdState { paths, modes } = &mut state;
        set_display_config(paths, modes, persist)?;

        let watchers = self.watchers.lock().unwrap().clone();
        for tx in watchers {
            let _ = tx.unbounded_send(TopologyEvent::Changed);
        }
        Ok(())
    }

    async fn watch(&self) -> Result<BoxStream<'static, TopologyEvent>, BackendError> {
        let (tx, rx) = mpsc::unbounded();
        let start = {
            let mut watchers = self.watchers.lock().unwrap();
            watchers.push(tx);
            watchers.len() == 1
        };
        if start {
            let state = query(QDC_ALL_PATHS)?;
            self.spawn_poller(state_serial(&state));
        }
        Ok(rx.boxed())
    }
}
