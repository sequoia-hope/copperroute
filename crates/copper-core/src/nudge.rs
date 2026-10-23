//! Route, then nudge the parts whose connections did not close, keeping a nudge only when
//! rerouting from it closes more connections. The router is the referee.

use std::collections::BTreeMap;

use copper_board::Board;
use copper_board::structure::Unit;
use copper_geometry::IntVector;
use copper_morph::MoveOracle;
use copper_router::RouterError;
use copper_router::pipeline::{
    NoopProgressSink, PipelineResult, ProgressSink, RouterBudget, RouterStop, run_pipeline,
};
use copper_settings::RouterSettings;

use crate::backend::RouterBackend;

const DIRECTIONS: [(f64, f64); 8] = [
    (1.0, 0.0),
    (-1.0, 0.0),
    (0.0, 1.0),
    (0.0, -1.0),
    (0.707, 0.707),
    (-0.707, 0.707),
    (0.707, -0.707),
    (-0.707, -0.707),
];
const DISTANCES_MM: [f64; 2] = [0.2, 0.5];
const SMALLEST_MOVE_MM: f64 = 0.05;
/// A targeted move is computed, not guessed, so it may be far smaller than a blind one.
const SMALLEST_TARGETED_MOVE_MM: f64 = 0.01;
const GUARD_MM: f64 = 0.015;
const TRIAL_PASSES: i32 = 6;
const TRIAL_MS: i32 = 20_000;
const ROUNDS: usize = 2;
const PARTS_PER_ROUND: usize = 4;
const PUSH_DEPTH: usize = 1;

#[derive(Debug, Clone, Copy, Default)]
pub struct NudgeBackend;

impl RouterBackend for NudgeBackend {
    fn name(&self) -> &'static str {
        "nudge"
    }

    fn summary(&self) -> &'static str {
        "maze, then nudge parts with unclosed connections and keep moves that close more"
    }

    fn route(
        &self,
        board: &mut Board,
        settings: &RouterSettings,
        stop: &RouterStop,
        budget: RouterBudget,
        progress: &mut dyn ProgressSink,
    ) -> Result<PipelineResult, RouterError> {
        let mut first_pass = settings.clone();
        first_pass.set_run_optimizer(false);
        let started = web_time::Instant::now();
        let mut result = base_route(board, &first_pass, stop, budget, progress)?;
        let first_pass_ms = started.elapsed().as_millis();

        let per_mm = board.communication.get_resolution(Unit::Mm);
        let oracle = MoveOracle {
            guard: GUARD_MM * per_mm,
            routable_copper_is_soft: true,
        };
        let mut trial_settings = settings.clone();
        trial_settings.set_run_optimizer(false);
        trial_settings.max_passes = Some(TRIAL_PASSES);

        let mut nudger = Nudger {
            oracle,
            trial_settings,
            stop,
            per_mm,
            trial_ms: i32::try_from(first_pass_ms * 3 / 2)
                .unwrap_or(i32::MAX)
                .max(TRIAL_MS),
            open: unconnected_count(board),
            violations: violation_count(board),
        };
        match NudgeMode::from_env() {
            NudgeMode::Blind => nudger.blind(board),
            #[cfg(feature = "gpu")]
            NudgeMode::Targeted => nudger.targeted(board),
        }

        if settings.get_run_optimizer() && !halted(stop) {
            let mut finish = settings.clone();
            finish.max_passes = Some(TRIAL_PASSES);
            let finish_stop = RouterStop::with_deadline(TRIAL_MS * 3);
            let finished = run_pipeline(board, &finish, &finish_stop, budget, progress)?;
            result.optimizer_state = finished.optimizer_state;
            result.optimizer_passes_completed = finished.optimizer_passes_completed;
            result.final_statistics = finished.final_statistics;
        } else {
            result.final_statistics = copper_router::score::BoardStatistics::new(board);
        }
        Ok(result)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NudgeMode {
    /// Every part with an open connection tries a fixed set of directions and distances.
    Blind,
    /// Parts in the way of an open connection move by the displacement that clears its path.
    #[cfg(feature = "gpu")]
    Targeted,
}

impl NudgeMode {
    fn from_env() -> NudgeMode {
        #[cfg(feature = "gpu")]
        if std::env::var("COPPERROUTE_NUDGE").map_or(true, |v| v != "blind") {
            return NudgeMode::Targeted;
        }
        NudgeMode::Blind
    }
}

struct Nudger<'a> {
    oracle: MoveOracle,
    trial_settings: RouterSettings,
    stop: &'a RouterStop,
    per_mm: f64,
    /// A trial reroute's deadline: enough for the base router to do what it did on the first
    /// pass.
    trial_ms: i32,
    open: usize,
    violations: usize,
}

impl Nudger<'_> {
    fn blind(&mut self, board: &mut Board) {
        for round in 0..ROUNDS {
            let mut improved = false;
            for component in stuck_components(board).into_iter().take(PARTS_PER_ROUND) {
                if halted(self.stop) || self.open == 0 {
                    break;
                }
                let mut best: Option<Trial> = None;
                for (dx, dy) in DIRECTIONS {
                    for distance in DISTANCES_MM {
                        if halted(self.stop) {
                            break;
                        }
                        let want = IntVector::new(
                            (dx * distance * self.per_mm).round() as i32,
                            (dy * distance * self.per_mm).round() as i32,
                        );
                        let to_beat = best.as_ref().map_or(self.open, |found| found.open);
                        if let Some(found) =
                            self.try_move(board, component, &want, SMALLEST_MOVE_MM, to_beat)
                        {
                            best = Some(found);
                        }
                    }
                }
                if let Some(found) = best {
                    self.accept(board, round, component, found);
                    improved = true;
                }
            }
            if !improved {
                break;
            }
        }
    }

    /// Greedy: each round blames the parts blocking the open connections, tries the smallest
    /// clearing moves first, and keeps the first that closes more.
    #[cfg(feature = "gpu")]
    fn targeted(&mut self, board: &mut Board) {
        let mut tried: std::collections::BTreeSet<(i32, i32, i32)> = Default::default();
        for round in 0..targeted::ROUNDS {
            if self.open == 0 {
                break;
            }
            let mut improved = false;
            for (component, want) in targeted::moves(board, self.per_mm) {
                if halted(self.stop) {
                    return;
                }
                if !tried.insert((component, want.x, want.y)) {
                    continue;
                }
                if let Some(found) = self.try_move(
                    board,
                    component,
                    &want,
                    SMALLEST_TARGETED_MOVE_MM,
                    self.open,
                ) {
                    self.accept(board, round, component, found);
                    improved = true;
                    break;
                }
            }
            if !improved {
                break;
            }
        }
    }

    /// Moves `component` by `want` (or as far as pushing its neighbour allows) on a copy,
    /// reroutes, and returns the copy if it leaves fewer than `to_beat` open connections and no
    /// more violations.
    fn try_move(
        &self,
        board: &Board,
        component: i32,
        want: &IntVector,
        smallest_mm: f64,
        to_beat: usize,
    ) -> Option<Trial> {
        let mut trial = board.clone();
        let step = if self
            .oracle
            .translate_shoving(&mut trial, &[component], want)
        {
            *want
        } else {
            let moves = self.oracle.push(&mut trial, component, want, PUSH_DEPTH);
            moves.iter().rev().find(|(c, _)| *c == component)?.1
        };
        if f64::from(step.x).hypot(f64::from(step.y)) / self.per_mm < smallest_mm {
            return None;
        }
        let trial_stop = RouterStop::with_deadline(self.trial_ms);
        base_route(
            &mut trial,
            &self.trial_settings,
            &trial_stop,
            RouterBudget::default(),
            &mut NoopProgressSink,
        )
        .ok()?;
        let open = unconnected_count(&mut trial);
        tracing::info!(
            "nudge trial: {} by ({:.3}, {:.3}) mm leaves {open} open",
            component_name(board, component),
            f64::from(step.x) / self.per_mm,
            f64::from(step.y) / self.per_mm,
        );
        if open >= to_beat {
            return None;
        }
        let violations = violation_count(&mut trial);
        (violations <= self.violations).then_some(Trial {
            board: trial,
            step,
            open,
            violations,
        })
    }

    fn accept(&mut self, board: &mut Board, round: usize, component: i32, found: Trial) {
        tracing::info!(
            "nudge round {round}: {} by ({:.3}, {:.3}) mm closes {} connection(s)",
            component_name(board, component),
            f64::from(found.step.x) / self.per_mm,
            f64::from(found.step.y) / self.per_mm,
            self.open - found.open
        );
        self.open = found.open;
        self.violations = found.violations;
        *board = found.board;
    }
}

struct Trial {
    board: Board,
    step: IntVector,
    open: usize,
    violations: usize,
}

fn component_name(board: &Board, component: i32) -> String {
    board
        .components
        .get_all()
        .find(|c| c.id == component)
        .map_or_else(|| component.to_string(), |c| c.name.clone())
}

#[cfg(feature = "gpu")]
mod targeted {
    use std::collections::BTreeSet;

    use copper_board::{Board, ItemId};
    use copper_geometry::IntVector;

    use super::GUARD_MM;

    pub(super) const ROUNDS: usize = 8;
    const BLAME_CELL_MM: f64 = 0.05;
    const BLAME_VIA_COST: u32 = 200;
    /// Beyond the guard: a grid router keeps its cell centres a little further off than the
    /// rules.
    const SLACK_MM: f64 = 0.03;
    const MAX_MOVE_MM: f64 = 1.5;
    /// Routed copper first as a wall, so moves open a channel of their own, then as a price
    /// against intruding into parts.
    const ROUTED_COPPER: [Option<u32>; 2] = [None, Some(6 * 10)];

    /// Clearing moves for the parts blocking each open connection, at full and one and a half
    /// times the computed displacement, smallest first.
    pub(super) fn moves(board: &mut Board, per_mm: f64) -> Vec<(i32, IntVector)> {
        let fixed: BTreeSet<i32> = board
            .components
            .get_all()
            .filter(|c| c.position_fixed)
            .map(|c| c.id)
            .collect();
        let options = |routed_copper| copper_gpu::BlameOptions {
            cell: BLAME_CELL_MM * per_mm,
            via_cost: BLAME_VIA_COST,
            margin: (GUARD_MM + SLACK_MM) * per_mm,
            max_move: MAX_MOVE_MM * per_mm,
            routed_copper,
        };
        let gaps: Vec<(ItemId, ItemId)> = copper_drc::DesignRulesChecker::new(board)
            .get_all_unconnected_items()
            .into_iter()
            .filter_map(|g| g.second_item.map(|second| (g.first_item, second)))
            .collect();
        let mut moves: Vec<(f64, i32, IntVector)> = Vec::new();
        let movable = |component: i32| !fixed.contains(&component);
        let blamed = gaps
            .iter()
            .flat_map(|&(from, to)| ROUTED_COPPER.map(|routed| (from, to, options(routed))));
        for (from, to, options) in blamed.collect::<Vec<_>>() {
            for blamed in copper_gpu::blame(board, from, to, &options, &movable).blamed {
                for scale in [1.0, 1.5] {
                    let want = IntVector::new(
                        (blamed.dx * scale).round() as i32,
                        (blamed.dy * scale).round() as i32,
                    );
                    let length = blamed.dx.hypot(blamed.dy) * scale;
                    moves.push((length, blamed.component, want));
                }
            }
        }
        moves.sort_by(|a, b| a.0.total_cmp(&b.0));
        moves.dedup_by_key(|(_, component, want)| (*component, want.x, want.y));
        moves
            .into_iter()
            .map(|(_, component, want)| (component, want))
            .collect()
    }
}

/// The router nudging builds on: the maze router, or with `COPPERROUTE_NUDGE_BASE=grid` the grid
/// router (which itself finishes with the maze router).
fn base_route(
    board: &mut Board,
    settings: &RouterSettings,
    stop: &RouterStop,
    budget: RouterBudget,
    progress: &mut dyn ProgressSink,
) -> Result<PipelineResult, RouterError> {
    #[cfg(feature = "gpu")]
    if std::env::var("COPPERROUTE_NUDGE_BASE").is_ok_and(|v| v == "grid") {
        return crate::grid_backend::GridBackend.route(board, settings, stop, budget, progress);
    }
    run_pipeline(board, settings, stop, budget, progress)
}

/// A full stop only: the router legitimately leaves its own stop in the autorouter-only state
/// when it finishes, so that state cannot mean "stop nudging".
fn halted(stop: &RouterStop) -> bool {
    stop.poll_cancel();
    stop.poll_deadline();
    stop.is_stop_requested()
}

fn violation_count(board: &mut Board) -> usize {
    copper_drc::DesignRulesChecker::new(board)
        .get_all_violations()
        .len()
}

fn unconnected_count(board: &mut Board) -> usize {
    copper_drc::DesignRulesChecker::new(board)
        .get_all_unconnected_items()
        .len()
}

/// Components owning items of unclosed connections, most involved first.
fn stuck_components(board: &mut Board) -> Vec<i32> {
    let mut involvement: BTreeMap<i32, usize> = BTreeMap::new();
    let unconnected = copper_drc::DesignRulesChecker::new(board).get_all_unconnected_items();
    for gap in unconnected {
        for id in [Some(gap.first_item), gap.second_item]
            .into_iter()
            .flatten()
        {
            let Some(component) = board.get_item(id).map(|item| item.component_id()) else {
                continue;
            };
            let fixed = board
                .components
                .get_all()
                .any(|c| c.id == component && c.position_fixed);
            if component > 0 && !fixed {
                *involvement.entry(component).or_insert(0) += 1;
            }
        }
    }
    let mut ranked: Vec<(i32, usize)> = involvement.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    ranked.into_iter().map(|(component, _)| component).collect()
}
