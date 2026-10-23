#![forbid(unsafe_code)]

use std::path::PathBuf;

use copperroute::ops::load::{BoardSource, LoadRequest};
use copperroute::ops::route::{OutputFormat, OutputTarget, RouteRequest, budget_for, route};
use copperroute::ops::{OpError, SettingsOverrides};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("fr-ops-route").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    dir
}

fn request(dsn: PathBuf, output: OutputTarget, set: Vec<String>) -> RouteRequest {
    let mut load = LoadRequest::for_board(BoardSource::Path(dsn));
    load.discover_adjacent_rules = true;
    load.settings = SettingsOverrides {
        settings_file: None,
        set,
        sparse: None,
    };
    RouteRequest {
        load,
        output,
        cancel: copper_core::CancelToken::new(),
        progress: copper_core::SyncProgressSink::noop(),
        visualize: None,
        memory_ceiling: None,
        backend: &copper_core::MazeBackend,
        lock: Vec::new(),
    }
}

#[test]
fn output_format_is_the_extension() {
    assert_eq!(
        OutputFormat::from_path("a/b.ses".as_ref()),
        Some(OutputFormat::Ses)
    );
    assert_eq!(
        OutputFormat::from_path("a/b.SES".as_ref()),
        Some(OutputFormat::Ses)
    );
    assert_eq!(
        OutputFormat::from_path("b.json".as_ref()),
        Some(OutputFormat::KicadSessionJson)
    );
    assert_eq!(OutputFormat::from_path("b.dsn".as_ref()), None);
    assert_eq!(OutputFormat::from_path("b".as_ref()), None);
}

#[test]
fn the_budget_is_the_default_with_the_settings_knob() {
    let mut settings = copper_settings::RouterSettings::new();
    assert_eq!(budget_for(&settings).opt_changed_area_ms, 0);
    settings.opt_changed_area_ms = Some(250);
    assert_eq!(budget_for(&settings).opt_changed_area_ms, 250);
}

#[test]
fn a_routed_board_answers_a_session_and_its_stats() {
    let dir = scratch("session");
    let outcome = route(request(
        testkit::fixture("Issue143-rpi_splitter.dsn"),
        OutputTarget::File(dir.join("out.ses")),
        vec!["router.max_passes=1".to_string()],
    ))
    .unwrap();
    assert_eq!(outcome.format, OutputFormat::Ses);
    assert_eq!(outcome.state, copper_core::RoutingJobState::Completed);
    assert!(String::from_utf8_lossy(&outcome.session).starts_with("(session"));
    assert!(outcome.result.stats.connections.incomplete_count.is_some());
    assert_eq!(outcome.job.get_current_pass(), 1);
    assert!(
        !dir.join("out.ses").exists(),
        "the operation writes nothing; the adapter does"
    );
}

#[test]
fn a_json_output_carries_the_routed_board() {
    let dir = scratch("json");
    let outcome = route(request(
        testkit::fixture("Issue143-rpi_splitter.dsn"),
        OutputTarget::File(dir.join("out.json")),
        vec!["router.max_passes=1".to_string()],
    ))
    .unwrap();
    assert_eq!(outcome.format, OutputFormat::KicadSessionJson);
    let text = String::from_utf8_lossy(&outcome.session);
    assert!(text.contains("\"traces\""));
    assert!(!text.contains("\"traces\": []"));
}

#[test]
fn a_zero_job_timeout_reports_timed_out() {
    let outcome = route(request(
        testkit::fixture("Issue143-rpi_splitter.dsn"),
        OutputTarget::Session,
        vec![
            "router.max_passes=1".to_string(),
            "router.job_timeout=0:00:00".to_string(),
        ],
    ))
    .unwrap();
    assert_eq!(outcome.state, copper_core::RoutingJobState::TimedOut);
    assert!(outcome.result.timed_out);
}

#[test]
fn a_bad_timeout_is_a_settings_error() {
    let error = match route(request(
        testkit::fixture("Issue143-rpi_splitter.dsn"),
        OutputTarget::Session,
        vec!["router.job_timeout=banana".to_string()],
    )) {
        Err(error) => error,
        Ok(_) => panic!("a bad timeout must be refused"),
    };
    assert!(matches!(error, OpError::Settings(_)), "{error}");
}
