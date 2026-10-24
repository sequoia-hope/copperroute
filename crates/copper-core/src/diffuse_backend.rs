//! Route, spread the parts by diffusion away from where the open connections crowd, route again,
//! and keep whichever board is better. The router is the referee.

use std::collections::BTreeSet;

use copper_board::Board;
use copper_board::structure::Unit;
use copper_morph::{DiffuseOptions, MoveOracle, airline_demand, apply_moves, diffusion_moves};
use copper_router::RouterError;
use copper_router::pipeline::{PipelineResult, ProgressSink, RouterBudget, RouterStop};
use copper_settings::RouterSettings;

use crate::backend::RouterBackend;
use crate::nudge::{halted, route_on, unconnected_count, violation_count};

/// The width an open connection is taken to need, for its routing demand.
const AIRLINE_WIDTH_MM: f64 = 0.4;
const AIRLINE_SAMPLE_MM: f64 = 0.5;
const GUARD_MM: f64 = 0.015;
const SWEEPS: usize = 3;

#[derive(Debug, Clone, Copy, Default)]
pub struct DiffuseBackend;

impl RouterBackend for DiffuseBackend {
    fn name(&self) -> &'static str {
        "diffuse"
    }

    fn summary(&self) -> &'static str {
        "route, spread crowded parts by diffusion, reroute, keep the better board"
    }

    fn route(
        &self,
        board: &mut Board,
        settings: &RouterSettings,
        stop: &RouterStop,
        budget: RouterBudget,
        progress: &mut dyn ProgressSink,
    ) -> Result<PipelineResult, RouterError> {
        let grid = std::env::var("COPPERROUTE_DIFFUSE_BASE").is_ok_and(|v| v == "grid");
        let mut first = settings.clone();
        first.set_run_optimizer(false);
        let mut result = route_on(grid, board, &first, stop, budget, progress)?;
        let open = unconnected_count(board);
        let violations = violation_count(board);
        if open == 0 || halted(stop) {
            return Ok(result);
        }

        let per_mm = board.communication.get_resolution(Unit::Mm);
        let options = options_from_env();
        let report = diffusion_moves(board, &options, &[], &BTreeSet::new(), &airline_demand(board, AIRLINE_WIDTH_MM, AIRLINE_SAMPLE_MM));
        let oracle = MoveOracle {
            guard: GUARD_MM * per_mm,
            routable_copper_is_soft: true,
            fixed_copper_bends: std::env::var("COPPERROUTE_NUDGE_BEND_FIXED").is_ok_and(|v| v == "1"),
        };
        let mut spread = board.clone();
        let made = apply_moves(&mut spread, &oracle, &report.moves, &[], SWEEPS);
        tracing::info!(
            "diffuse: {open} open after the first route; peak density {:.2} -> {:.2} in {} rounds, {} of {} parts moved",
            report.peak_before,
            report.peak_after,
            report.rounds,
            made.len(),
            report.moves.len()
        );
        if made.is_empty() {
            return finish(board, settings, stop, budget, progress, grid, result);
        }
        let rerouted = route_on(grid, &mut spread, &first, stop, budget, progress)?;
        let spread_open = unconnected_count(&mut spread);
        let spread_violations = violation_count(&mut spread);
        tracing::info!(
            "diffuse: {spread_open} open and {spread_violations} violation(s) spread, from {open} and {violations}"
        );
        if spread_open < open && spread_violations <= violations {
            *board = spread;
            result = rerouted;
        }
        finish(board, settings, stop, budget, progress, grid, result)
    }
}

fn finish(
    board: &mut Board,
    settings: &RouterSettings,
    stop: &RouterStop,
    budget: RouterBudget,
    progress: &mut dyn ProgressSink,
    grid: bool,
    result: PipelineResult,
) -> Result<PipelineResult, RouterError> {
    if !settings.get_run_optimizer() || halted(stop) {
        return Ok(result);
    }
    route_on(grid, board, settings, stop, budget, progress)
}

fn options_from_env() -> DiffuseOptions {
    let number = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<f64>().ok());
    let defaults = DiffuseOptions::default();
    DiffuseOptions {
        target_density: number("COPPERROUTE_DIFFUSE_TARGET").unwrap_or(defaults.target_density),
        max_move_mm: number("COPPERROUTE_DIFFUSE_MAX_MM").unwrap_or(defaults.max_move_mm),
        cell_mm: number("COPPERROUTE_DIFFUSE_CELL_MM").unwrap_or(defaults.cell_mm),
        ..defaults
    }
}
