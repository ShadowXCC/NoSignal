//! In-memory mock display backend used for headless testing of NoSignal.
//!
//! Implements the full [`DisplayBackend`] contract (serial checking, whole-
//! layout applies, change events) and adds test controls: simulate hotplugs,
//! external changes, and forced failures; count applies for loop-guard tests.

use async_trait::async_trait;
use futures::StreamExt;
use futures::channel::mpsc;
use futures::stream::BoxStream;
use nosignal_core::{
    ApplyMode, BackendError, Capabilities, DisplayBackend, EdidId, LayoutPlan, Mode, Output,
    OutputIdentity, Topology, TopologyEvent,
};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Fixture helpers for building test topologies.
pub mod fixtures {
    use super::*;

    pub fn mode(w: u32, h: u32, mhz: u32) -> Mode {
        Mode {
            width: w,
            height: h,
            refresh_mhz: mhz,
        }
    }

    pub fn edid(vendor: &str, product: &str, serial: &str) -> EdidId {
        EdidId {
            vendor: vendor.into(),
            product: product.into(),
            serial: serial.into(),
        }
    }

    /// A 4K@60 output with sane defaults.
    pub fn output(connector: &str, edid_id: Option<EdidId>, enabled: bool) -> Output {
        Output {
            identity: OutputIdentity::new(connector, edid_id),
            alias: None,
            display_name: format!("Mock {connector}"),
            builtin: nosignal_core::topology::connector_is_builtin(connector),
            enabled,
            mode: enabled.then(|| mode(3840, 2160, 60_000)),
            preferred_mode: Some(mode(3840, 2160, 60_000)),
            modes: vec![mode(3840, 2160, 60_000), mode(1920, 1080, 60_000)],
            position: (0, 0),
            scale: 1.0,
            transform: Default::default(),
            primary: false,
        }
    }

    /// Desktop + TV rig: DP-1 primary enabled, HDMI TV enabled to its right.
    pub fn desk_with_tv() -> Topology {
        let mut dp = output("DP-1", Some(edid("DEL", "0xa0b1", "12345")), true);
        dp.primary = true;
        let mut tv = output("HDMI-A-1", Some(edid("SAM", "0x7201", "777")), true);
        tv.alias = Some("TV".into());
        tv.position = (3840, 0);
        Topology {
            serial: "1".into(),
            outputs: vec![dp, tv],
        }
    }
}

struct Inner {
    topology: Topology,
    serial: u64,
    watchers: Vec<mpsc::UnboundedSender<TopologyEvent>>,
}

/// The mock backend. Wrap in `Arc` to share with a daemon under test; all
/// state lives behind a mutex.
pub struct MockBackend {
    inner: Mutex<Inner>,
    applies: AtomicUsize,
    fail_next_apply: AtomicBool,
    native_persistence: bool,
}

impl MockBackend {
    pub fn new(mut topology: Topology) -> Self {
        topology.serial = "1".into();
        Self {
            inner: Mutex::new(Inner {
                topology,
                serial: 1,
                watchers: Vec::new(),
            }),
            applies: AtomicUsize::new(0),
            fail_next_apply: AtomicBool::new(false),
            native_persistence: true,
        }
    }

    /// A mock that reports no native persistence (like the X11 backend).
    pub fn without_native_persistence(mut self) -> Self {
        self.native_persistence = false;
        self
    }

    /// Number of successful applies so far (for loop-guard tests).
    pub fn apply_count(&self) -> usize {
        self.applies.load(Ordering::SeqCst)
    }

    /// Make the next apply fail with a server error.
    pub fn fail_next_apply(&self) {
        self.fail_next_apply.store(true, Ordering::SeqCst);
    }

    /// Current topology (test inspection).
    pub fn topology(&self) -> Topology {
        self.inner.lock().unwrap().topology.clone()
    }

    /// Mutate the topology out-of-band (hotplug, external tool, resume) and
    /// notify watchers — the serial bumps, invalidating outstanding plans.
    pub fn simulate_change(&self, f: impl FnOnce(&mut Topology)) {
        let mut inner = self.inner.lock().unwrap();
        f(&mut inner.topology);
        bump_serial(&mut inner);
        notify(&mut inner);
    }

    /// Convenience: plug in a new output.
    pub fn simulate_connect(&self, output: Output) {
        self.simulate_change(|t| t.outputs.push(output));
    }

    /// Convenience: unplug the output with this connector.
    pub fn simulate_disconnect(&self, connector: &str) {
        self.simulate_change(|t| t.outputs.retain(|o| o.identity.connector != connector));
    }
}

fn bump_serial(inner: &mut Inner) {
    inner.serial += 1;
    inner.topology.serial = inner.serial.to_string();
}

fn notify(inner: &mut Inner) {
    inner
        .watchers
        .retain(|tx| tx.unbounded_send(TopologyEvent::Changed).is_ok());
}

#[async_trait]
impl DisplayBackend for MockBackend {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            native_persistence: self.native_persistence,
            events: true,
        }
    }

    async fn snapshot(&self) -> Result<Topology, BackendError> {
        Ok(self.inner.lock().unwrap().topology.clone())
    }

    async fn apply(&self, plan: &LayoutPlan, mode: ApplyMode) -> Result<(), BackendError> {
        if self.fail_next_apply.swap(false, Ordering::SeqCst) {
            return Err(BackendError::Server("mock: forced failure".into()));
        }

        let mut inner = self.inner.lock().unwrap();
        if plan.serial != inner.topology.serial {
            return Err(BackendError::StaleSerial);
        }
        // Whole-layout contract: every live output must be planned.
        for live in &inner.topology.outputs {
            if !plan
                .outputs
                .iter()
                .any(|p| p.identity.connector == live.identity.connector)
            {
                return Err(BackendError::InvalidLayout(format!(
                    "plan is missing output {}",
                    live.identity.connector
                )));
            }
        }
        for planned in &plan.outputs {
            if inner
                .topology
                .find_connector(&planned.identity.connector)
                .is_none()
            {
                return Err(BackendError::UnknownOutput(
                    planned.identity.connector.clone(),
                ));
            }
            if planned.enabled && planned.mode.is_none() {
                return Err(BackendError::InvalidLayout(format!(
                    "enabled output {} has no mode",
                    planned.identity.connector
                )));
            }
        }
        let enabled_primaries = plan
            .outputs
            .iter()
            .filter(|p| p.enabled && p.primary)
            .count();
        if plan.outputs.iter().any(|p| p.enabled) && enabled_primaries != 1 {
            return Err(BackendError::InvalidLayout(format!(
                "expected exactly one enabled primary, got {enabled_primaries}"
            )));
        }

        if mode == ApplyMode::Verify {
            return Ok(());
        }

        for planned in &plan.outputs {
            let out = inner
                .topology
                .outputs
                .iter_mut()
                .find(|o| o.identity.connector == planned.identity.connector)
                .expect("validated above");
            out.enabled = planned.enabled;
            out.mode = planned.enabled.then(|| planned.mode.expect("validated"));
            out.position = planned.position;
            out.scale = planned.scale;
            out.transform = planned.transform;
            out.primary = planned.enabled && planned.primary;
        }
        bump_serial(&mut inner);
        self.applies.fetch_add(1, Ordering::SeqCst);
        notify(&mut inner);
        Ok(())
    }

    async fn watch(&self) -> Result<BoxStream<'static, TopologyEvent>, BackendError> {
        let (tx, rx) = mpsc::unbounded();
        self.inner.lock().unwrap().watchers.push(tx);
        Ok(rx.boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use nosignal_core::layout::normalize;

    #[tokio::test]
    async fn disable_tv_and_restore_exact_layout() {
        let backend = MockBackend::new(desk_with_tv());

        // Disable the TV.
        let topo = backend.snapshot().await.unwrap();
        let remembered = topo.find_connector("HDMI-A-1").unwrap().clone();
        let mut plan = LayoutPlan::from_topology(&topo);
        plan.set_enabled("HDMI-A-1", false);
        normalize(&mut plan);
        backend.apply(&plan, ApplyMode::Persistent).await.unwrap();

        let now = backend.topology();
        assert!(!now.find_connector("HDMI-A-1").unwrap().enabled);
        assert_eq!(now.enabled_count(), 1);

        // Re-enable with the remembered config.
        let topo = backend.snapshot().await.unwrap();
        let mut plan = LayoutPlan::from_topology(&topo);
        {
            let tv = plan.find_connector_mut("HDMI-A-1").unwrap();
            tv.enabled = true;
            tv.mode = remembered.mode;
            tv.position = remembered.position;
        }
        normalize(&mut plan);
        backend.apply(&plan, ApplyMode::Persistent).await.unwrap();

        let now = backend.topology();
        let tv = now.find_connector("HDMI-A-1").unwrap();
        assert!(tv.enabled);
        assert_eq!(tv.position, (3840, 0));
        assert_eq!(tv.mode, remembered.mode);
    }

    #[tokio::test]
    async fn stale_serial_is_rejected() {
        let backend = MockBackend::new(desk_with_tv());
        let topo = backend.snapshot().await.unwrap();
        let mut plan = LayoutPlan::from_topology(&topo);
        plan.set_enabled("HDMI-A-1", false);
        normalize(&mut plan);

        // Out-of-band change invalidates the plan.
        backend.simulate_change(|_| {});
        let err = backend
            .apply(&plan, ApplyMode::Temporary)
            .await
            .unwrap_err();
        assert!(matches!(err, BackendError::StaleSerial));

        // Re-snapshot and retry succeeds.
        let topo = backend.snapshot().await.unwrap();
        let mut plan = LayoutPlan::from_topology(&topo);
        plan.set_enabled("HDMI-A-1", false);
        normalize(&mut plan);
        backend.apply(&plan, ApplyMode::Temporary).await.unwrap();
    }

    #[tokio::test]
    async fn events_fire_on_apply_and_hotplug() {
        let backend = MockBackend::new(desk_with_tv());
        let mut events = backend.watch().await.unwrap();

        backend.simulate_disconnect("HDMI-A-1");
        assert!(matches!(events.next().await, Some(TopologyEvent::Changed)));

        let topo = backend.snapshot().await.unwrap();
        let plan = LayoutPlan::from_topology(&topo);
        backend.apply(&plan, ApplyMode::Temporary).await.unwrap();
        assert!(matches!(events.next().await, Some(TopologyEvent::Changed)));
    }

    #[tokio::test]
    async fn verify_mode_validates_without_mutating() {
        let backend = MockBackend::new(desk_with_tv());
        let topo = backend.snapshot().await.unwrap();
        let mut plan = LayoutPlan::from_topology(&topo);
        plan.set_enabled("HDMI-A-1", false);
        normalize(&mut plan);
        backend.apply(&plan, ApplyMode::Verify).await.unwrap();
        assert!(
            backend
                .topology()
                .find_connector("HDMI-A-1")
                .unwrap()
                .enabled
        );
        assert_eq!(backend.apply_count(), 0);
    }

    #[tokio::test]
    async fn incomplete_plans_are_rejected() {
        let backend = MockBackend::new(desk_with_tv());
        let topo = backend.snapshot().await.unwrap();
        let mut plan = LayoutPlan::from_topology(&topo);
        plan.outputs.remove(1);
        let err = backend
            .apply(&plan, ApplyMode::Temporary)
            .await
            .unwrap_err();
        assert!(matches!(err, BackendError::InvalidLayout(_)));
    }
}
