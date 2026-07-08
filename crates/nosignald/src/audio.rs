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
