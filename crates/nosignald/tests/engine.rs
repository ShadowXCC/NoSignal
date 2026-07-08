//! Engine behavior tests against the mock backend: the SPEC edge-case
//! register (E1, E3, E4, E5, E12 analogues) exercised headlessly.

use nosignal_backend_mock::{MockBackend, fixtures};
use nosignal_core::DisplayBackend;
use nosignal_ipc::types::{SetOpts, SetOutcome};
use nosignald::audio::FakeAudio;
use nosignald::engine::{Engine, EnginePaths};
use std::path::PathBuf;
use std::sync::Arc;

struct Rig {
    engine: Arc<Engine>,
    backend: Arc<MockBackend>,
    audio: Arc<FakeAudio>,
    _dir: PathBuf,
}

fn rig() -> Rig {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "nosignal-engine-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let backend = Arc::new(MockBackend::new(fixtures::desk_with_tv()));
    let audio = Arc::new(FakeAudio::default());
    let paths = EnginePaths {
        config: dir.join("config.toml"),
        state: dir.join("daemon.toml"),
        profiles: dir.join("profiles.toml"),
        remembered: dir.join("remembered.toml"),
    };
    let engine = Engine::new(
        backend.clone() as Arc<dyn DisplayBackend>,
        audio.clone(),
        paths,
    );
    Rig {
        engine,
        backend,
        audio,
        _dir: dir,
    }
}

fn tv_enabled(backend: &MockBackend) -> bool {
    backend
        .topology()
        .find_connector("HDMI-A-1")
        .map(|o| o.enabled)
        .unwrap_or(false)
}

#[tokio::test]
async fn routine_toggle_applies_immediately() {
    let r = rig();
    let out = r
        .engine
        .set_enabled("TV", Some(false), SetOpts::default())
        .await
        .unwrap();
    assert!(matches!(out, SetOutcome::Applied { .. }));
    assert!(!tv_enabled(&r.backend));

    // Idempotent.
    let out = r
        .engine
        .set_enabled("TV", Some(false), SetOpts::default())
        .await
        .unwrap();
    assert!(matches!(out, SetOutcome::AlreadyInState));
}

#[tokio::test]
async fn re_enable_restores_remembered_position() {
    let r = rig();
    r.engine
        .set_enabled("TV", Some(false), SetOpts::default())
        .await
        .unwrap();
    r.engine
        .set_enabled("TV", Some(true), SetOpts::default())
        .await
        .unwrap();
    let tv = r
        .backend
        .topology()
        .find_connector("HDMI-A-1")
        .cloned()
        .unwrap();
    assert!(tv.enabled);
    assert_eq!(tv.position, (3840, 0), "remembered position restored");
}

#[tokio::test(start_paused = true)]
async fn last_display_pending_auto_reverts_e1() {
    let r = rig();
    r.engine
        .set_enabled("TV", Some(false), SetOpts::default())
        .await
        .unwrap();

    // Disabling DP-1 now hits the last-display guard.
    let refused = r
        .engine
        .set_enabled("DP-1", Some(false), SetOpts::default())
        .await
        .unwrap();
    assert!(matches!(refused, SetOutcome::GuardRefused { .. }));

    let no_timer = r
        .engine
        .set_enabled(
            "DP-1",
            Some(false),
            SetOpts {
                force: true,
                no_timer: true,
                revert_secs: None,
            },
        )
        .await
        .unwrap();
    assert!(matches!(no_timer, SetOutcome::GuardRefused { .. }));

    let out = r
        .engine
        .set_enabled(
            "DP-1",
            Some(false),
            SetOpts {
                force: true,
                no_timer: false,
                revert_secs: None,
            },
        )
        .await
        .unwrap();
    let SetOutcome::Pending { deadline_secs, .. } = out else {
        panic!("expected pending, got {out:?}");
    };
    assert_eq!(deadline_secs, 20);
    assert!(!r.backend.topology().find_connector("DP-1").unwrap().enabled);

    // Nobody confirms; the timer fires and the display comes back.
    tokio::time::sleep(std::time::Duration::from_secs(21)).await;
    tokio::task::yield_now().await;
    assert!(r.backend.topology().find_connector("DP-1").unwrap().enabled);
}

#[tokio::test(start_paused = true)]
async fn confirm_pending_keeps_the_change() {
    let r = rig();
    r.engine
        .set_enabled("TV", Some(false), SetOpts::default())
        .await
        .unwrap();
    r.engine
        .set_enabled(
            "DP-1",
            Some(false),
            SetOpts {
                force: true,
                no_timer: false,
                revert_secs: Some(30),
            },
        )
        .await
        .unwrap();
    assert!(r.engine.confirm_pending().await.unwrap());
    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    tokio::task::yield_now().await;
    assert!(
        !r.backend.topology().find_connector("DP-1").unwrap().enabled,
        "confirmed change must survive past the original deadline"
    );
}

#[tokio::test]
async fn profile_reasserts_after_hotplug_e3() {
    let r = rig();
    // Save "tv-off" profile: capture with TV disabled.
    r.engine
        .set_enabled("TV", Some(false), SetOpts::default())
        .await
        .unwrap();
    r.engine.save_profile("tv-off").await.unwrap();
    r.engine.apply_profile("tv-off").await.unwrap();
    r.engine.startup().await;

    // TV power-cycled: drops off and returns enabled (as compositors do).
    let mut tv = fixtures::output(
        "HDMI-A-1",
        Some(fixtures::edid("SAM", "0x7201", "777")),
        true,
    );
    tv.position = (3840, 0);
    r.backend.simulate_disconnect("HDMI-A-1");
    r.engine.handle_topology_event().await;
    r.backend.simulate_connect(tv);
    r.engine.handle_topology_event().await;

    assert!(!tv_enabled(&r.backend), "profile re-asserted: TV stays off");
}

#[tokio::test]
async fn external_change_marks_drift_without_fighting_e4() {
    let r = rig();
    r.engine
        .set_enabled("TV", Some(false), SetOpts::default())
        .await
        .unwrap();
    r.engine.save_profile("tv-off").await.unwrap();
    r.engine.apply_profile("tv-off").await.unwrap();
    r.engine.startup().await;
    let applies_before = r.backend.apply_count();

    // User re-enables the TV in GNOME Settings: same connector set, layout
    // change only.
    r.backend.simulate_change(|t| {
        let tv = t
            .outputs
            .iter_mut()
            .find(|o| o.identity.connector == "HDMI-A-1")
            .unwrap();
        tv.enabled = true;
        tv.mode = Some(fixtures::mode(3840, 2160, 60_000));
    });
    r.engine.handle_topology_event().await;

    assert!(tv_enabled(&r.backend), "daemon must not fight the user");
    assert_eq!(r.backend.apply_count(), applies_before);
    let status = r.engine.status().await.unwrap();
    assert!(status.drifted);
}

#[tokio::test]
async fn loop_guard_suspends_profile_e5() {
    let r = rig();
    r.engine
        .set_enabled("TV", Some(false), SetOpts::default())
        .await
        .unwrap();
    r.engine.save_profile("tv-off").await.unwrap();
    r.engine.apply_profile("tv-off").await.unwrap();
    r.engine.startup().await;

    // A hostile compositor re-enables the TV on every hotplug, forever.
    for _ in 0..8 {
        let mut tv = fixtures::output(
            "HDMI-A-1",
            Some(fixtures::edid("SAM", "0x7201", "777")),
            true,
        );
        tv.position = (3840, 0);
        r.backend.simulate_disconnect("HDMI-A-1");
        r.engine.handle_topology_event().await;
        r.backend.simulate_connect(tv);
        r.engine.handle_topology_event().await;
    }

    let status = r.engine.status().await.unwrap();
    assert!(
        status.suspended,
        "loop guard must suspend, not fight forever"
    );

    // Explicit re-apply clears the suspension.
    r.engine.apply_profile("tv-off").await.unwrap();
    let status = r.engine.status().await.unwrap();
    assert!(!status.suspended);
}

#[tokio::test(start_paused = true)]
async fn audio_sink_recorded_and_restored_e10() {
    let r = rig();
    {
        let mut audio = r.audio.state.lock().unwrap();
        audio.default = Some("hdmi-sink".into());
        audio.sinks = vec!["hdmi-sink".into(), "analog".into()];
    }

    r.engine
        .set_enabled("TV", Some(false), SetOpts::default())
        .await
        .unwrap();
    // PipeWire fails over on its own.
    r.audio.state.lock().unwrap().default = Some("analog".into());
    tokio::time::sleep(std::time::Duration::from_secs(3)).await; // fallback capture task
    tokio::task::yield_now().await;

    r.engine
        .set_enabled("TV", Some(true), SetOpts::default())
        .await
        .unwrap();
    let calls = r.audio.state.lock().unwrap().set_calls.clone();
    assert_eq!(calls, vec!["hdmi-sink".to_string()], "sink restored");
}

#[tokio::test(start_paused = true)]
async fn audio_not_restored_when_user_switched_meanwhile() {
    let r = rig();
    {
        let mut audio = r.audio.state.lock().unwrap();
        audio.default = Some("hdmi-sink".into());
        audio.sinks = vec!["hdmi-sink".into(), "analog".into(), "usb-dac".into()];
    }

    r.engine
        .set_enabled("TV", Some(false), SetOpts::default())
        .await
        .unwrap();
    r.audio.state.lock().unwrap().default = Some("analog".into());
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    tokio::task::yield_now().await;

    // User deliberately picks a different sink while the TV is off.
    r.audio.state.lock().unwrap().default = Some("usb-dac".into());

    r.engine
        .set_enabled("TV", Some(true), SetOpts::default())
        .await
        .unwrap();
    let calls = r.audio.state.lock().unwrap().set_calls.clone();
    assert!(calls.is_empty(), "user's choice must be respected");
}

#[tokio::test]
async fn profiles_crud_and_active_persistence() {
    let r = rig();
    r.engine.save_profile("desk").await.unwrap();
    r.engine
        .set_enabled("TV", Some(false), SetOpts::default())
        .await
        .unwrap();
    r.engine.save_profile("movie-off").await.unwrap();

    let info = r.engine.list_profiles().await.unwrap();
    assert_eq!(info.profiles.len(), 2);
    assert_eq!(info.active, None);

    r.engine.apply_profile("desk").await.unwrap();
    assert!(tv_enabled(&r.backend), "desk profile re-enables the TV");
    let info = r.engine.list_profiles().await.unwrap();
    assert_eq!(info.active.as_deref(), Some("desk"));

    assert!(r.engine.delete_profile("movie-off").await.unwrap());
    assert!(!r.engine.delete_profile("movie-off").await.unwrap());
    let info = r.engine.list_profiles().await.unwrap();
    assert_eq!(info.profiles.len(), 1);
}

#[tokio::test]
async fn alias_attaches_to_snapshots() {
    let r = rig();
    r.engine.set_alias("cinema", "HDMI-A-1").await.unwrap();
    let topo = r.engine.snapshot().await.unwrap();
    let tv = topo.find_connector("HDMI-A-1").unwrap();
    assert_eq!(tv.alias.as_deref(), Some("cinema"));

    // And resolves as a target.
    let out = r
        .engine
        .set_enabled("cinema", Some(false), SetOpts::default())
        .await
        .unwrap();
    assert!(matches!(out, SetOutcome::Applied { .. }));
}
