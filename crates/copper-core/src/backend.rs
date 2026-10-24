use copper_board::Board;
use copper_router::RouterError;
use copper_router::pipeline::{
    PipelineResult, ProgressSink, RouterBudget, RouterStop, TaskState, run_pipeline,
};
use copper_router::score::BoardStatistics;
use copper_settings::RouterSettings;

/// A routing approach. It adds copper to `board` in place and must honour `stop`, which carries
/// the job's deadline, cancellation and memory ceiling.
pub trait RouterBackend: std::fmt::Debug + Send + Sync {
    fn name(&self) -> &'static str;
    fn summary(&self) -> &'static str;
    fn route(
        &self,
        board: &mut Board,
        settings: &RouterSettings,
        stop: &RouterStop,
        budget: RouterBudget,
        progress: &mut dyn ProgressSink,
    ) -> Result<PipelineResult, RouterError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MazeBackend;

impl RouterBackend for MazeBackend {
    fn name(&self) -> &'static str {
        "maze"
    }

    fn summary(&self) -> &'static str {
        "shape-based maze router with fanout and optimizer"
    }

    fn route(
        &self,
        board: &mut Board,
        settings: &RouterSettings,
        stop: &RouterStop,
        budget: RouterBudget,
        progress: &mut dyn ProgressSink,
    ) -> Result<PipelineResult, RouterError> {
        run_pipeline(board, settings, stop, budget, progress)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NoneBackend;

impl RouterBackend for NoneBackend {
    fn name(&self) -> &'static str {
        "none"
    }

    fn summary(&self) -> &'static str {
        "adds no copper; the floor every other backend is scored against"
    }

    fn route(
        &self,
        board: &mut Board,
        _settings: &RouterSettings,
        _stop: &RouterStop,
        _budget: RouterBudget,
        _progress: &mut dyn ProgressSink,
    ) -> Result<PipelineResult, RouterError> {
        Ok(PipelineResult {
            router_state: TaskState::Finished,
            optimizer_state: None,
            router_passes_completed: 0,
            optimizer_passes_completed: 0,
            fanout: None,
            per_pass: Vec::new(),
            final_statistics: BoardStatistics::new(board),
            timed_out: false,
        })
    }
}

pub const DEFAULT_BACKEND: &str = "maze";

#[cfg(not(feature = "gpu"))]
static BACKENDS: [&dyn RouterBackend; 3] =
    [&MazeBackend, &NoneBackend, &crate::nudge::NudgeBackend];
#[cfg(feature = "gpu")]
static BACKENDS: [&dyn RouterBackend; 5] = [
    &MazeBackend,
    &NoneBackend,
    &crate::nudge::NudgeBackend,
    &crate::grid_backend::GridBackend,
    &crate::topo_backend::TopoBackend,
];

pub fn backends() -> &'static [&'static dyn RouterBackend] {
    &BACKENDS
}

pub fn backend(name: &str) -> Option<&'static dyn RouterBackend> {
    BACKENDS
        .iter()
        .copied()
        .find(|backend| backend.name() == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_the_default_is_registered() {
        let mut names: Vec<_> = backends().iter().map(|b| b.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), backends().len());
        assert_eq!(backend(DEFAULT_BACKEND).unwrap().name(), DEFAULT_BACKEND);
        assert!(backend("no-such-router").is_none());
    }
}
