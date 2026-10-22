//! Grid first, maze after: open connections are routed on a conservative grid (on the GPU when
//! one is available), then the maze router finishes and optimizes the board.

use copper_board::Board;
use copper_board::structure::Unit;
use copper_gpu::{CpuSolver, FieldSolver, GpuSolver, GridOptions, route_open_connections};
use copper_router::RouterError;
use copper_router::pipeline::{
    PipelineResult, ProgressSink, RouterBudget, RouterStop, run_pipeline,
};
use copper_settings::RouterSettings;

use crate::backend::RouterBackend;

const DEFAULT_CELL_MM: f64 = 0.1;
const VIA_COST: u32 = 200;
const MAX_ATTEMPTS: usize = 2_000;
/// The share of the job timeout grid routing may use; the maze router gets the rest.
const GRID_SHARE_OF_JOB: f64 = 0.4;

#[derive(Debug, Clone, Copy, Default)]
pub struct GridBackend;

impl RouterBackend for GridBackend {
    fn name(&self) -> &'static str {
        "grid"
    }

    fn summary(&self) -> &'static str {
        "conservative grid routing (GPU when available), then the maze router to finish"
    }

    fn route(
        &self,
        board: &mut Board,
        settings: &RouterSettings,
        stop: &RouterStop,
        budget: RouterBudget,
        progress: &mut dyn ProgressSink,
    ) -> Result<PipelineResult, RouterError> {
        let per_mm = board.communication.get_resolution(Unit::Mm);
        let cell_mm = std::env::var("COPPERROUTE_GRID_CELL_MM")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| *v > 0.0)
            .unwrap_or(DEFAULT_CELL_MM);
        let job_seconds = settings
            .job_timeout_string
            .as_deref()
            .and_then(crate::parse_timespan)
            .map(|d| d.as_secs_f64());
        let options = GridOptions {
            cell: cell_mm * per_mm,
            via_cost: VIA_COST,
            max_attempts: MAX_ATTEMPTS,
            max_seconds: job_seconds.map(|s| s * GRID_SHARE_OF_JOB),
        };
        let use_cpu = std::env::var_os("COPPERROUTE_GRID_CPU").is_some();
        let mut gpu = if use_cpu { None } else { GpuSolver::new() };
        let solver: &mut dyn FieldSolver = match gpu.as_mut() {
            Some(gpu) => {
                tracing::info!("grid routing on {}", gpu.adapter_name);
                gpu
            }
            None => {
                tracing::info!("grid routing on the CPU");
                &mut CpuSolver
            }
        };
        let halted = || {
            stop.poll_cancel();
            stop.poll_deadline();
            stop.is_stop_auto_router_requested()
        };
        let stats = route_open_connections(board, solver, &options, &halted);
        tracing::info!(
            "grid: {} connections routed, {} left for the maze router, {} vias",
            stats.routed,
            stats.failed,
            stats.vias
        );
        run_pipeline(board, settings, stop, budget, progress)
    }
}
