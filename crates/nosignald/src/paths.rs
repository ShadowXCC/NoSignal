//! Platform config/state directories (XDG on Linux, Known Folders on Windows).

use directories::ProjectDirs;
use std::path::PathBuf;

fn project_dirs() -> ProjectDirs {
    // Qualifier/org fields mirror nosignal_core::APP_ID (placeholder until
    // repo creation); only the app name leaks into visible paths.
    ProjectDirs::from("org", "nosignal", "nosignal").expect("no home directory")
}

/// Config dir: profiles, settings. `~/.config/nosignal/` on Linux.
pub fn config_dir() -> PathBuf {
    project_dirs().config_dir().to_path_buf()
}

/// State dir: remembered layouts, active profile, pending jobs.
/// `~/.local/state/nosignal/` on Linux, data dir on Windows.
pub fn state_dir() -> PathBuf {
    project_dirs()
        .state_dir()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| project_dirs().data_local_dir().to_path_buf())
}

pub fn profiles_path() -> PathBuf {
    config_dir().join("profiles.toml")
}

pub fn remembered_path() -> PathBuf {
    state_dir().join("remembered.toml")
}

pub fn mock_state_path() -> PathBuf {
    std::env::var_os("NOSIGNAL_MOCK_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|| state_dir().join("mock-topology.json"))
}
