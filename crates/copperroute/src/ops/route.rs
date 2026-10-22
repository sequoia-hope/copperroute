use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use copper_governor::{MemoryGuard, MemoryPressure};

use copper_core::{
    BoardFileDetails, CancelToken, Ctx, FileFormat, JobStopReason, RouterBudget, RoutingJob,
    RouterBackend, RoutingJobState, RoutingPipeline, RoutingResult, SyncProgressSink,
};
use copper_router::{RoutingVisualizationOptions, RoutingVisualizationSummary};
use copper_settings::RouterSettings;

use super::OpError;
use super::load::{LoadRequest, Loaded, load};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Ses,
    KicadSessionJson,
}

impl OutputFormat {
    pub fn from_path(path: &Path) -> Option<OutputFormat> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        match extension.as_str() {
            "ses" => Some(OutputFormat::Ses),
            "json" => Some(OutputFormat::KicadSessionJson),
            _ => None,
        }
    }

    fn file_format(self) -> FileFormat {
        match self {
            OutputFormat::Ses => FileFormat::Ses,
            OutputFormat::KicadSessionJson => FileFormat::KicadSessionJson,
        }
    }

    fn extension(self) -> &'static str {
        match self {
            OutputFormat::Ses => "ses",
            OutputFormat::KicadSessionJson => "json",
        }
    }
}

#[derive(Debug, Clone)]
pub enum OutputTarget {
    File(PathBuf),
    Session,
}

pub struct RouteRequest {
    pub load: LoadRequest,
    pub output: OutputTarget,
    pub cancel: CancelToken,
    pub progress: SyncProgressSink,
    pub visualize: Option<RoutingVisualizationOptions>,
    pub memory_ceiling: Option<u64>,
    pub backend: &'static dyn RouterBackend,
}

pub struct RouteOutcome {
    pub job: RoutingJob,
    pub session: Vec<u8>,
    pub format: OutputFormat,
    pub state: RoutingJobState,
    pub result: RoutingResult,
    pub visualization: Option<RoutingVisualizationSummary>,
    pub warnings: Vec<String>,
}

pub fn budget_for(settings: &RouterSettings) -> RouterBudget {
    let mut budget = RouterBudget::default();
    if let Some(ms) = settings.opt_changed_area_ms {
        budget.opt_changed_area_ms = ms;
    }
    budget
}

pub fn route(request: RouteRequest) -> Result<RouteOutcome, OpError> {
    let format = match &request.output {
        OutputTarget::File(path) => OutputFormat::from_path(path).ok_or_else(|| {
            OpError::Input(format!(
                "'{}' is not an output file this program can write: the output must end in \
                 .ses (a Specctra session) or .json (a KiCad session)",
                path.display()
            ))
        })?,
        OutputTarget::Session => OutputFormat::Ses,
    };

    let Loaded {
        mut job,
        mut board,
        transform,
        settings,
        mut warnings,
        ..
    } = load(&request.load)?;

    if let OutputTarget::File(path) = &request.output {
        job.try_to_set_output_file(Some(path));
    }

    let cancel = match copper_core::job_timeout_deadline(settings.job_timeout_string.as_deref()) {
        Ok(Some(deadline)) => request.cancel.with_deadline_from(deadline),
        Ok(None) => request.cancel.clone(),
        Err(error) => {
            return Err(OpError::Settings(format!("router.job_timeout: {error}")));
        }
    };

    let visualization = match request.visualize {
        Some(options) => Some(
            copper_router::start_routing_visualization(options)
                .map_err(|error| OpError::Input(format!("--visualize: {error}")))?,
        ),
        None => None,
    };

    job.started_at = Some(Instant::now());
    job.state = RoutingJobState::Running;
    let ctx = Ctx {
        settings: &settings,
        cancel,
        progress: &request.progress,
        budget: budget_for(&settings),
        backend: request.backend,
    };
    let guard = request
        .memory_ceiling
        .map(|ceiling| memory_guard(ceiling, &ctx.cancel));
    let result = RoutingPipeline::run(&mut board, &ctx)?;
    if let Some(warning) = guard.and_then(|guard| memory_warning(&guard)) {
        warnings.push(warning);
    }
    let visualization = visualization.map(copper_router::RoutingVisualizationGuard::finish);

    job.finished_at = Some(Instant::now());
    job.set_current_pass(result.pipeline.router_passes_completed);
    job.set_optimizer_pass(result.pipeline.optimizer_passes_completed);
    let state = match result.stop_reason {
        Some(JobStopReason::Deadline) => RoutingJobState::TimedOut,
        Some(JobStopReason::Cancelled) => RoutingJobState::Cancelled,
        None => RoutingJobState::Completed,
    };
    job.state = state;

    let session = match format {
        OutputFormat::Ses => {
            let mut buffer = Vec::new();
            copper_core::save_as_specctra_session_ses(&board, &transform, &job.name, &mut buffer)?;
            buffer
        }
        OutputFormat::KicadSessionJson => copper_dsn::kicad::write(&board, &job.name).into_bytes(),
    };
    attach_output(&mut job, &session, format);

    Ok(RouteOutcome {
        job,
        session,
        format,
        state,
        result,
        visualization,
        warnings,
    })
}

fn memory_guard(ceiling: u64, cancel: &CancelToken) -> MemoryGuard {
    let (soft, hard) = (cancel.clone(), cancel.clone());
    MemoryGuard::spawn(
        ceiling,
        Duration::from_millis(200),
        copper_governor::rss_bytes,
        move |_| soft.cancel_auto_router(),
        move |_| hard.cancel(),
    )
}

fn memory_warning(guard: &MemoryGuard) -> Option<String> {
    let peak = guard.peak_bytes() >> 20;
    match guard.pressure() {
        MemoryPressure::None => None,
        MemoryPressure::Soft => Some(format!(
            "routing stopped early at the memory ceiling (peak {peak} MiB); the session holds \
             what was routed so far"
        )),
        MemoryPressure::Hard => Some(format!(
            "the job was cancelled past the hard memory ceiling (peak {peak} MiB)"
        )),
    }
}

fn attach_output(job: &mut RoutingJob, session: &[u8], format: OutputFormat) {
    let mut output = job.output.take().unwrap_or_default();
    if output.get_filename().is_empty() {
        let base = job.get_input().map_or_else(
            || job.name.clone(),
            BoardFileDetails::get_filename_without_extension,
        );
        output.filename = format!("{base}.{}", format.extension());
    }
    output.format = format.file_format();
    output.set_data(session.to_vec(), format.file_format());
    job.output = Some(output);
}
