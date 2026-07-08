//! `nosignal` — command-line client.
//!
//! M1 operates directly on the display backend; once the daemon lands (M2)
//! the CLI becomes a thin IPC client and direct mode stays available behind
//! `--direct` for daemon-less use (SSH rescue, scripting, debugging).
//!
//! Exit codes: 0 ok · 1 error · 2 ambiguous target · 3 guarded action refused.

mod ops;

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "nosignal",
    version,
    about = "Software unplug for displays: disable outputs as if the cable were pulled"
)]
struct Cli {
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
    /// Show backend and output summary
    Status {
        /// Machine-readable output
        #[arg(long)]
        json: bool,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::List { json } => ops::list(json).await,
        Cmd::Off { target, flags } => ops::set_enabled(&target, Some(false), flags.into()).await,
        Cmd::On { target } => {
            ops::set_enabled(
                &target,
                Some(true),
                ops::ToggleOpts {
                    force: false,
                    no_timer: true,
                    timer: None,
                },
            )
            .await
        }
        Cmd::Toggle { target, flags } => ops::set_enabled(&target, None, flags.into()).await,
        Cmd::Status { json } => ops::status(json).await,
    };

    match result {
        Ok(()) => {}
        Err(err) => {
            eprintln!("error: {err}");
            if let Some(hint) = err.hint() {
                eprintln!("hint: {hint}");
            }
            std::process::exit(err.exit_code());
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
