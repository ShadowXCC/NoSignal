//! Risk assessment for layout changes — the policy behind auto-revert timers.
//!
//! The daemon (and the CLI in direct mode) runs every apply through
//! [`assess`]. The rules, per the project decisions:
//!
//! - Disabling the **last active display** is allowed but *always* arms a
//!   revert timer; `--force` cannot bypass it.
//! - Disabling a **built-in laptop panel** always arms a revert timer, even
//!   when other displays remain (a dead internal panel plus a flaky external
//!   display strands the user).
//! - Everything else is routine; clients choose whether to use a timer.

use crate::topology::{LayoutPlan, Topology};
use serde::{Deserialize, Serialize};

/// Risk classification for a plan, ordered by severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RiskClass {
    /// No display-loss risk; revert timer optional.
    Routine,
    /// Disables an internal laptop panel; revert timer mandatory.
    BuiltinPanel,
    /// Leaves zero active displays; revert timer mandatory, cannot be forced off.
    LastActiveDisplay,
}

impl RiskClass {
    /// Whether the auto-revert timer is mandatory for this class.
    pub fn timer_mandatory(&self) -> bool {
        !matches!(self, RiskClass::Routine)
    }
}

/// Assess the risk of applying `plan` on top of the current `topology`.
/// Returns the most severe applicable class.
pub fn assess(topology: &Topology, plan: &LayoutPlan) -> RiskClass {
    if plan.enabled_count() == 0 && topology.enabled_count() > 0 {
        return RiskClass::LastActiveDisplay;
    }

    let disables_builtin = plan.outputs.iter().any(|p| {
        !p.enabled
            && topology
                .find_connector(&p.identity.connector)
                .is_some_and(|o| o.builtin && o.enabled)
    });
    if disables_builtin {
        return RiskClass::BuiltinPanel;
    }

    RiskClass::Routine
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::OutputIdentity;
    use crate::topology::Output;

    fn topo(outputs: Vec<Output>) -> Topology {
        Topology {
            serial: "1".into(),
            outputs,
        }
    }

    fn out(connector: &str, enabled: bool, builtin: bool) -> Output {
        Output {
            identity: OutputIdentity::new(connector, None),
            enabled,
            builtin,
            ..Output::default()
        }
    }

    #[test]
    fn disabling_the_last_display_is_flagged() {
        let t = topo(vec![
            out("DP-1", true, false),
            out("HDMI-A-1", false, false),
        ]);
        let mut plan = LayoutPlan::from_topology(&t);
        plan.set_enabled("DP-1", false);
        assert_eq!(assess(&t, &plan), RiskClass::LastActiveDisplay);
        assert!(RiskClass::LastActiveDisplay.timer_mandatory());
    }

    #[test]
    fn disabling_builtin_panel_is_flagged_even_with_others_active() {
        let t = topo(vec![out("eDP-1", true, true), out("DP-1", true, false)]);
        let mut plan = LayoutPlan::from_topology(&t);
        plan.set_enabled("eDP-1", false);
        assert_eq!(assess(&t, &plan), RiskClass::BuiltinPanel);
    }

    #[test]
    fn routine_toggle_with_other_display_remaining() {
        let t = topo(vec![out("DP-1", true, false), out("HDMI-A-1", true, false)]);
        let mut plan = LayoutPlan::from_topology(&t);
        plan.set_enabled("HDMI-A-1", false);
        assert_eq!(assess(&t, &plan), RiskClass::Routine);
        assert!(!RiskClass::Routine.timer_mandatory());
    }

    #[test]
    fn re_enabling_is_routine() {
        let t = topo(vec![
            out("DP-1", true, false),
            out("HDMI-A-1", false, false),
        ]);
        let mut plan = LayoutPlan::from_topology(&t);
        plan.set_enabled("HDMI-A-1", true);
        assert_eq!(assess(&t, &plan), RiskClass::Routine);
    }
}
