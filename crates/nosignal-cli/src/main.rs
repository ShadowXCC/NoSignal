//! `nosignal` — command-line client.
//!
//! Talks to the daemon over IPC when it is running (or DBus-activatable);
//! falls back to driving the display backend directly — `--direct` forces
//! that (daemon-less scripting, SSH rescue).
//!
//! Exit codes: 0 ok · 1 error · 2 ambiguous target · 3 guarded action refused.

mod client;
mod ops;

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "nosignal",
    version,
    about = "Software unplug for displays: disable outputs as if the cable were pulled"
)]
struct Cli {
    /// Bypass the daemon and drive the display backend directly
    #[arg(long, global = true)]
    direct: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Clone, Copy)]
struct ToggleFlags {
    /// Proceed without interactive confirmation of risk warnings
    #[arg(long)]
    force: bool,
    /// Skip the auto-revert timer (refused when disabling the last active
    /// display or a built-in panel)
    #[arg(long)]
    no_timer: bool,
    /// Use an auto-revert timer of this many seconds (default 20 when a
    /// timer is mandatory)
    #[arg(long, value_name = "SECS")]
    timer: Option<u64>,
}

#[derive(Subcommand)]
enum Cmd {
    /// List outputs: alias, connector, EDID identity, state
    List {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Disable an output (TARGET = alias | connector | EDID substring)
    Off {
        target: String,
        #[command(flatten)]
        flags: ToggleFlags,
    },
    /// Re-enable an output, restoring its remembered layout
    On { target: String },
    /// Toggle an output
    Toggle {
        target: String,
        #[command(flatten)]
        flags: ToggleFlags,
    },
    /// Confirm the pending change (keep it)
    Confirm,
    /// Revert the pending change now
    Revert,
    /// Manage saved layouts
    Profile {
        #[command(subcommand)]
        cmd: ProfileCmd,
    },
    /// Name an output (e.g. `nosignal alias TV HDMI-A-1`)
    Alias { name: String, target: String },
    /// Show daemon/backend status
    Status {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
    /// Hotkey helpers
    Hotkeys {
        #[command(subcommand)]
        cmd: HotkeysCmd,
    },
    /// DDC/CI instant-standby (opt-in, per output)
    Ddc {
        #[command(subcommand)]
        cmd: DdcCmd,
    },
    /// Control the daemon process
    Daemon {
        #[command(subcommand)]
        cmd: DaemonCmd,
    },
}

#[derive(Subcommand)]
enum ProfileCmd {
    /// List saved profiles
    List,
    /// Apply a profile (sets it active; the daemon keeps re-asserting it)
    Apply { name: String },
    /// Capture the current layout as a profile
    Save { name: String },
    /// Delete a profile
    Delete { name: String },
}

#[derive(Subcommand)]
enum HotkeysCmd {
    /// Write GNOME custom keybindings that invoke this CLI (portal-free
    /// fallback; the daemon binds hotkeys via the desktop portal otherwise)
    Install,
}

#[derive(Subcommand)]
enum DdcCmd {
    /// Test whether the monitor answers DDC/CI power commands
    Probe { target: String },
    /// Send DDC/CI standby when this output is disabled
    OptIn { target: String },
    /// Stop sending DDC/CI standby for this output
    OptOut { target: String },
}

#[derive(Subcommand)]
enum DaemonCmd {
    /// Show daemon status
    Status,
    /// Start the daemon (systemd unit if installed, else spawn)
    Start,
    /// Ask the daemon to exit
    Stop,
    /// Show recent daemon logs (journalctl)
    Logs,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let result = run(cli).await;
    if let Err(err) = result {
        eprintln!("error: {err}");
        if let Some(hint) = err.hint() {
            eprintln!("hint: {hint}");
        }
        std::process::exit(err.exit_code());
    }
}

async fn run(cli: Cli) -> Result<(), ops::CliError> {
    // Daemon-control commands manage the connection themselves.
    if let Cmd::Daemon { cmd } = &cli.cmd {
        return daemon_cmd(cmd).await;
    }

    let daemon = client::try_connect(cli.direct).await;
    if daemon.is_none() && !cli.direct {
        eprintln!("note: daemon not reachable — operating directly on the display backend");
    }

    match (cli.cmd, daemon) {
        (Cmd::List { json }, Some(c)) => client::list(c.as_ref(), json).await,
        (Cmd::List { json }, None) => ops::list(json).await,

        (Cmd::Off { target, flags }, Some(c)) => {
            client::set_enabled(c.as_ref(), &target, Some(false), flags.into()).await
        }
        (Cmd::Off { target, flags }, None) => {
            ops::set_enabled(&target, Some(false), flags.into()).await
        }

        (Cmd::On { target }, Some(c)) => {
            client::set_enabled(c.as_ref(), &target, Some(true), on_flags()).await
        }
        (Cmd::On { target }, None) => ops::set_enabled(&target, Some(true), on_flags()).await,

        (Cmd::Toggle { target, flags }, Some(c)) => {
            client::set_enabled(c.as_ref(), &target, None, flags.into()).await
        }
        (Cmd::Toggle { target, flags }, None) => {
            ops::set_enabled(&target, None, flags.into()).await
        }

        (Cmd::Confirm, Some(c)) => {
            if c.confirm_pending().await.map_err(ops::CliError::from)? {
                println!("kept");
            } else {
                println!("nothing pending");
            }
            Ok(())
        }
        (Cmd::Confirm, None) | (Cmd::Revert, None) => Err(ops::CliError::Other(
            "pending changes live in the daemon; it is not running".into(),
        )),
        (Cmd::Revert, Some(c)) => {
            if c.revert_pending().await.map_err(ops::CliError::from)? {
                println!("reverted");
            } else {
                println!("nothing pending");
            }
            Ok(())
        }

        (Cmd::Profile { cmd }, Some(c)) => match cmd {
            ProfileCmd::List => {
                let info = c.list_profiles().await.map_err(ops::CliError::from)?;
                if info.profiles.is_empty() {
                    println!("no profiles saved");
                }
                for p in &info.profiles {
                    let mut notes = Vec::new();
                    if p.active {
                        notes.push("active".to_string());
                    }
                    if p.drifted {
                        notes.push("drifted".to_string());
                    }
                    if let Some(h) = &p.hotkey {
                        notes.push(format!("hotkey: {h}"));
                    }
                    if notes.is_empty() {
                        println!("{}", p.name);
                    } else {
                        println!("{} ({})", p.name, notes.join(", "));
                    }
                }
                if info.suspended {
                    eprintln!(
                        "warning: active profile suspended by the loop guard; \
                         re-apply it to resume"
                    );
                }
                Ok(())
            }
            ProfileCmd::Apply { name } => client::apply_profile(c.as_ref(), &name).await,
            ProfileCmd::Save { name } => {
                c.save_profile(&name).await.map_err(ops::CliError::from)?;
                println!("profile '{name}' saved");
                Ok(())
            }
            ProfileCmd::Delete { name } => {
                if c.delete_profile(&name).await.map_err(ops::CliError::from)? {
                    println!("profile '{name}' deleted");
                    Ok(())
                } else {
                    Err(ops::CliError::Other(format!("no profile named '{name}'")))
                }
            }
        },
        (Cmd::Profile { cmd }, None) => match cmd {
            ProfileCmd::List => ops::profile::list().await,
            ProfileCmd::Apply { name } => ops::profile::apply(&name).await,
            ProfileCmd::Save { name } => ops::profile::save(&name).await,
            ProfileCmd::Delete { name } => ops::profile::delete(&name).await,
        },

        (Cmd::Alias { name, target }, Some(c)) => {
            c.set_alias(&name, &target)
                .await
                .map_err(ops::CliError::from)?;
            println!("alias '{name}' -> {target}");
            Ok(())
        }
        (Cmd::Alias { .. }, None) => Err(ops::CliError::Other(
            "alias management needs the daemon (it owns the config)".into(),
        )),

        (Cmd::Status { json }, Some(c)) => client::status(c.as_ref(), json).await,
        (Cmd::Status { json }, None) => ops::status(json).await,

        (Cmd::Hotkeys { cmd }, _) => match cmd {
            HotkeysCmd::Install => ops::hotkeys::install().await,
        },

        (Cmd::Ddc { cmd }, _) => match cmd {
            DdcCmd::Probe { target } => ops::ddc::probe(&target).await,
            DdcCmd::OptIn { target } => ops::ddc::set_opt_in(&target, true).await,
            DdcCmd::OptOut { target } => ops::ddc::set_opt_in(&target, false).await,
        },

        (Cmd::Daemon { .. }, _) => unreachable!("handled above"),
    }
}

fn on_flags() -> ops::ToggleOpts {
    ops::ToggleOpts {
        force: false,
        no_timer: true,
        timer: None,
    }
}

async fn daemon_cmd(cmd: &DaemonCmd) -> Result<(), ops::CliError> {
    match cmd {
        DaemonCmd::Status => match client::try_connect(false).await {
            Some(c) => client::status(c.as_ref(), false).await,
            None => {
                println!("daemon:   not running");
                Ok(())
            }
        },
        DaemonCmd::Start => {
            if client::try_connect(false).await.is_some() {
                println!("daemon already running");
                return Ok(());
            }
            // Prefer the systemd unit when installed.
            let unit = std::process::Command::new("systemctl")
                .args(["--user", "start", "nosignal-daemon"])
                .status();
            if matches!(unit, Ok(s) if s.success()) {
                println!("started via systemd user unit");
                return Ok(());
            }
            // Fall back to spawning next to this binary.
            let daemon_bin = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("nosignald")))
                .filter(|p| p.exists())
                .unwrap_or_else(|| "nosignald".into());
            std::process::Command::new(daemon_bin)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|e| ops::CliError::Other(format!("could not spawn nosignald: {e}")))?;
            println!("daemon spawned");
            Ok(())
        }
        DaemonCmd::Stop => match client::try_connect(false).await {
            Some(c) => {
                c.quit().await.map_err(ops::CliError::from)?;
                println!("daemon stopping");
                Ok(())
            }
            None => {
                println!("daemon not running");
                Ok(())
            }
        },
        DaemonCmd::Logs => {
            let status = std::process::Command::new("journalctl")
                .args(["--user", "-u", "nosignal-daemon", "-n", "100", "--no-pager"])
                .status()
                .map_err(|e| ops::CliError::Other(format!("journalctl: {e}")))?;
            if !status.success() {
                eprintln!(
                    "(no journal for the unit — if the daemon was spawned manually, \
                     its logs went to its stderr)"
                );
            }
            Ok(())
        }
    }
}

impl From<ToggleFlags> for ops::ToggleOpts {
    fn from(f: ToggleFlags) -> Self {
        ops::ToggleOpts {
            force: f.force,
            no_timer: f.no_timer,
            timer: f.timer,
        }
    }
}
