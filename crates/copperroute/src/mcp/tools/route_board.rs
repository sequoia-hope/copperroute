use super::super::jsonrpc::RpcError;
use super::super::server::{ProgressWriter, State};
use crate::ops::SettingsOverrides;
use crate::ops::load::LoadRequest;
use crate::ops::route::{OutputTarget, RouteRequest, route};
use copper_core::{CancelToken, SyncProgressSink};
use copper_settings::RouterSettings;
use serde_json::{Value, json};
use std::path::PathBuf;

pub fn run(
    state: &State,
    args: Value,
    progress: &ProgressWriter,
    cancel: &CancelToken,
) -> Result<Value, RpcError> {
    let ses = super::optional_string(&args, "ses_path")?.map(PathBuf::from);
    let rules = super::optional_string(&args, "rules_path")?.map(PathBuf::from);
    let output_path = super::optional_string(&args, "output_path")?.map(PathBuf::from);
    let payload = settings_payload(&args)?;

    let mut load = LoadRequest::for_board(super::board_source(&args)?);
    load.rules = rules;
    load.discover_adjacent_rules = true;
    load.session = ses;
    load.settings = SettingsOverrides {
        sparse: payload,
        ..state.overrides.clone()
    };

    let mut outcome = route(RouteRequest {
        load,
        output: OutputTarget::Session,
        cancel: cancel.clone(),
        progress: progress_sink(progress),
        visualize: None,
        memory_ceiling: state.memory_ceiling,
        backend: &copper_core::MazeBackend,
        lock: Vec::new(),
        retire_fixed: Vec::new(),
    })
    .map_err(super::rpc_error)?;
    outcome.job.id = super::mint_job_id();

    let output = outcome
        .job
        .output
        .as_ref()
        .ok_or_else(|| RpcError::internal("the router produced no session document"))?;
    let mut answer = super::file_payload_fields(output);
    let object = answer.as_object_mut().expect("a JSON object");
    object.insert("job_id".into(), json!(outcome.job.id.to_java_string()));
    match output_path.as_deref() {
        Some(path) => {
            std::fs::write(path, &outcome.session).map_err(|error| {
                RpcError::internal(format!(
                    "Couldn't save the output file '{}': {error}",
                    path.display()
                ))
            })?;
            object.insert("ses_path".into(), json!(path.display().to_string()));
        }
        None => {
            object.insert(
                "ses_text".into(),
                json!(String::from_utf8_lossy(&outcome.session).into_owned()),
            );
            object.insert("data".into(), json!(super::base64_encode(&outcome.session)));
        }
    }
    object.insert(
        "stats".into(),
        copper_core::to_gson_json(&outcome.result.stats),
    );
    object.insert(
        "incompletes".into(),
        json!(outcome.result.incomplete_count()),
    );
    object.insert(
        "unrouted_report".into(),
        json!(outcome.result.unrouted_report),
    );
    object.insert(
        "drc_violation_count".into(),
        json!(outcome.result.violation_count()),
    );
    object.insert("timed_out".into(), json!(outcome.result.timed_out));
    Ok(answer)
}

fn settings_payload(args: &Value) -> Result<Option<RouterSettings>, RpcError> {
    let settings = match args.get("settings") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Object(map)) => Value::Object(map.clone()),
        Some(_) => {
            return Err(RpcError::invalid_params(
                "settings must be an object; call list_settings for the field names",
            ));
        }
    };
    let text = settings.to_string();
    RouterSettings::from_json_str(&text)
        .map(Some)
        .map_err(|error| {
            RpcError::invalid_params(format!(
                "settings is not a valid RouterSettings object: {error}"
            ))
        })
}

/// Three of the five event variants are reported. `BoardUpdated` fires on every throttled
/// board update and `OptimizerImproved` fires per item, which on a real board is thousands
/// of notifications down a pipe an agent reads one at a time; the stage ladder is what a
/// client can use. `progress` is a monotonic counter, because the number of passes a run
/// will take is not known until it has taken them.
fn progress_sink(progress: &ProgressWriter) -> SyncProgressSink {
    if !progress.is_enabled() {
        return SyncProgressSink::noop();
    }
    let progress = progress.clone();
    let mut step = 0.0f64;
    SyncProgressSink::new(move |event| {
        use copper_core::RoutingEvent;
        let (message, total) = match event {
            RoutingEvent::TaskStateChanged { algorithm, state } => {
                (format!("{algorithm:?}: {state:?}"), None)
            }
            RoutingEvent::FanoutProgress {
                pass,
                routed,
                pins_to_go,
            } => (
                format!("fanout pass #{pass}: {routed} routed, {pins_to_go} to go"),
                Some(f64::from(routed + pins_to_go)),
            ),
            RoutingEvent::BoardSnapshot { pass } => (format!("pass #{pass} complete"), None),
            RoutingEvent::BoardUpdated { .. } | RoutingEvent::OptimizerImproved { .. } => return,
        };
        step += 1.0;
        progress.progress(step, total, Some(&message));
    })
}
