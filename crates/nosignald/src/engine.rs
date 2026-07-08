//! The daemon engine: owns all display logic, state, guards, timers,
//! profiles, audio follow-through, and the persistence/re-assert loop.
//! IPC layers (DBus today, named pipe in M4) are thin wrappers over this.

use crate::audio::AudioController;
use crate::config::DaemonConfig;
use crate::fullscreen::FullscreenOracle;
use crate::state::{AudioRestore, DaemonState, PersistedPending};
use nosignal_core::{
    ApplyMode, BackendError, DisplayBackend, LayoutPlan, Output, PlannedOutput, ResolveError,
    Topology,
    guards::{self, RiskClass},
    identity::resolve_target,
    layout::{self, normalize},
    profile::{Profile, ProfileWarning},
    remembered::RememberedLayouts,
};
use nosignal_ipc::types::DaemonEvent;
use nosignal_ipc::types::{
    ApiErrorKind, PendingInfo, ProfileInfo, ProfilesInfo, SetOpts, SetOutcome, StatusInfo,
};
use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::sync::{Mutex, broadcast};
use tokio::time::Instant;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error(transparent)]
    Resolve(#[from] ResolveError),
    #[error(transparent)]
    Backend(#[from] BackendError),
    #[error("no profile named '{0}'")]
    NoSuchProfile(String),
    #[error("state error: {0}")]
    Store(String),
}

impl EngineError {
    pub fn kind(&self) -> ApiErrorKind {
        match self {
            EngineError::Resolve(ResolveError::NotFound(_)) => ApiErrorKind::NotFound,
            EngineError::Resolve(ResolveError::Ambiguous { .. }) => ApiErrorKind::Ambiguous,
            EngineError::Backend(_) => ApiErrorKind::Backend,
            EngineError::NoSuchProfile(_) => ApiErrorKind::NotFound,
            EngineError::Store(_) => ApiErrorKind::Other,
        }
    }
}

pub struct EnginePaths {
    pub config: PathBuf,
    pub state: PathBuf,
    pub profiles: PathBuf,
    pub remembered: PathBuf,
}

impl EnginePaths {
    pub fn default_locations() -> Self {
        Self {
            config: crate::paths::config_dir().join("config.toml"),
            state: crate::paths::state_dir().join("daemon.toml"),
            profiles: crate::paths::profiles_path(),
            remembered: crate::paths::remembered_path(),
        }
    }
}

struct Pending {
    id: u64,
    prior: Topology,
    deadline: Instant,
    risk: RiskClass,
    /// Output the job disabled (audio follow-through on confirm) or
    /// re-enabled (restore on confirm), if any.
    disabled: Option<Output>,
    enabled: Option<Output>,
    timer: Option<tokio::task::AbortHandle>,
}

struct EngineState {
    config: DaemonConfig,
    profiles: nosignal_core::profile::ProfileStore,
    remembered: RememberedLayouts,
    daemon: DaemonState,
    pending: Option<Pending>,
    drifted: bool,
    /// Timestamps of recent re-assert applies (loop guard).
    reasserts: VecDeque<Instant>,
    last_connectors: BTreeSet<String>,
}

pub struct Engine {
    backend: Arc<dyn DisplayBackend>,
    audio: Arc<dyn AudioController>,
    oracle: Arc<dyn FullscreenOracle>,
    paths: EnginePaths,
    st: Mutex<EngineState>,
    events: broadcast::Sender<DaemonEvent>,
    next_job: AtomicU64,
    /// Loop guard: max re-asserts within the window before suspending.
    loop_guard_max: usize,
    loop_guard_window: Duration,
}

impl Engine {
    pub fn new(
        backend: Arc<dyn DisplayBackend>,
        audio: Arc<dyn AudioController>,
        paths: EnginePaths,
    ) -> Arc<Self> {
        Self::with_oracle(
            backend,
            audio,
            Arc::new(crate::fullscreen::NoopOracle),
            paths,
        )
    }

    pub fn with_oracle(
        backend: Arc<dyn DisplayBackend>,
        audio: Arc<dyn AudioController>,
        oracle: Arc<dyn FullscreenOracle>,
        paths: EnginePaths,
    ) -> Arc<Self> {
        let config = DaemonConfig::load(&paths.config);
        let profiles =
            nosignal_core::profile::ProfileStore::load(&paths.profiles).unwrap_or_default();
        let remembered = RememberedLayouts::load(&paths.remembered).unwrap_or_default();
        let daemon = DaemonState::load(&paths.state);
        let (events, _) = broadcast::channel(64);
        Arc::new(Self {
            backend,
            audio,
            oracle,
            paths,
            st: Mutex::new(EngineState {
                config,
                profiles,
                remembered,
                daemon,
                pending: None,
                drifted: false,
                reasserts: VecDeque::new(),
                last_connectors: BTreeSet::new(),
            }),
            events,
            next_job: AtomicU64::new(1),
            loop_guard_max: 5,
            loop_guard_window: Duration::from_secs(30),
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<DaemonEvent> {
        self.events.subscribe()
    }

    pub fn backend_name(&self) -> &'static str {
        self.backend.name()
    }

    fn emit(&self, event: DaemonEvent) {
        let _ = self.events.send(event);
    }

    /// Snapshot with configured aliases attached.
    pub async fn snapshot(&self) -> Result<Topology, EngineError> {
        let mut topo = self.backend.snapshot().await?;
        self.st.lock().await.config.attach_aliases(&mut topo);
        Ok(topo)
    }

    /// Apply, rebasing once onto a fresh snapshot if the serial raced (E15).
    async fn apply_with_rebase(
        &self,
        plan: &LayoutPlan,
        mode: ApplyMode,
    ) -> Result<(), BackendError> {
        match self.backend.apply(plan, mode).await {
            Err(BackendError::StaleSerial) => {
                let fresh = self.backend.snapshot().await?;
                let rebased = layout::rebase_plan(plan, &fresh);
                self.backend.apply(&rebased, mode).await
            }
            other => other,
        }
    }

    // ---------------------------------------------------------- set_enabled

    pub async fn set_enabled(
        self: &Arc<Self>,
        target: &str,
        enabled: Option<bool>,
        opts: SetOpts,
    ) -> Result<SetOutcome, EngineError> {
        let topo = self.snapshot().await?;
        let output = resolve_target(target, &topo.outputs)?.clone();
        let want = enabled.unwrap_or(!output.enabled);
        if output.enabled == want {
            return Ok(SetOutcome::AlreadyInState);
        }

        // Fullscreen warn-and-defer (E11): real detection on Windows/X11,
        // documented as unavailable on Wayland (auto-revert is the net there).
        if !want && !opts.force && self.oracle.fullscreen_active() {
            return Ok(SetOutcome::GuardRefused {
                reason: "a fullscreen application appears to be running; \
                         acknowledge with force to disable anyway"
                    .into(),
            });
        }

        let mut plan = LayoutPlan::from_topology(&topo);
        if want {
            let remembered = { self.st.lock().await.remembered.clone() };
            plan_enable(&mut plan, &output, &remembered);
        } else {
            self.remember_output(&output).await;
            plan.set_enabled(&output.identity.connector, false);
        }
        normalize(&mut plan);

        let risk = guards::assess(&topo, &plan);
        let default_secs = { self.st.lock().await.config.revert_secs };
        let timer_secs = match risk {
            RiskClass::LastActiveDisplay | RiskClass::BuiltinPanel => {
                if opts.no_timer {
                    return Ok(SetOutcome::GuardRefused {
                        reason: format!(
                            "{risk:?}: the auto-revert timer is mandatory and cannot be disabled"
                        ),
                    });
                }
                if !opts.force {
                    return Ok(SetOutcome::GuardRefused {
                        reason: format!(
                            "{risk:?}: risky change requires the caller to acknowledge with force"
                        ),
                    });
                }
                Some(opts.revert_secs.unwrap_or(default_secs))
            }
            RiskClass::Routine => {
                if opts.no_timer {
                    None
                } else {
                    opts.revert_secs
                }
            }
        };

        match timer_secs {
            Some(secs) => {
                self.apply_with_rebase(&plan, ApplyMode::Temporary).await?;
                let job_id = self
                    .begin_pending(
                        topo,
                        secs,
                        risk,
                        (!want).then(|| output.clone()),
                        want.then(|| output.clone()),
                    )
                    .await;
                Ok(SetOutcome::Pending {
                    job_id,
                    deadline_secs: secs,
                    risk,
                    warnings: vec![],
                })
            }
            None => {
                self.apply_with_rebase(&plan, ApplyMode::Persistent).await?;
                if want {
                    self.audio_after_enable(&output).await;
                } else {
                    self.audio_after_disable(&output).await;
                }
                Ok(SetOutcome::Applied { warnings: vec![] })
            }
        }
    }

    /// Store the output's current config so re-enable restores it exactly.
    async fn remember_output(&self, output: &Output) {
        let mut st = self.st.lock().await;
        st.remembered.upsert(PlannedOutput::from_output(output));
        if let Err(e) = st.remembered.save(&self.paths.remembered) {
            tracing::warn!("could not save remembered layouts: {e}");
        }
    }

    // ------------------------------------------------------------- pending

    async fn begin_pending(
        self: &Arc<Self>,
        prior: Topology,
        secs: u64,
        risk: RiskClass,
        disabled: Option<Output>,
        enabled: Option<Output>,
    ) -> u64 {
        let job_id = self.next_job.fetch_add(1, Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_secs(secs);

        let engine = Arc::clone(self);
        let timer = tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            if let Err(e) = engine.resolve_pending_inner(Some(job_id), false).await {
                tracing::error!("auto-revert of job {job_id} failed: {e}");
            }
        })
        .abort_handle();

        let mut st = self.st.lock().await;
        // Only one pending job at a time; a new one force-reverts nothing —
        // callers can't start a second (we replace, aborting the old timer).
        if let Some(old) = st.pending.take()
            && let Some(t) = old.timer
        {
            t.abort();
        }
        st.pending = Some(Pending {
            id: job_id,
            prior: prior.clone(),
            deadline,
            risk,
            disabled,
            enabled,
            timer: Some(timer),
        });
        st.daemon.pending = Some(PersistedPending {
            job_id,
            deadline_epoch: unix_now() + secs,
            prior_json: serde_json::to_string(&prior).unwrap_or_default(),
        });
        let _ = st.daemon.save(&self.paths.state);
        drop(st);

        self.emit(DaemonEvent::PendingChange {
            job_id,
            seconds_remaining: secs,
        });
        job_id
    }

    pub async fn confirm_pending(self: &Arc<Self>) -> Result<bool, EngineError> {
        self.resolve_pending_inner(None, true).await
    }

    pub async fn revert_pending(self: &Arc<Self>) -> Result<bool, EngineError> {
        self.resolve_pending_inner(None, false).await
    }

    /// Resolve the pending job. `expect_id` guards the timer path against
    /// resolving a newer job than the one that timed out.
    async fn resolve_pending_inner(
        self: &Arc<Self>,
        expect_id: Option<u64>,
        keep: bool,
    ) -> Result<bool, EngineError> {
        let pending = {
            let mut st = self.st.lock().await;
            match &st.pending {
                Some(p) if expect_id.is_none() || expect_id == Some(p.id) => {
                    let p = st.pending.take().expect("checked above");
                    if let Some(t) = &p.timer {
                        t.abort();
                    }
                    st.daemon.pending = None;
                    let _ = st.daemon.save(&self.paths.state);
                    p
                }
                _ => return Ok(false),
            }
        };

        if keep {
            // Make the temporarily-applied state stick.
            let fresh = self.backend.snapshot().await?;
            let keep_plan = LayoutPlan::from_topology(&fresh);
            self.apply_with_rebase(&keep_plan, ApplyMode::Persistent)
                .await?;
            if let Some(out) = &pending.disabled {
                self.audio_after_disable(out).await;
            }
            if let Some(out) = &pending.enabled {
                self.audio_after_enable(out).await;
            }
        } else {
            let fresh = self.backend.snapshot().await?;
            let restore = layout::restore_plan(&fresh, &pending.prior);
            self.apply_with_rebase(&restore, ApplyMode::Persistent)
                .await?;
            // A reverted disable brings the output back; restore its audio.
            if let Some(out) = &pending.disabled {
                self.audio_after_enable(out).await;
            }
        }
        self.emit(DaemonEvent::PendingResolved {
            job_id: pending.id,
            kept: keep,
        });
        Ok(true)
    }

    // --------------------------------------------------------------- audio

    /// Record the default sink before the output disappears, then (async)
    /// capture what PipeWire failed over to.
    async fn audio_after_disable(self: &Arc<Self>, output: &Output) {
        let Some(current) = self.audio.default_sink() else {
            return;
        };
        {
            let mut st = self.st.lock().await;
            st.daemon
                .audio_restore
                .retain(|r| r.connector != output.identity.connector);
            st.daemon.audio_restore.push(AudioRestore {
                connector: output.identity.connector.clone(),
                edid: output.identity.edid.clone(),
                sink: current,
                fallback: None,
            });
            let _ = st.daemon.save(&self.paths.state);
        }
        // Give the audio server a moment to fail over, then record the result.
        let engine = Arc::clone(self);
        let connector = output.identity.connector.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let fallback = engine.audio.default_sink();
            let mut st = engine.st.lock().await;
            if let Some(entry) = st
                .daemon
                .audio_restore
                .iter_mut()
                .find(|r| r.connector == connector)
            {
                entry.fallback = fallback;
                let _ = st.daemon.save(&engine.paths.state);
            }
        });
    }

    /// Restore the remembered sink when its output returns, unless the user
    /// switched sinks manually while it was gone.
    async fn audio_after_enable(&self, output: &Output) {
        let entry = {
            let mut st = self.st.lock().await;
            let idx = st.daemon.audio_restore.iter().position(|r| {
                output.identity.match_quality(&r.identity())
                    >= nosignal_core::identity::MatchQuality::Connector
            });
            let entry = idx.map(|i| st.daemon.audio_restore.remove(i));
            if entry.is_some() {
                let _ = st.daemon.save(&self.paths.state);
            }
            entry
        };
        let Some(entry) = entry else { return };

        let current = self.audio.default_sink();
        let user_changed = match (&entry.fallback, &current) {
            (Some(fallback), Some(cur)) => cur != fallback && cur != &entry.sink,
            _ => false,
        };
        if user_changed {
            tracing::info!(
                "not restoring audio sink {}: user switched to {:?} meanwhile",
                entry.sink,
                current
            );
            return;
        }
        // The sink may take a moment to reappear with the output.
        for _ in 0..10 {
            if self.audio.has_sink(&entry.sink) {
                if self.audio.set_default_sink(&entry.sink) {
                    tracing::info!("restored default audio sink {}", entry.sink);
                }
                return;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        tracing::info!(
            "audio sink {} did not reappear; leaving default",
            entry.sink
        );
    }

    // ------------------------------------------------------------ profiles

    pub async fn list_profiles(&self) -> Result<ProfilesInfo, EngineError> {
        let topo = self.snapshot().await?;
        let st = self.st.lock().await;
        let active = st.daemon.active_profile.clone();
        let profiles = st
            .profiles
            .profiles
            .iter()
            .map(|p| {
                let is_active = active.as_deref() == Some(p.name.as_str());
                ProfileInfo {
                    name: p.name.clone(),
                    hotkey: p.hotkey.clone(),
                    active: is_active,
                    drifted: is_active && !p.is_satisfied_by(&topo).unwrap_or(true),
                }
            })
            .collect();
        Ok(ProfilesInfo {
            profiles,
            active,
            suspended: st.daemon.suspended,
        })
    }

    pub async fn apply_profile(self: &Arc<Self>, name: &str) -> Result<SetOutcome, EngineError> {
        let topo = self.snapshot().await?;
        let (profile, default_secs) = {
            let st = self.st.lock().await;
            (
                st.profiles
                    .find(name)
                    .cloned()
                    .ok_or_else(|| EngineError::NoSuchProfile(name.to_string()))?,
                st.config.revert_secs,
            )
        };

        let (plan, warnings) = profile.to_plan(&topo)?;

        // Remember configs of outputs this profile turns off.
        for planned in plan.outputs.iter().filter(|p| !p.enabled) {
            if let Some(live) = topo.find_connector(&planned.identity.connector)
                && live.enabled
            {
                self.remember_output(live).await;
            }
        }

        let outcome = if profile.is_satisfied_by(&topo)? {
            SetOutcome::AlreadyInState
        } else {
            let risk = guards::assess(&topo, &plan);
            match risk {
                // Unattended-safe: profile applies never prompt; the timer is
                // the safety net when a profile blanks every display.
                RiskClass::LastActiveDisplay | RiskClass::BuiltinPanel => {
                    self.apply_with_rebase(&plan, ApplyMode::Temporary).await?;
                    let job_id = self
                        .begin_pending(topo.clone(), default_secs, risk, None, None)
                        .await;
                    SetOutcome::Pending {
                        job_id,
                        deadline_secs: default_secs,
                        risk,
                        warnings: warnings.clone(),
                    }
                }
                RiskClass::Routine => {
                    self.apply_with_rebase(&plan, ApplyMode::Persistent).await?;
                    SetOutcome::Applied {
                        warnings: warnings.clone(),
                    }
                }
            }
        };

        {
            let mut st = self.st.lock().await;
            st.daemon.active_profile = Some(name.to_string());
            st.daemon.suspended = false;
            st.drifted = false;
            st.reasserts.clear();
            let _ = st.daemon.save(&self.paths.state);

            // Migrate stored connectors when EDID matched elsewhere (E7).
            for w in &warnings {
                if let ProfileWarning::ConnectorMigrated { stored, live } = w {
                    tracing::info!("profile '{name}': connector migrated {stored} -> {live}");
                }
            }
        }

        // Audio preference attached to the profile.
        if let Some(prefs) = &profile.audio
            && let Some(sink) = &prefs.preferred_sink
        {
            let engine = Arc::clone(self);
            let sink = sink.clone();
            tokio::spawn(async move {
                for _ in 0..10 {
                    if engine.audio.has_sink(&sink) {
                        engine.audio.set_default_sink(&sink);
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            });
        }

        self.emit(DaemonEvent::ProfileApplied {
            name: name.to_string(),
        });
        Ok(outcome)
    }

    /// Capture the current layout as a (new or replaced) profile.
    pub async fn save_profile(&self, name: &str) -> Result<(), EngineError> {
        let topo = self.snapshot().await?;
        let mut st = self.st.lock().await;
        let mut profile = Profile::capture(name, &topo);
        // Preserve hotkey/audio of an existing profile with this name.
        if let Some(existing) = st.profiles.find(name) {
            profile.hotkey = existing.hotkey.clone();
            profile.audio = existing.audio.clone();
        }
        st.profiles.upsert(profile);
        st.profiles
            .save(&self.paths.profiles)
            .map_err(|e| EngineError::Store(e.to_string()))?;
        if st.daemon.active_profile.as_deref() == Some(name) {
            st.drifted = false;
        }
        Ok(())
    }

    pub async fn delete_profile(&self, name: &str) -> Result<bool, EngineError> {
        let mut st = self.st.lock().await;
        let removed = st.profiles.remove(name);
        if removed {
            st.profiles
                .save(&self.paths.profiles)
                .map_err(|e| EngineError::Store(e.to_string()))?;
            if st.daemon.active_profile.as_deref() == Some(name) {
                st.daemon.active_profile = None;
                let _ = st.daemon.save(&self.paths.state);
            }
        }
        Ok(removed)
    }

    pub async fn set_alias(&self, alias: &str, target: &str) -> Result<(), EngineError> {
        let topo = self.snapshot().await?;
        let output = resolve_target(target, &topo.outputs)?.clone();
        let mut st = self.st.lock().await;
        st.config.set_alias(alias, &output.identity);
        st.config
            .save(&self.paths.config)
            .map_err(|e| EngineError::Store(e.to_string()))?;
        Ok(())
    }

    // -------------------------------------------------------------- status

    pub async fn status(&self) -> Result<StatusInfo, EngineError> {
        let topo = self.snapshot().await?;
        let st = self.st.lock().await;
        Ok(StatusInfo {
            version: env!("CARGO_PKG_VERSION").to_string(),
            backend: self.backend.name().to_string(),
            active_profile: st.daemon.active_profile.clone(),
            suspended: st.daemon.suspended,
            drifted: st.drifted,
            pending: st.pending.as_ref().map(|p| PendingInfo {
                job_id: p.id,
                deadline_secs: p
                    .deadline
                    .saturating_duration_since(Instant::now())
                    .as_secs(),
                risk: p.risk,
            }),
            outputs_total: topo.outputs.len(),
            outputs_enabled: topo.enabled_count(),
        })
    }

    // ------------------------------------------------- persistence engine

    /// Startup: recover crashed pending jobs (E12), seed the connector set,
    /// and re-assert the active profile (E3 at boot: GNOME may have
    /// reconnected everything at login).
    pub async fn startup(self: &Arc<Self>) {
        // Crash recovery.
        let persisted = { self.st.lock().await.daemon.pending.clone() };
        if let Some(p) = persisted {
            let now = unix_now();
            if p.deadline_epoch <= now {
                if let Some(prior) = p.prior() {
                    tracing::info!("reverting pending job {} from before restart", p.job_id);
                    if let Ok(fresh) = self.backend.snapshot().await {
                        let restore = layout::restore_plan(&fresh, &prior);
                        let _ = self
                            .apply_with_rebase(&restore, ApplyMode::Persistent)
                            .await;
                    }
                }
                let mut st = self.st.lock().await;
                st.daemon.pending = None;
                let _ = st.daemon.save(&self.paths.state);
            } else if let Some(prior) = p.prior() {
                let remaining = p.deadline_epoch - now;
                tracing::info!(
                    "re-arming pending job {} ({remaining}s left) from before restart",
                    p.job_id
                );
                self.begin_pending(prior, remaining, RiskClass::Routine, None, None)
                    .await;
            }
        }

        if let Ok(topo) = self.snapshot().await {
            let mut st = self.st.lock().await;
            st.last_connectors = topo
                .outputs
                .iter()
                .map(|o| o.identity.connector.clone())
                .collect();
        }
        self.reassert(true).await;
    }

    /// React to a (debounced) topology event: hotplug ⇒ re-assert the active
    /// profile; pure layout change ⇒ mark drift, never fight (E3/E4).
    pub async fn handle_topology_event(self: &Arc<Self>) {
        {
            let st = self.st.lock().await;
            if st.pending.is_some() {
                return; // our own temporary change is in flight
            }
        }
        let Ok(topo) = self.snapshot().await else {
            return;
        };
        self.emit(DaemonEvent::OutputsChanged);

        let hotplug = {
            let mut st = self.st.lock().await;
            let connectors: BTreeSet<String> = topo
                .outputs
                .iter()
                .map(|o| o.identity.connector.clone())
                .collect();
            let hotplug = connectors != st.last_connectors;
            st.last_connectors = connectors;
            hotplug
        };
        self.reassert(hotplug).await;
    }

    /// Core of the persistence engine. `hotplug` decides whether divergence
    /// is corrected (re-assert) or only flagged (drift).
    async fn reassert(self: &Arc<Self>, hotplug: bool) {
        let Ok(topo) = self.snapshot().await else {
            return;
        };
        let (profile, name) = {
            let st = self.st.lock().await;
            let Some(name) = st.daemon.active_profile.clone() else {
                return;
            };
            if st.daemon.suspended {
                return;
            }
            let Some(profile) = st.profiles.find(&name).cloned() else {
                return;
            };
            (profile, name)
        };

        match profile.is_satisfied_by(&topo) {
            Ok(true) => {
                let mut st = self.st.lock().await;
                st.drifted = false;
            }
            Ok(false) if hotplug => {
                // Loop guard (E5): never fight the compositor indefinitely.
                {
                    let mut st = self.st.lock().await;
                    let now = Instant::now();
                    while let Some(front) = st.reasserts.front()
                        && now.duration_since(*front) > self.loop_guard_window
                    {
                        st.reasserts.pop_front();
                    }
                    if st.reasserts.len() >= self.loop_guard_max {
                        tracing::warn!(
                            "loop guard: suspending profile '{name}' after {} re-asserts",
                            st.reasserts.len()
                        );
                        st.daemon.suspended = true;
                        let _ = st.daemon.save(&self.paths.state);
                        drop(st);
                        self.emit(DaemonEvent::ProfileSuspended { name });
                        return;
                    }
                    st.reasserts.push_back(now);
                }

                match profile.to_plan(&topo) {
                    Ok((plan, _warnings)) => {
                        tracing::info!("re-asserting profile '{name}' after hotplug");
                        if let Err(e) = self.apply_with_rebase(&plan, ApplyMode::Persistent).await {
                            tracing::error!("re-assert failed: {e}");
                        } else {
                            self.emit(DaemonEvent::ProfileApplied { name });
                        }
                    }
                    Err(e) => tracing::warn!("cannot re-assert '{name}': {e}"),
                }
            }
            Ok(false) => {
                let mut st = self.st.lock().await;
                if !st.drifted {
                    st.drifted = true;
                    drop(st);
                    tracing::info!("profile '{name}' drifted (external change); not fighting");
                    self.emit(DaemonEvent::ProfileDrifted { name });
                }
            }
            Err(e) => tracing::warn!("drift check for '{name}' failed: {e}"),
        }
    }

    /// Main loop: startup re-assert, then debounced topology events.
    pub async fn run(self: Arc<Self>) -> Result<(), BackendError> {
        use futures::StreamExt;
        self.startup().await;
        let mut stream = self.backend.watch().await?;
        while stream.next().await.is_some() {
            // Debounce: wait for 500 ms of quiet before reacting, so we don't
            // fight the display server mid-negotiation.
            loop {
                match tokio::time::timeout(Duration::from_millis(500), stream.next()).await {
                    Ok(Some(_)) => continue,
                    Ok(None) => return Ok(()),
                    Err(_) => break,
                }
            }
            self.handle_topology_event().await;
        }
        Ok(())
    }
}

/// Plan enabling `output`: restore remembered layout when known, otherwise
/// preferred mode at the right edge of the desktop.
fn plan_enable(plan: &mut LayoutPlan, output: &Output, remembered: &RememberedLayouts) {
    let rightmost_edge = plan
        .outputs
        .iter()
        .filter(|p| p.enabled)
        .map(|p| p.position.0 + layout::logical_width(p))
        .max()
        .unwrap_or(0);

    let planned = plan
        .find_connector_mut(&output.identity.connector)
        .expect("plan covers every live output");
    planned.enabled = true;
    match remembered.find(&output.identity) {
        Some(rem) => {
            planned.mode = rem
                .mode
                .filter(|m| output.modes.contains(m))
                .or(output.preferred_mode)
                .or_else(|| output.modes.first().copied());
            planned.position = rem.position;
            planned.scale = rem.scale;
            planned.transform = rem.transform;
            planned.primary = rem.primary;
        }
        None => {
            planned.mode = output
                .preferred_mode
                .or(output.mode)
                .or_else(|| output.modes.first().copied());
            planned.position = (rightmost_edge, 0);
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
