//! NoSignal daemon library: backend selection and (from M2) the engine that
//! owns display state, profiles, guards, timers, and persistence.
//!
//! The `nosignald` binary is a thin wrapper around this library; the CLI's
//! direct mode reuses [`select::detect`] so terminal use works without a
//! running daemon.

pub mod audio;
pub mod config;
pub mod engine;
pub mod fullscreen;
pub mod mock_file;
pub mod paths;
pub mod rpc;
pub mod select;
pub mod state;

#[cfg(target_os = "linux")]
pub mod dbus;
#[cfg(target_os = "windows")]
pub mod pipe;
