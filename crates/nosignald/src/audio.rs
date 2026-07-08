//! Default-audio-sink control.
//!
//! When a disabled output was the default HDMI/DP audio sink, PipeWire (or
//! Windows) fails over on its own; the daemon's job is to *restore* the
//! remembered sink when the output comes back — unless the user picked a
//! different sink in the meantime.
//!
//! v1 Linux implementation shells out to `pactl` (PipeWire's pulse shim or
//! PulseAudio proper — identical CLI). Native libpulse/pipewire bindings and
//! the Windows `IPolicyConfig` implementation are drop-in replacements behind
//! [`AudioController`].

/// Platform audio control. Implementations must be cheap and non-blocking
/// enough to call from async context (single short-lived subprocess).
pub trait AudioController: Send + Sync {
    /// Name of the current default sink.
    fn default_sink(&self) -> Option<String>;
    /// Make `sink` the default. Returns false on failure.
    fn set_default_sink(&self, sink: &str) -> bool;
    /// Whether a sink with this name currently exists.
    fn has_sink(&self, sink: &str) -> bool;
}

/// No-op controller for platforms/sessions without audio integration.
pub struct NoopAudio;

impl AudioController for NoopAudio {
    fn default_sink(&self) -> Option<String> {
        None
    }
    fn set_default_sink(&self, _sink: &str) -> bool {
        false
    }
    fn has_sink(&self, _sink: &str) -> bool {
        false
    }
}

#[cfg(target_os = "linux")]
pub use pactl::PactlAudio;

#[cfg(target_os = "linux")]
mod pactl {
    use super::AudioController;
    use std::process::Command;

    /// `pactl`-based controller (PipeWire and PulseAudio).
    pub struct PactlAudio;

    impl PactlAudio {
        /// Probe for a usable `pactl`; fall back to [`super::NoopAudio`] when
        /// absent so audio never blocks display control.
        pub fn detect() -> Option<Self> {
            let ok = Command::new("pactl")
                .arg("info")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            ok.then_some(Self)
        }
    }

    fn run(args: &[&str]) -> Option<String> {
        let out = Command::new("pactl").args(args).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    impl AudioController for PactlAudio {
        fn default_sink(&self) -> Option<String> {
            run(&["get-default-sink"]).filter(|s| !s.is_empty())
        }

        fn set_default_sink(&self, sink: &str) -> bool {
            run(&["set-default-sink", sink]).is_some()
        }

        fn has_sink(&self, sink: &str) -> bool {
            run(&["list", "short", "sinks"])
                .map(|text| {
                    text.lines()
                        .filter_map(|l| l.split_whitespace().nth(1))
                        .any(|name| name == sink)
                })
                .unwrap_or(false)
        }
    }
}

#[cfg(target_os = "windows")]
pub use win::WindowsAudio;

#[cfg(target_os = "windows")]
// COM vtable methods must keep their canonical PascalCase names.
#[allow(non_snake_case, clippy::too_many_arguments)]
mod win {
    use super::AudioController;
    use windows::Win32::Media::Audio::{
        DEVICE_STATE_ACTIVE, IMMDeviceEnumerator, MMDeviceEnumerator, eCommunications, eConsole,
        eMultimedia, eRender,
    };
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
    };
    use windows::core::{GUID, HSTRING, IUnknown, IUnknown_Vtbl, PCWSTR, interface};

    /// Undocumented but industry-standard COM interface used by every
    /// default-device switcher (SoundVolumeView, AudioSwitcher, EarTrumpet).
    /// Only `SetDefaultEndpoint` is called; earlier vtable slots are declared
    /// for layout and never used.
    #[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
    unsafe trait IPolicyConfig: IUnknown {
        unsafe fn GetMixFormat(
            &self,
            name: PCWSTR,
            fmt: *mut *mut core::ffi::c_void,
        ) -> windows::core::HRESULT;
        unsafe fn GetDeviceFormat(
            &self,
            name: PCWSTR,
            default: i32,
            fmt: *mut *mut core::ffi::c_void,
        ) -> windows::core::HRESULT;
        unsafe fn ResetDeviceFormat(&self, name: PCWSTR) -> windows::core::HRESULT;
        unsafe fn SetDeviceFormat(
            &self,
            name: PCWSTR,
            endpoint: *mut core::ffi::c_void,
            mix: *mut core::ffi::c_void,
        ) -> windows::core::HRESULT;
        unsafe fn GetProcessingPeriod(
            &self,
            name: PCWSTR,
            default: i32,
            def_period: *mut i64,
            min_period: *mut i64,
        ) -> windows::core::HRESULT;
        unsafe fn SetProcessingPeriod(
            &self,
            name: PCWSTR,
            period: *mut i64,
        ) -> windows::core::HRESULT;
        unsafe fn GetShareMode(
            &self,
            name: PCWSTR,
            mode: *mut core::ffi::c_void,
        ) -> windows::core::HRESULT;
        unsafe fn SetShareMode(
            &self,
            name: PCWSTR,
            mode: *mut core::ffi::c_void,
        ) -> windows::core::HRESULT;
        unsafe fn GetPropertyValue(
            &self,
            name: PCWSTR,
            fx_store: i32,
            key: *const core::ffi::c_void,
            value: *mut core::ffi::c_void,
        ) -> windows::core::HRESULT;
        unsafe fn SetPropertyValue(
            &self,
            name: PCWSTR,
            fx_store: i32,
            key: *const core::ffi::c_void,
            value: *mut core::ffi::c_void,
        ) -> windows::core::HRESULT;
        unsafe fn SetDefaultEndpoint(
            &self,
            name: PCWSTR,
            role: windows::Win32::Media::Audio::ERole,
        ) -> windows::core::HRESULT;
        unsafe fn SetEndpointVisibility(
            &self,
            name: PCWSTR,
            visible: i32,
        ) -> windows::core::HRESULT;
    }

    const POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

    pub struct WindowsAudio;

    impl WindowsAudio {
        pub fn detect() -> Option<Self> {
            let probe = WindowsAudio;
            probe.enumerator().map(|_| probe)
        }

        fn enumerator(&self) -> Option<IMMDeviceEnumerator> {
            unsafe {
                // S_FALSE (already initialized) is fine.
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()
            }
        }
    }

    impl AudioController for WindowsAudio {
        fn default_sink(&self) -> Option<String> {
            let enumerator = self.enumerator()?;
            unsafe {
                let device = enumerator
                    .GetDefaultAudioEndpoint(eRender, eMultimedia)
                    .ok()?;
                let id = device.GetId().ok()?;
                let text = id.to_string().ok()?;
                windows::Win32::System::Com::CoTaskMemFree(Some(id.as_ptr() as *const _));
                Some(text)
            }
        }

        fn set_default_sink(&self, sink: &str) -> bool {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                let Ok(policy): windows::core::Result<IPolicyConfig> =
                    CoCreateInstance(&POLICY_CONFIG_CLIENT, None, CLSCTX_ALL)
                else {
                    return false;
                };
                let wide = HSTRING::from(sink);
                for role in [eConsole, eMultimedia, eCommunications] {
                    if policy
                        .SetDefaultEndpoint(PCWSTR(wide.as_ptr()), role)
                        .is_err()
                    {
                        return false;
                    }
                }
                true
            }
        }

        fn has_sink(&self, sink: &str) -> bool {
            let Some(enumerator) = self.enumerator() else {
                return false;
            };
            unsafe {
                let Ok(devices) = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
                else {
                    return false;
                };
                let count = devices.GetCount().unwrap_or(0);
                for i in 0..count {
                    if let Ok(device) = devices.Item(i)
                        && let Ok(id) = device.GetId()
                    {
                        let matches = id.to_string().map(|s| s == sink).unwrap_or(false);
                        windows::Win32::System::Com::CoTaskMemFree(Some(id.as_ptr() as *const _));
                        if matches {
                            return true;
                        }
                    }
                }
                false
            }
        }
    }
}

/// Test double recording calls.
#[derive(Default)]
pub struct FakeAudio {
    pub state: std::sync::Mutex<FakeAudioState>,
}

#[derive(Default)]
pub struct FakeAudioState {
    pub default: Option<String>,
    pub sinks: Vec<String>,
    pub set_calls: Vec<String>,
}

impl AudioController for FakeAudio {
    fn default_sink(&self) -> Option<String> {
        self.state.lock().unwrap().default.clone()
    }

    fn set_default_sink(&self, sink: &str) -> bool {
        let mut st = self.state.lock().unwrap();
        st.set_calls.push(sink.to_string());
        st.default = Some(sink.to_string());
        true
    }

    fn has_sink(&self, sink: &str) -> bool {
        self.state.lock().unwrap().sinks.iter().any(|s| s == sink)
    }
}
