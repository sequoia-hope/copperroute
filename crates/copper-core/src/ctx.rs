use crate::backend::{MazeBackend, RouterBackend};
use crate::cancel::{CancelToken, JobStopReason};
use crate::progress::SyncProgressSink;

#[derive(Debug)]
pub struct Ctx<'a> {
    pub settings: &'a copper_settings::RouterSettings,
    pub cancel: CancelToken,
    pub progress: &'a SyncProgressSink,
    pub budget: copper_router::pipeline::RouterBudget,
    pub backend: &'static dyn RouterBackend,
}

impl<'a> Ctx<'a> {
    pub fn new(
        settings: &'a copper_settings::RouterSettings,
        progress: &'a SyncProgressSink,
    ) -> Ctx<'a> {
        Ctx {
            settings,
            cancel: CancelToken::new(),
            progress,
            budget: copper_router::pipeline::RouterBudget::default(),
            backend: &MazeBackend,
        }
    }

    pub fn with_disabled_budget(
        settings: &'a copper_settings::RouterSettings,
        progress: &'a SyncProgressSink,
    ) -> Ctx<'a> {
        Ctx {
            budget: copper_router::pipeline::RouterBudget::disabled(),
            ..Ctx::new(settings, progress)
        }
    }
}

#[derive(Debug, Clone)]
pub struct RoutingResult {
    pub stats: copper_router::score::BoardStatistics,
    pub unrouted_report: String,
    pub drc_violations: Vec<copper_drc::DrcViolation>,
    pub timed_out: bool,
    pub stop_reason: Option<JobStopReason>,
    pub pipeline: copper_router::pipeline::PipelineResult,
}

impl RoutingResult {
    pub fn incomplete_count(&self) -> Option<i32> {
        self.stats.connections.incomplete_count
    }

    pub fn violation_count(&self) -> usize {
        self.drc_violations.len()
    }
}
