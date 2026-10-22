use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use copper_core::{
    CancelToken, Ctx, Deadline, JobStopReason, RoutingPipeline, StopRequestState, SyncProgressSink,
};
use copper_dsn::{BoardReadResult, DsnReadOptions};
use copper_settings::sources::{CliSettings, DsnFileSettings, EnvironmentVariablesSource};
use copper_settings::{HostEnvironment, SettingsInputs, SettingsSource, resolve_headless};

const DSN: &str = "fixtures/Issue143-rpi_splitter.dsn";
const MAX_PASSES: i32 = 8;

#[test]
fn cancel_all_and_cancel_auto_router_are_distinct() {
    let all = CancelToken::new();
    all.cancel();
    assert_eq!(all.as_router_stop().state(), StopRequestState::All);

    let auto = CancelToken::new();
    auto.cancel_auto_router();
    assert_eq!(
        auto.as_router_stop().state(),
        StopRequestState::AutoRouterOnly
    );

    let auto_stop = auto.as_router_stop();
    assert!(
        auto_stop.is_stop_auto_router_requested(),
        "AutorouteBatchLoop.java:250 stops the router"
    );
    assert!(
        !auto_stop.is_stop_requested(),
        "RoutingPipeline.java:117 still runs the optimizer — quirk #202"
    );

    let both = CancelToken::new();
    both.cancel();
    both.cancel_auto_router();
    assert_eq!(both.as_router_stop().state(), StopRequestState::All);
}

#[test]
fn an_uncancelled_token_is_a_no_op() {
    let token = CancelToken::new();
    let stop = token.as_router_stop();
    assert_eq!(stop.state(), StopRequestState::None);
    assert!(!stop.is_timed_out());
    assert!(
        !stop.poll_deadline(),
        "no deadline means poll_deadline is a constant false"
    );

    for _ in 0..1000 {
        token.apply_to(&stop);
    }
    assert_eq!(stop.state(), StopRequestState::None);
    assert!(!stop.is_timed_out());

    token.cancel();
    token.apply_to(&stop);
    assert_eq!(stop.state(), StopRequestState::All);
}

#[test]
fn a_cancel_from_a_second_thread_stops_a_run() {
    let uncancelled = route(&CancelToken::new());
    assert!(
        uncancelled.passes_run > 0,
        "the uncancelled control routed no passes at all, so it proves nothing"
    );

    let token = CancelToken::new();
    let raiser = token.clone();
    std::thread::spawn(move || raiser.cancel())
        .join()
        .expect("the cancelling thread panicked");
    assert!(
        token.is_cancelled(),
        "the flag must be visible on this thread — it is one Arc<AtomicBool>, not two"
    );

    let cancelled = route(&token);
    assert_eq!(cancelled.stop_reason, Some(JobStopReason::Cancelled));
    assert_eq!(
        cancelled.passes_run, 0,
        "a run entered with the stop already at ALL never starts the routing stage \
         (RoutingPipeline.java:97)"
    );
    assert!(
        cancelled.passes_run < uncancelled.passes_run,
        "the cancel must reduce the work done"
    );
    assert!(
        cancelled.incompletes >= uncancelled.incompletes,
        "a cancelled run cannot have routed more connections than the full one"
    );
}

#[test]
fn an_expired_deadline_reaches_the_router_stop() {
    let expired = CancelToken::with_deadline(Deadline::in_seconds(-1));
    let stop = expired.as_router_stop();
    assert!(
        stop.poll_deadline(),
        "an expired job deadline must trip on the first poll"
    );
    assert_eq!(stop.state(), StopRequestState::All);
    assert!(stop.is_timed_out());

    let live = CancelToken::with_timeout(std::time::Duration::from_secs(3600));
    let live_stop = live.as_router_stop();
    assert!(!live_stop.poll_deadline());
    assert_eq!(live_stop.state(), StopRequestState::None);
}

#[test]
fn an_observed_job_deadline_is_the_routing_stop_reason() {
    let result = route(&CancelToken::with_deadline(Deadline::in_seconds(-1)));
    assert_eq!(result.stop_reason, Some(JobStopReason::Deadline));
    assert!(result.timed_out);
}

#[test]
fn a_sync_sink_sees_the_runs_events() {
    let seen = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&seen);
    let sink = SyncProgressSink::new(move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    route_with(&CancelToken::new(), &sink);
    assert!(
        seen.load(Ordering::SeqCst) > 0,
        "the pipeline fired no events through a SyncProgressSink"
    );
}

struct Run {
    passes_run: i32,
    incompletes: Option<i32>,
    stop_reason: Option<JobStopReason>,
    timed_out: bool,
}

fn route(token: &CancelToken) -> Run {
    route_with(token, &SyncProgressSink::noop())
}

fn route_with(token: &CancelToken, sink: &SyncProgressSink) -> Run {
    let dsn = testkit::corpus_dir().join(DSN);
    let bytes =
        std::fs::read(&dsn).unwrap_or_else(|e| panic!("cannot read {}: {e}", dsn.display()));
    let file_name = dsn
        .file_name()
        .expect("a file name")
        .to_string_lossy()
        .into_owned();

    let file = std::fs::File::open(&dsn).unwrap_or_else(|e| panic!("cannot open {dsn:?}: {e}"));
    let mut board =
        match copper_dsn::read_board(file, None, Some(&file_name), &DsnReadOptions::default()) {
            BoardReadResult::Success { board, .. }
            | BoardReadResult::OutlineMissing { board, .. } => {
                *board.expect("the stem produces a board")
            }
            other => panic!("{file_name} did not read: {other:?}"),
        };

    let argv = vec![
        "-de".to_string(),
        dsn.display().to_string(),
        "-do".to_string(),
        dsn.display().to_string().replace(".dsn", ".ses"),
        "-mp".to_string(),
        MAX_PASSES.to_string(),
        "--router.fanout.enabled=true".to_string(),
        "--router.optimizer.enabled=true".to_string(),
    ];
    let dsn_source = DsnFileSettings::new(&bytes[..], &file_name);
    let env_map: std::collections::BTreeMap<String, String> = std::env::vars().collect();
    let env_source = EnvironmentVariablesSource::new(&env_map);
    let cli_source = CliSettings::new(&argv);
    let inputs = SettingsInputs {
        json_file: None,
        dsn: dsn_source.get_settings(),
        cli_rules: None,
        scheduler_rules: None,
        env: env_source.get_settings(),
        cli: cli_source.get_settings(),
    };
    let settings = resolve_headless(&inputs, Some(&board), &HostEnvironment::detect());
    copper_core::prepare_board(&mut board, &settings);

    let ctx = Ctx {
        settings: &settings,
        cancel: token.clone(),
        progress: sink,
        budget: copper_core::RouterBudget::disabled(),
        backend: &copper_core::MazeBackend,
    };
    let result = RoutingPipeline::run(&mut board, &ctx).expect("the stem has a routable layer");
    Run {
        passes_run: result.pipeline.router_passes_completed,
        incompletes: result.incomplete_count(),
        stop_reason: result.stop_reason,
        timed_out: result.timed_out,
    }
}
