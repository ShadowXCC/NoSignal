//! Daemon-mode operations: the CLI as a thin IPC client.

use crate::ops::{self, CliError, ToggleOpts};
use nosignal_ipc::types::{ApiErrorKind, SetOpts, SetOutcome};
use nosignal_ipc::{DaemonClient, IpcError};

impl From<IpcError> for CliError {
    fn from(e: IpcError) -> Self {
        match e {
            IpcError::Api { kind, message } => match kind {
                ApiErrorKind::Ambiguous => CliError::Other2(2, message),
                ApiErrorKind::Guard => CliError::Guard(message),
                _ => CliError::Other(message),
            },
            other => CliError::Other(other.to_string()),
        }
    }
}

/// Connect to the daemon, or fall back to direct mode with a notice.
pub async fn try_connect(direct: bool) -> Option<Box<dyn DaemonClient>> {
    if direct {
        return None;
    }
    match nosignal_ipc::connect().await {
        Ok(client) => match client.get_status().await {
            Ok(_) => Some(client),
            Err(_) => None,
        },
        Err(_) => None,
    }
}

pub async fn list(client: &dyn DaemonClient, json: bool) -> Result<(), CliError> {
    let topo = client.list_outputs().await?;
    ops::print_topology(&topo, json)
}

pub async fn status(client: &dyn DaemonClient, json: bool) -> Result<(), CliError> {
    let status = client.get_status().await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&status).map_err(|e| CliError::Other(e.to_string()))?
        );
        return Ok(());
    }
    println!("daemon:   running (v{})", status.version);
    println!("backend:  {}", status.backend);
    println!(
        "outputs:  {} connected, {} enabled",
        status.outputs_total, status.outputs_enabled
    );
    match &status.active_profile {
        Some(name) => {
            let mut notes = Vec::new();
            if status.drifted {
                notes.push("drifted");
            }
            if status.suspended {
                notes.push("SUSPENDED by loop guard");
            }
            let suffix = if notes.is_empty() {
                String::new()
            } else {
                format!(" ({})", notes.join(", "))
            };
            println!("profile:  {name}{suffix}");
        }
        None => println!("profile:  none active"),
    }
    if let Some(p) = &status.pending {
        println!(
            "pending:  job {} auto-reverts in {}s — `nosignal confirm` to keep",
            p.job_id, p.deadline_secs
        );
    }
    Ok(())
}

pub async fn set_enabled(
    client: &dyn DaemonClient,
    target: &str,
    enabled: Option<bool>,
    opts: ToggleOpts,
) -> Result<(), CliError> {
    let mut set_opts = SetOpts {
        force: opts.force,
        no_timer: opts.no_timer,
        revert_secs: opts.timer,
    };

    let mut outcome = client.set_output_enabled(target, enabled, set_opts).await?;

    // Risky change not yet acknowledged: warn, prompt, retry with force.
    if let SetOutcome::GuardRefused { reason } = &outcome
        && !set_opts.force
        && reason.contains("force")
    {
        eprintln!("warning: {reason}");
        if !ops::confirm_risk().await {
            return Err(CliError::Guard("aborted".into()));
        }
        set_opts.force = true;
        outcome = client.set_output_enabled(target, enabled, set_opts).await?;
    }

    handle_outcome(client, outcome).await
}

pub async fn apply_profile(client: &dyn DaemonClient, name: &str) -> Result<(), CliError> {
    let outcome = client.apply_profile(name).await?;
    println!("profile '{name}' applied");
    handle_outcome(client, outcome).await
}

async fn handle_outcome(client: &dyn DaemonClient, outcome: SetOutcome) -> Result<(), CliError> {
    match outcome {
        SetOutcome::Applied { warnings } => {
            for w in &warnings {
                eprintln!("note: {w:?}");
            }
            println!("done");
            Ok(())
        }
        SetOutcome::AlreadyInState => {
            println!("already in the requested state");
            Ok(())
        }
        SetOutcome::Pending {
            deadline_secs,
            risk,
            ..
        } => {
            eprintln!(
                "applied temporarily ({risk:?}) — press Enter to keep, or run `nosignal confirm`"
            );
            if ops::wait_for_enter(deadline_secs).await {
                client.confirm_pending().await?;
                println!("kept");
            } else {
                println!("auto-reverted by the daemon");
            }
            Ok(())
        }
        SetOutcome::GuardRefused { reason } => Err(CliError::Guard(reason)),
    }
}
