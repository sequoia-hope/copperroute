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
        let mut result = run_pipeline(board, &first_pass, stop, budget, progress)?;

        let per_mm = board.communication.get_resolution(Unit::Mm);
        let oracle = MoveOracle {
            guard: GUARD_MM * per_mm,
            routable_copper_is_soft: true,
        };
        let mut trial_settings = settings.clone();
        trial_settings.set_run_optimizer(false);
        trial_settings.max_passes = Some(TRIAL_PASSES);

        let mut open = unconnected_count(board);
        for round in 0..ROUNDS {
            let mut improved = false;
            for component in stuck_components(board).into_iter().take(PARTS_PER_ROUND) {
                if halted(stop) || open == 0 {
                    break;
                }
                let mut best: Option<(usize, Board, IntVector)> = None;
                for (dx, dy) in DIRECTIONS {
                    for distance in DISTANCES_MM {
                        if halted(stop) {
                            break;
                        }
                        let want = IntVector::new(
                            (dx * distance * per_mm).round() as i32,
                            (dy * distance * per_mm).round() as i32,
                        );
                        let mut trial = board.clone();
                        let moves = oracle.push(&mut trial, component, &want, PUSH_DEPTH);
                        let Some(&(_, step)) = moves.iter().rev().find(|(c, _)| *c == component)
                        else {
                            continue;
                        };
                        let moved_mm = f64::from(step.x).hypot(f64::from(step.y))
                            / per_mm.max(f64::MIN_POSITIVE);
                        if moved_mm < SMALLEST_MOVE_MM {
                            continue;
                        }
                        let trial_stop = RouterStop::with_deadline(TRIAL_MS);
                        if run_pipeline(
                            &mut trial,
                            &trial_settings,
                            &trial_stop,
                            RouterBudget::default(),
                            &mut NoopProgressSink,
                        )
                        .is_err()
                        {
                            continue;
                        }
                        let after = unconnected_count(&mut trial);
                        if after < best.as_ref().map_or(open, |(count, _, _)| *count) {
                            best = Some((after, trial, step));
                        }
                    }
                }
                if let Some((count, trial, step)) = best {
                    tracing::info!(
                        "nudge round {round}: component {component} by ({}, {}) closes {} connection(s)",
                        step.x,
                        step.y,
                        open - count
                    );
                    *board = trial;
                    open = count;
                    improved = true;
                }
            }
            if !improved {
                break;
            }
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

/// A full stop only: the router legitimately leaves its own stop in the autorouter-only state
/// when it finishes, so that state cannot mean "stop nudging".
fn halted(stop: &RouterStop) -> bool {
    stop.poll_cancel();
    stop.poll_deadline();
    stop.is_stop_requested()
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
