use copper_settings::RouterSettings;

use crate::error::RouterError;
use crate::pipeline::batch_loop::{AutorouteBatchLoop, BatchLoopResult};
use crate::pipeline::fanout::FanoutRunSummary;
use crate::pipeline::optimizer::BatchOptimizer;
use crate::pipeline::stop::{PassRecord, RouterBudget, RouterStop};
use crate::pipeline::{ProgressSink, TaskState};
use crate::score::BoardStatistics;
use copper_board::Board;

pub fn run_pipeline(
    board: &mut Board,
    settings: &RouterSettings,
    stop: &RouterStop,
    budget: RouterBudget,
    progress: &mut dyn ProgressSink,
) -> Result<PipelineResult, RouterError> {
    // Project constraints can be attached after the board was prepared.
    crate::pipeline::raise_to_project_minimums(board);
    crate::autoroute::maze::ripup_history::reset();
    let router_enabled =
        settings.get_run_router() && settings.max_passes.is_none_or(|max| max >= 0);

    let router_loop: Option<BatchLoopResult> =
        if router_enabled && !stop.is_stop_auto_router_requested() {
            Some(AutorouteBatchLoop::run(
                board, settings, stop, budget, progress,
            )?)
        } else if settings.is_fanout_enabled() && !stop.is_stop_auto_router_requested() {
            let mut fanout_only_settings = settings.clone();
            fanout_only_settings.set_max_passes(Some(0));
            Some(AutorouteBatchLoop::run(
                board,
                &fanout_only_settings,
                stop,
                budget,
                progress,
            )?)
        } else {
            None
        };

    let mut optimizer_timed_out = false;
    let mut optimizer_last_reported_pass = 0;
    let optimizer_state = if settings.get_run_optimizer() {
        if stop.is_stop_requested() {
            Some(TaskState::Idle)
        } else {
            stop.begin_optimizer_stage();
            let mut optimizer = BatchOptimizer::new(settings);
            let result = optimizer.run_batch_loop(board, stop, budget, progress)?;
            optimizer_timed_out = result.timed_out;
            optimizer_last_reported_pass = result.last_reported_pass;
            Some(result.state)
        }
    } else {
        None
    };

    let final_statistics = BoardStatistics::new(board);

    let (router_state, router_passes_completed, fanout, per_pass) = match router_loop {
        Some(result) => (
            result.state,
            result.passes_run,
            result.fanout,
            result.per_pass,
        ),
        None => (TaskState::Idle, 0, None, Vec::new()),
    };

    let timed_out = router_state == TaskState::TimedOut
        || fanout.as_ref().is_some_and(|f| f.is_timed_out)
        || optimizer_timed_out;

    Ok(PipelineResult {
        router_state,
        optimizer_state,
        router_passes_completed,
        optimizer_passes_completed: optimizer_last_reported_pass,
        fanout,
        per_pass,
        final_statistics,
        timed_out,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct PipelineResult {
    pub router_state: TaskState,
    pub optimizer_state: Option<TaskState>,
    pub router_passes_completed: i32,
    pub optimizer_passes_completed: i32,
    pub fanout: Option<FanoutRunSummary>,
    pub per_pass: Vec<PassRecord>,
    pub final_statistics: BoardStatistics,
    pub timed_out: bool,
}

pub fn normalize_router_algorithm(algorithm: &str) -> String {
    if algorithm == RouterSettings::ALGORITHM_CURRENT {
        algorithm.to_string()
    } else {
        RouterSettings::ALGORITHM_CURRENT.to_string()
    }
}
