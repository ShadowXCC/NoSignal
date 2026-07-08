//! Layout normalization: after disabling outputs, the remaining layout must
//! stay valid for the display server — exactly one primary among enabled
//! outputs, bounding-box origin at (0, 0), and no gaps for servers that
//! require adjacency (Mutter rejects disjoint layouts).

use crate::identity::MatchQuality;
use crate::topology::{LayoutPlan, PlannedOutput, Topology};

/// Normalize a plan in place:
/// 1. exactly one enabled output is primary (prefers the existing primary,
///    otherwise the leftmost-topmost enabled output),
/// 2. enabled outputs are translated so their bounding box starts at (0, 0).
pub fn normalize(plan: &mut LayoutPlan) {
    let enabled: Vec<usize> = plan
        .outputs
        .iter()
        .enumerate()
        .filter(|(_, o)| o.enabled)
        .map(|(i, _)| i)
        .collect();
    if enabled.is_empty() {
        return;
    }

    // Primary: keep the first enabled primary, demote the rest; if none,
    // promote the leftmost-topmost enabled output.
    let mut primary_seen = false;
    for &i in &enabled {
        if plan.outputs[i].primary {
            if primary_seen {
                plan.outputs[i].primary = false;
            }
            primary_seen = true;
        }
    }
    if !primary_seen {
        let best = enabled
            .iter()
            .copied()
            .min_by_key(|&i| plan.outputs[i].position)
            .expect("non-empty");
        plan.outputs[best].primary = true;
    }
    // A disabled output can never be primary.
    for o in plan.outputs.iter_mut().filter(|o| !o.enabled) {
        o.primary = false;
    }

    // Translate so the enabled bounding box origin is (0, 0).
    let min_x = enabled
        .iter()
        .map(|&i| plan.outputs[i].position.0)
        .min()
        .expect("non-empty");
    let min_y = enabled
        .iter()
        .map(|&i| plan.outputs[i].position.1)
        .min()
        .expect("non-empty");
    if min_x != 0 || min_y != 0 {
        for &i in &enabled {
            let p = &mut plan.outputs[i].position;
            *p = (p.0 - min_x, p.1 - min_y);
        }
    }
}

/// Re-pack enabled outputs horizontally (sorted by current x, then y),
/// preserving each output's y offset relative to the topmost. Fallback for
/// display servers that reject layouts with gaps after a middle monitor is
/// disabled. Uses the planned mode width as the advance; outputs without a
/// mode keep their position.
pub fn compact_horizontal(plan: &mut LayoutPlan) {
    let mut order: Vec<usize> = plan
        .outputs
        .iter()
        .enumerate()
        .filter(|(_, o)| o.enabled && o.mode.is_some())
        .map(|(i, _)| i)
        .collect();
    order.sort_by_key(|&i| plan.outputs[i].position);

    let mut x = 0i32;
    for &i in &order {
        let o = &mut plan.outputs[i];
        o.position.0 = x;
        let logical_width = logical_width(o);
        x += logical_width;
    }
    normalize(plan);
}

/// Build a plan that restores `prior`'s state on top of the current topology
/// (used to revert a pending change). Outputs are matched EDID-first; outputs
/// that appeared since `prior` keep their current state.
pub fn restore_plan(current: &Topology, prior: &Topology) -> LayoutPlan {
    let mut plan = LayoutPlan::from_topology(current);
    for planned in &mut plan.outputs {
        let old = prior
            .outputs
            .iter()
            .find(|o| o.identity.match_quality(&planned.identity) == MatchQuality::Edid)
            .or_else(|| {
                prior.outputs.iter().find(|o| {
                    o.identity.match_quality(&planned.identity) == MatchQuality::Connector
                })
            });
        if let Some(old) = old {
            *planned = PlannedOutput::from_output(old);
            // Keep the live identity (the connector may have migrated).
            planned.identity = plan_identity(planned, current);
        }
    }
    normalize(&mut plan);
    plan
}

fn plan_identity(planned: &PlannedOutput, current: &Topology) -> crate::identity::OutputIdentity {
    current
        .outputs
        .iter()
        .find(|o| o.identity.match_quality(&planned.identity) >= MatchQuality::Connector)
        .map(|o| o.identity.clone())
        .unwrap_or_else(|| planned.identity.clone())
}

/// Width the output occupies in logical desktop coordinates (accounts for
/// rotation and scale). Returns 0 when no mode is planned.
pub fn logical_width(o: &PlannedOutput) -> i32 {
    let Some(mode) = o.mode else { return 0 };
    let raw = match o.transform.to_u8() {
        // 90°/270° rotations swap width and height.
        1 | 3 | 5 | 7 => mode.height,
        _ => mode.width,
    };
    let scale = if o.scale > 0.0 { o.scale } else { 1.0 };
    (f64::from(raw) / scale).round() as i32
}

/// Rebase a plan onto a fresh topology snapshot after a serial race: keep the
/// desired per-output states, adopt the fresh serial and any newly appeared
/// outputs (which keep their current state).
pub fn rebase_plan(plan: &LayoutPlan, fresh: &Topology) -> LayoutPlan {
    let mut rebased = LayoutPlan::from_topology(fresh);
    for target in &mut rebased.outputs {
        let wanted = plan
            .outputs
            .iter()
            .find(|p| p.identity.match_quality(&target.identity) >= MatchQuality::Connector);
        if let Some(wanted) = wanted {
            let identity = target.identity.clone();
            *target = wanted.clone();
            target.identity = identity;
        }
    }
    normalize(&mut rebased);
    rebased
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::OutputIdentity;
    use crate::topology::{LayoutPlan, Mode, PlannedOutput, Transform};

    fn planned(connector: &str, enabled: bool, pos: (i32, i32), primary: bool) -> PlannedOutput {
        PlannedOutput {
            identity: OutputIdentity::new(connector, None),
            enabled,
            mode: Some(Mode {
                width: 1920,
                height: 1080,
                refresh_mhz: 60_000,
            }),
            position: pos,
            scale: 1.0,
            transform: Transform::Normal,
            primary,
        }
    }

    #[test]
    fn origin_translates_to_zero_after_disabling_leftmost() {
        let mut plan = LayoutPlan {
            serial: "1".into(),
            outputs: vec![
                planned("DP-1", false, (0, 0), false),
                planned("DP-2", true, (1920, 0), true),
            ],
        };
        normalize(&mut plan);
        assert_eq!(plan.outputs[1].position, (0, 0));
    }

    #[test]
    fn primary_moves_to_an_enabled_output() {
        let mut plan = LayoutPlan {
            serial: "1".into(),
            outputs: vec![
                planned("DP-1", false, (0, 0), true), // disabled primary
                planned("DP-2", true, (1920, 0), false),
            ],
        };
        normalize(&mut plan);
        assert!(!plan.outputs[0].primary);
        assert!(plan.outputs[1].primary);
    }

    #[test]
    fn exactly_one_primary_survives() {
        let mut plan = LayoutPlan {
            serial: "1".into(),
            outputs: vec![
                planned("DP-1", true, (0, 0), true),
                planned("DP-2", true, (1920, 0), true),
            ],
        };
        normalize(&mut plan);
        let primaries = plan.outputs.iter().filter(|o| o.primary).count();
        assert_eq!(primaries, 1);
    }

    #[test]
    fn compaction_closes_the_gap_left_by_a_middle_monitor() {
        let mut plan = LayoutPlan {
            serial: "1".into(),
            outputs: vec![
                planned("DP-1", true, (0, 0), true),
                planned("DP-2", false, (1920, 0), false), // middle, disabled
                planned("DP-3", true, (3840, 0), false),
            ],
        };
        compact_horizontal(&mut plan);
        assert_eq!(plan.outputs[0].position, (0, 0));
        assert_eq!(plan.outputs[2].position, (1920, 0));
    }

    #[test]
    fn rotated_output_advances_by_its_height() {
        let mut left = planned("DP-1", true, (0, 0), true);
        left.transform = Transform::Rot90;
        let right = planned("DP-2", true, (5000, 0), false);
        let mut plan = LayoutPlan {
            serial: "1".into(),
            outputs: vec![left, right],
        };
        compact_horizontal(&mut plan);
        assert_eq!(plan.outputs[1].position.0, 1080);
    }
}
