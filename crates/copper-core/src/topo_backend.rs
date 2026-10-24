//! Topological routing first, maze after: open connections are routed as rubber bands through a
//! triangulation of the copper, then the maze router finishes and optimizes the board.

use copper_board::Board;
use copper_board::structure::Unit;
use copper_router::RouterError;
use copper_router::pipeline::{PipelineResult, ProgressSink, RouterBudget, RouterStop, run_pipeline};
use copper_settings::RouterSettings;
use copper_topo::{TopoOptions, route_topological};

use crate::backend::RouterBackend;

const SAMPLE_MM: f64 = 0.1;
const VIA_COST_MM: f64 = 2.0;
const MAX_EXPANSIONS: usize = 400_000;
const ORDERS: usize = 1;
/// The share of the job timeout topological routing may use; the maze router gets the rest.
const TOPO_SHARE_OF_JOB: f64 = 0.4;

#[derive(Debug, Clone, Copy, Default)]
pub struct TopoBackend;

impl RouterBackend for TopoBackend {
    fn name(&self) -> &'static str {
        "topo"
    }

    fn summary(&self) -> &'static str {
        "rubber-band routing through a triangulation of the copper, then the maze router to finish"
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
        let job_seconds = settings
            .job_timeout_string
            .as_deref()
            .and_then(crate::parse_timespan)
            .map(|d| d.as_secs_f64());
        let options = TopoOptions {
            sample: SAMPLE_MM * per_mm,
            via_cost: VIA_COST_MM * per_mm,
            max_seconds: job_seconds.map(|s| s * TOPO_SHARE_OF_JOB),
            max_expansions: MAX_EXPANSIONS,
            orders: ORDERS,
        };
        let halted = || {
            stop.poll_cancel();
            stop.poll_deadline();
            stop.is_stop_auto_router_requested()
        };
        route_topological(board, &options, &halted);
        if std::env::var_os("COPPERROUTE_TOPO_ONLY").is_some() {
            return crate::backend::NoneBackend.route(board, settings, stop, budget, progress);
        }
        run_pipeline(board, settings, stop, budget, progress)
    }
}
