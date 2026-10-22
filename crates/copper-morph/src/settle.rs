//! The settle engine's driver: parts step toward their targets cycle by cycle, largest remaining
//! distance first, each step taking the gentlest move that works (warp the copper, else shove
//! it, else push or step as a group, ripping what must be re-planned), until every part arrives
//! or nothing has moved for a while.

use std::collections::BTreeMap;

use copper_board::Board;
use copper_board::structure::Unit;
use copper_geometry::IntVector;

use crate::oracle::{MoveOracle, scale};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SettleOptions {
    /// Largest step a part takes per cycle, in millimetres (settle's STEP).
    pub step_mm: f64,
    pub max_cycles: usize,
    /// Cycles without progress before giving up (settle's stagnation window).
    pub patience: usize,
    pub push_depth: usize,
    pub max_group: usize,
}

impl Default for SettleOptions {
    fn default() -> Self {
        SettleOptions {
            step_mm: 0.05,
            max_cycles: 600,
            patience: 5,
            push_depth: 1,
            max_group: 6,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MoveKind {
    Warp,
    Shove,
    Push,
    Group,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PartOutcome {
    /// How far the part got toward its target, in board units, and how far it wanted to go.
    pub travelled: f64,
    pub wanted: f64,
    pub arrived: bool,
    pub moves: BTreeMap<MoveKind, usize>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SettleReport {
    pub cycles: usize,
    pub parts: BTreeMap<i32, PartOutcome>,
}

impl MoveOracle {
    /// Moves each component toward `targets[component]` (a displacement from where it starts).
    pub fn settle(
        &self,
        board: &mut Board,
        targets: &BTreeMap<i32, IntVector>,
        options: &SettleOptions,
    ) -> SettleReport {
        let per_mm = board.communication.get_resolution(Unit::Mm);
        let step = options.step_mm * per_mm;
        let arrive = 0.5;
        let mut remaining: BTreeMap<i32, (f64, f64)> = targets
            .iter()
            .map(|(c, v)| (*c, (f64::from(v.x), f64::from(v.y))))
            .collect();
        let mut report = SettleReport::default();
        for (component, (x, y)) in &remaining {
            report.parts.insert(
                *component,
                PartOutcome {
                    wanted: x.hypot(*y),
                    ..PartOutcome::default()
                },
            );
        }
        let mut idle = 0;
        for cycle in 0..options.max_cycles {
            report.cycles = cycle + 1;
            let mut order: Vec<i32> = remaining.keys().copied().collect();
            order.sort_by(|a, b| {
                let len = |c: &i32| remaining[c].0.hypot(remaining[c].1);
                len(b).total_cmp(&len(a)).then(a.cmp(b))
            });
            let mut progress = 0.0;
            for component in order {
                let (rx, ry) = remaining[&component];
                let length = rx.hypot(ry);
                if length <= arrive {
                    continue;
                }
                let k = (step / length).min(1.0);
                let want = IntVector::new((rx * k).round() as i32, (ry * k).round() as i32);
                if want == IntVector::ZERO {
                    continue;
                }
                let moved = self.step_part(board, component, &want, options);
                for (part, kind, v) in moved {
                    let (dx, dy) = (f64::from(v.x), f64::from(v.y));
                    if let Some(r) = remaining.get_mut(&part) {
                        r.0 -= dx;
                        r.1 -= dy;
                    }
                    if let Some(outcome) = report.parts.get_mut(&part) {
                        outcome.travelled += dx.hypot(dy);
                        *outcome.moves.entry(kind).or_insert(0) += 1;
                    }
                    if part == component {
                        progress += dx.hypot(dy);
                    }
                }
            }
            if remaining.values().all(|(x, y)| x.hypot(*y) <= arrive) {
                break;
            }
            idle = if progress > 0.0 { 0 } else { idle + 1 };
            if idle >= options.patience {
                break;
            }
        }
        for (component, (x, y)) in &remaining {
            if let Some(outcome) = report.parts.get_mut(component) {
                outcome.arrived = x.hypot(*y) <= arrive;
            }
        }
        report
    }

    /// One part's step: every (part, kind, vector) actually moved.
    fn step_part(
        &self,
        board: &mut Board,
        component: i32,
        want: &IntVector,
        options: &SettleOptions,
    ) -> Vec<(i32, MoveKind, IntVector)> {
        if self.translate_warping(board, &[component], want) {
            return vec![(component, MoveKind::Warp, *want)];
        }
        if self.translate_shoving(board, &[component], want) {
            return vec![(component, MoveKind::Shove, *want)];
        }
        let pushed = self.push(board, component, want, options.push_depth);
        if pushed.iter().any(|(c, _)| *c == component) {
            return pushed
                .into_iter()
                .map(|(c, v)| (c, MoveKind::Push, v))
                .collect();
        }
        let (group, step) = self.group_step(board, component, want, options.max_group);
        if step != IntVector::ZERO {
            return group
                .into_iter()
                .map(|c| (c, MoveKind::Group, step))
                .collect();
        }
        let (fraction, _) = self.line_search(board, component, want);
        let partial = scale(want, fraction);
        if partial != IntVector::ZERO && self.translate(board, component, &partial) {
            return vec![(component, MoveKind::Push, partial)];
        }
        Vec::new()
    }
}
