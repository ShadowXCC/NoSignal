//! Direct-mode operations: the CLI drives the display backend itself.

use nosignal_core::{
    ApplyMode, BackendError, DisplayBackend, LayoutPlan, Output, PlannedOutput, ResolveError,
    Topology,
    guards::{self, RiskClass},
    identity::resolve_target,
    layout::{self, normalize},
    remembered::RememberedLayouts,
};
use nosignald::{paths, select};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncBufReadExt;

const DEFAULT_TIMER_SECS: u64 = 20;

#[derive(Debug)]
pub enum CliError {
    Resolve(ResolveError),
    Guard(String),
    Backend(BackendError),
    Other(String),
}

impl CliError {
    pub fn exit_code(&self) -> i32 {
        match self {
            CliError::Resolve(ResolveError::Ambiguous { .. }) => 2,
            CliError::Guard(_) => 3,
            _ => 1,
        }
    }

    /// Actionable follow-up for known hardware realities.
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            CliError::Resolve(ResolveError::NotFound(_)) => Some(
                "if this display was just disabled or powered off, a DisplayPort monitor in \
                 deep sleep can drop off the bus entirely — wake it with its power button and retry",
            ),
            CliError::Backend(BackendError::OutputUnresponsive(_)) => {
                Some("wake the monitor with its power button and retry")
            }
            _ => None,
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliError::Resolve(e) => write!(f, "{e}"),
            CliError::Guard(msg) => write!(f, "{msg}"),
            CliError::Backend(e) => write!(f, "{e}"),
            CliError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl From<ResolveError> for CliError {
    fn from(e: ResolveError) -> Self {
        CliError::Resolve(e)
    }
}
impl From<BackendError> for CliError {
    fn from(e: BackendError) -> Self {
        CliError::Backend(e)
    }
}
impl From<serde_json::Error> for CliError {
    fn from(e: serde_json::Error) -> Self {
        CliError::Other(e.to_string())
    }
}

#[derive(Clone, Copy)]
pub struct ToggleOpts {
    pub force: bool,
    pub no_timer: bool,
    pub timer: Option<u64>,
}

pub async fn list(json: bool) -> Result<(), CliError> {
    let backend = select::detect().await?;
    let topo = backend.snapshot().await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&topo)?);
        return Ok(());
    }
    println!(
        "{:<12} {:<28} {:<5} {:<20} {:<12} FLAGS",
        "CONNECTOR", "NAME", "STATE", "MODE", "POSITION"
    );
    for o in &topo.outputs {
        let name = match &o.alias {
            Some(alias) => format!("{} [{alias}]", o.display_name),
            None => o.display_name.clone(),
        };
        let mode = o.mode.map(|m| m.to_string()).unwrap_or_else(|| "-".into());
        let pos = if o.enabled {
            format!("{},{}", o.position.0, o.position.1)
        } else {
            "-".into()
        };
        let mut flags = Vec::new();
        if o.primary {
            flags.push("primary");
        }
        if o.builtin {
            flags.push("builtin");
        }
        println!(
            "{:<12} {:<28} {:<5} {:<20} {:<12} {}",
            o.identity.connector,
            name,
            if o.enabled { "on" } else { "off" },
            mode,
            pos,
            flags.join(",")
        );
    }
    Ok(())
}

pub async fn status(json: bool) -> Result<(), CliError> {
    let backend = select::detect().await?;
    let topo = backend.snapshot().await?;
    if json {
        let value = serde_json::json!({
            "backend": backend.name(),
            "outputs_total": topo.outputs.len(),
            "outputs_enabled": topo.enabled_count(),
            "topology": topo,
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    println!("backend:  {}", backend.name());
    println!(
        "outputs:  {} connected, {} enabled",
        topo.outputs.len(),
        topo.enabled_count()
    );
    for o in &topo.outputs {
        println!(
            "  {:<12} {}",
            o.identity.connector,
            if o.enabled { "on" } else { "off" }
        );
    }
    Ok(())
}

pub async fn set_enabled(
    target: &str,
    want: Option<bool>,
    opts: ToggleOpts,
) -> Result<(), CliError> {
    let backend = select::detect().await?;
    let topo = backend.snapshot().await?;
    let output = resolve_target(target, &topo.outputs)?.clone();
    let connector = output.identity.connector.clone();
    let want = want.unwrap_or(!output.enabled);

    if output.enabled == want {
        println!(
            "{connector} is already {}",
            if want { "enabled" } else { "disabled" }
        );
        return Ok(());
    }

    let mut plan = LayoutPlan::from_topology(&topo);
    if want {
        plan_enable(&mut plan, &output);
    } else {
        remember(&output);
        plan.set_enabled(&connector, false);
    }
    normalize(&mut plan);

    let risk = guards::assess(&topo, &plan);
    let use_timer = match risk {
        RiskClass::LastActiveDisplay | RiskClass::BuiltinPanel => {
            if opts.no_timer {
                return Err(CliError::Guard(format!(
                    "refusing --no-timer: {} — the auto-revert timer is mandatory here",
                    risk_text(risk)
                )));
            }
            true
        }
        RiskClass::Routine => opts.timer.is_some() && !opts.no_timer,
    };
    if risk != RiskClass::Routine {
        eprintln!("warning: {}", risk_text(risk));
        if !opts.force && !confirm_risk().await {
            return Err(CliError::Guard("aborted".into()));
        }
    }

    if use_timer {
        let secs = opts.timer.unwrap_or(DEFAULT_TIMER_SECS);
        revert_flow(backend.as_ref(), &topo, &plan, secs).await?;
    } else {
        apply_with_rebase(backend.as_ref(), &plan, ApplyMode::Persistent)
            .await
            .map_err(|e| enable_hint(e, want))?;
        println!(
            "{connector} is now {}",
            if want { "enabled" } else { "disabled" }
        );
    }
    Ok(())
}

fn risk_text(risk: RiskClass) -> &'static str {
    match risk {
        RiskClass::LastActiveDisplay => {
            "this disables your LAST active display; the desktop will go dark until \
             the change is confirmed or auto-reverts"
        }
        RiskClass::BuiltinPanel => {
            "this disables a built-in laptop panel; if the external display fails you \
             may be left without a screen"
        }
        RiskClass::Routine => "routine change",
    }
}

/// Plan enabling `output`: restore its remembered layout if we have one,
/// otherwise preferred mode placed to the right of the current desktop.
fn plan_enable(plan: &mut LayoutPlan, output: &Output) {
    let remembered = RememberedLayouts::load(&paths::remembered_path()).unwrap_or_default();
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

/// Store the output's current logical config so a later `on` restores it
/// exactly. Best-effort: failure to persist must not block the disable.
fn remember(output: &Output) {
    let path = paths::remembered_path();
    let mut store = RememberedLayouts::load(&path).unwrap_or_default();
    store.upsert(PlannedOutput::from_output(output));
    if let Err(e) = store.save(&path) {
        eprintln!("warning: could not save remembered layout: {e}");
    }
}

/// Apply, rebasing the plan onto a fresh snapshot once if the serial raced.
async fn apply_with_rebase(
    backend: &dyn DisplayBackend,
    plan: &LayoutPlan,
    mode: ApplyMode,
) -> Result<(), BackendError> {
    match backend.apply(plan, mode).await {
        Err(BackendError::StaleSerial) => {
            let fresh = backend.snapshot().await?;
            let rebased = layout::rebase_plan(plan, &fresh);
            backend.apply(&rebased, mode).await
        }
        other => other,
    }
}

fn enable_hint(e: BackendError, enabling: bool) -> CliError {
    if enabling && matches!(e, BackendError::Server(_)) {
        CliError::Backend(BackendError::OutputUnresponsive(format!("{e}")))
    } else {
        CliError::Backend(e)
    }
}

/// GNOME-resolution-change flow: apply temporarily, count down, keep on
/// confirmation, revert on timeout.
async fn revert_flow(
    backend: &dyn DisplayBackend,
    prior: &Topology,
    plan: &LayoutPlan,
    secs: u64,
) -> Result<(), CliError> {
    apply_with_rebase(backend, plan, ApplyMode::Temporary).await?;

    eprintln!("applied temporarily — press Enter to keep");
    let confirmed = wait_for_enter(secs).await;

    if confirmed {
        let fresh = backend.snapshot().await?;
        let keep = LayoutPlan::from_topology(&fresh);
        apply_with_rebase(backend, &keep, ApplyMode::Persistent).await?;
        println!("kept");
    } else {
        let fresh = backend.snapshot().await?;
        let restore = layout::restore_plan(&fresh, prior);
        apply_with_rebase(backend, &restore, ApplyMode::Persistent).await?;
        println!("reverted");
    }
    Ok(())
}

async fn confirm_risk() -> bool {
    eprint!("proceed? [y/N] ");
    let mut line = String::new();
    let mut reader = tokio::io::BufReader::new(tokio::io::stdin());
    match reader.read_line(&mut line).await {
        Ok(n) if n > 0 => matches!(line.trim(), "y" | "Y" | "yes"),
        _ => false,
    }
}

/// Countdown on stderr; true when the user pressed Enter in time. On EOF
/// (no interactive stdin) the full timeout elapses and we return false —
/// safe-by-default for scripts that hit a mandatory-timer case.
async fn wait_for_enter(secs: u64) -> bool {
    let mut reader = tokio::io::BufReader::new(tokio::io::stdin());
    let mut line = String::new();
    let mut stdin_open = true;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    let mut tick = tokio::time::interval(Duration::from_secs(1));

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            eprintln!();
            return false;
        }
        eprint!("\rauto-revert in {:>3}s ", remaining.as_secs());

        if stdin_open {
            tokio::select! {
                read = reader.read_line(&mut line) => match read {
                    Ok(0) | Err(_) => stdin_open = false,
                    Ok(_) => {
                        eprintln!();
                        return true;
                    }
                },
                _ = tick.tick() => {}
            }
        } else {
            tick.tick().await;
        }
    }
}

// Keep the Arc<dyn DisplayBackend> alias importable for future daemon-mode ops.
#[allow(dead_code)]
type Backend = Arc<dyn DisplayBackend>;
