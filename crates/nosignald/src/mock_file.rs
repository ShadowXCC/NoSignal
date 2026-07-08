//! A mock backend whose topology persists to a JSON file across process
//! invocations. Selected with `NOSIGNAL_BACKEND=mock`; used for developing
//! and end-to-end testing the CLI/daemon on machines (and CI) where no real
//! display backend is available.

use async_trait::async_trait;
use futures::stream::BoxStream;
use nosignal_backend_mock::{MockBackend, fixtures};
use nosignal_core::{
    ApplyMode, BackendError, Capabilities, DisplayBackend, LayoutPlan, Topology, TopologyEvent,
};
use std::path::PathBuf;

pub struct FileMockBackend {
    inner: MockBackend,
    path: PathBuf,
}

impl FileMockBackend {
    /// Load the persisted topology, or seed the desk+TV fixture.
    pub fn load_or_seed(path: PathBuf) -> Self {
        let topology = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Topology>(&text).ok())
            .unwrap_or_else(fixtures::desk_with_tv);
        Self {
            inner: MockBackend::new(topology),
            path,
        }
    }

    fn persist(&self) -> Result<(), BackendError> {
        let topology = self.inner.topology();
        let text = serde_json::to_string_pretty(&topology)
            .map_err(|e| BackendError::Server(format!("mock persist: {e}")))?;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| BackendError::Server(format!("mock persist: {e}")))?;
        }
        std::fs::write(&self.path, text)
            .map_err(|e| BackendError::Server(format!("mock persist: {e}")))?;
        Ok(())
    }
}

#[async_trait]
impl DisplayBackend for FileMockBackend {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    async fn snapshot(&self) -> Result<Topology, BackendError> {
        self.inner.snapshot().await
    }

    async fn apply(&self, plan: &LayoutPlan, mode: ApplyMode) -> Result<(), BackendError> {
        self.inner.apply(plan, mode).await?;
        if mode != ApplyMode::Verify {
            self.persist()?;
        }
        Ok(())
    }

    async fn watch(&self) -> Result<BoxStream<'static, TopologyEvent>, BackendError> {
        self.inner.watch().await
    }
}
