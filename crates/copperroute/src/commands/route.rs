use std::path::Path;

use copper_core::{RoutingJobState, RoutingResultManifest, SyncProgressSink};

use crate::ExitCode;
use crate::cli::{Cli, RouteArgs};
use crate::ops::load::{BoardSource, LoadRequest};
use crate::ops::route::{OutputTarget, RouteOutcome, RouteRequest, route};

pub fn run(cli: &Cli, args: &RouteArgs, memory_ceiling: Option<u64>) -> ExitCode {
    let mut set = args.set.clone();
    if let Some(passes) = args.max_passes {
        set.push(format!("router.max_passes={passes}"));
    }
    if let Some(timeout) = args.timeout.as_deref() {
        set.push(format!("router.job_timeout={timeout}"));
    }

    let mut load = LoadRequest::for_board(BoardSource::Path(args.input.clone()));
    load.rules = args.rules.clone();
    load.discover_adjacent_rules = true;
    load.session = args.ses.clone();
    load.kicad_project = args.kicad_project.clone();
    load.settings = super::overrides(cli, &set);

    let visualize = args.visualize.as_ref().map(|dir| {
        let mut options = copper_router::RoutingVisualizationOptions::new(dir.clone());
        options.every = args.visualize_every;
        options.max_frames = args.visualize_max_frames;
        options.width = args.visualize_width;
        options.height = args.visualize_height;
        options
    });

    let _recording = match &args.morph_frames {
        Some(dir) => match super::frames::record(dir, 2000, String::new()) {
            Ok(recording) => Some(recording),
            Err(error) => {
                tracing::error!("Couldn't record frames in {}: {error}", dir.display());
                return ExitCode::Failure;
            }
        },
        None => None,
    };
    let cancel = copper_core::CancelToken::new();
    let watching = match args.live.as_deref() {
        Some(path) => match live_output(path, &cancel) {
            Ok(out) => Some(super::live::start(out, args.live_fps)),
            Err(error) => {
                tracing::error!("Couldn't open --live {}: {error}", path.display());
                return ExitCode::Failure;
            }
        },
        None => None,
    };
    let progress = match &watching {
        Some(watching) => {
            let stream = watching.stream.clone();
            SyncProgressSink::new(move |event| stream.event(serde_json::json!(format!("{event:?}"))))
        }
        None => SyncProgressSink::noop(),
    };
    let outcome = match route(RouteRequest {
        load,
        output: OutputTarget::File(args.output.clone()),
        cancel,
        progress,
        visualize,
        memory_ceiling,
        backend: args.backend,
        lock: args.lock.clone(),
        retire_fixed: args.retire_fixed.clone(),
        place: args.place.clone(),
        live: watching.as_ref().map(|w| w.stream.clone()),
    }) {
        Ok(outcome) => outcome,
        Err(error) => {
            tracing::error!("{error}");
            return ExitCode::Failure;
        }
    };

    for warning in &outcome.warnings {
        tracing::warn!("{warning}");
    }

    if let Some(summary) = outcome.visualization.as_ref() {
        if let Some(error) = summary.error.as_ref() {
            tracing::warn!("routing visualization stopped after an I/O error: {error}");
        }
        eprintln!(
            "Visualization: {} SVG frames from {} routing steps in {}{}",
            summary.frames_written,
            summary.observed_steps,
            summary.output_dir.display(),
            if summary.reached_frame_limit {
                " (frame limit reached)"
            } else {
                ""
            }
        );
    }

    let written = write_session(&outcome, &args.output);
    let exit_code = if written
        && matches!(
            outcome.state,
            RoutingJobState::Completed | RoutingJobState::TimedOut
        ) {
        ExitCode::Ok
    } else {
        ExitCode::Failure
    };
    write_manifest(args, &outcome, written, exit_code);
    exit_code
}

fn live_output(path: &Path, cancel: &copper_core::CancelToken) -> std::io::Result<Box<dyn std::io::Write + Send>> {
    if path != Path::new("-") {
        return Ok(Box::new(std::io::BufWriter::new(std::fs::File::create(path)?)));
    }
    let cancel = cancel.clone();
    std::thread::spawn(move || {
        for line in std::io::stdin().lines() {
            match line {
                Ok(line) if line.trim() == "stop" => cancel.cancel(),
                Ok(_) => {}
                Err(_) => return,
            }
        }
    });
    Ok(Box::new(std::io::stdout()))
}

fn write_session(outcome: &RouteOutcome, path: &Path) -> bool {
    if !matches!(
        outcome.state,
        RoutingJobState::Completed | RoutingJobState::TimedOut
    ) {
        return false;
    }
    if let Err(error) = std::fs::write(path, &outcome.session) {
        tracing::error!(
            "Couldn't save the output file '{}': {error}",
            path.display()
        );
        return false;
    }
    if let Err(error) = crate::ops::fixed_wiring::write_sidecar(path, &outcome.superseded) {
        tracing::error!("Couldn't save the superseded wiring beside '{}': {error}", path.display());
        return false;
    }
    !outcome.session.is_empty()
}

fn write_manifest(args: &RouteArgs, outcome: &RouteOutcome, written: bool, exit_code: ExitCode) {
    let from_settings = outcome
        .job
        .router_settings
        .result_json_path
        .as_deref()
        .map(str::to_string)
        .filter(|path| !path.trim().is_empty());
    let Some(path) = from_settings.or_else(|| {
        args.result_json
            .as_deref()
            .map(|path| path.to_string_lossy().into_owned())
    }) else {
        return;
    };
    let mut manifest = RoutingResultManifest::from_job(
        &outcome.job,
        Some(&args.input),
        written,
        exit_code.code(),
        &copper_core::now_utc_iso8601,
        Some(&outcome.result.stats),
    );
    manifest.backend = Some(args.backend.name().to_string());
    if let Err(error) = RoutingResultManifest::write(Path::new(&path), &manifest) {
        tracing::error!("Couldn't write routing result manifest to '{path}': {error}");
    }
}
