use super::jsonrpc::{INVALID_REQUEST, PARSE_ERROR, Request, Response};
use super::server::{ProgressWriter, SharedWriter, State, handle, write_line};
use copper_core::CancelToken;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

pub fn run(overrides: crate::ops::SettingsOverrides, memory_ceiling: Option<u64>) -> i32 {
    let mut state = State::with_overrides(overrides);
    state.memory_ceiling = memory_ceiling;
    super::tools::register_all(&mut state);
    run_with(
        state,
        std::io::BufReader::new(std::io::stdin()),
        std::io::stdout(),
    )
}

enum Event {
    Line(String),
    Eof,
    ReadFailed,
    ToolDone(String),
}

pub fn run_with<R: BufRead + Send + 'static, W: Write + Send + 'static>(
    state: State,
    reader: R,
    writer: W,
) -> i32 {
    let state = Arc::new(state);
    let writer: SharedWriter = Arc::new(Mutex::new(writer));
    let (events, inbox) = channel::<Event>();

    let reader_thread = {
        let events = events.clone();
        std::thread::spawn(move || {
            for line in reader.lines() {
                let event = match line {
                    Ok(line) => Event::Line(line),
                    Err(_) => {
                        let _ = events.send(Event::ReadFailed);
                        return;
                    }
                };
                if events.send(event).is_err() {
                    return;
                }
            }
            let _ = events.send(Event::Eof);
        })
    };

    let mut in_flight: HashMap<String, CancelToken> = HashMap::new();
    let mut tool_threads: Vec<JoinHandle<()>> = Vec::new();
    let mut exit_code = 0;
    let mut draining = false;
    let mut reader_ended = false;

    while let Ok(event) = inbox.recv() {
        match event {
            Event::Line(line) => {
                if draining {
                    if in_flight.is_empty() {
                        break;
                    }
                    continue;
                }

                if line.trim().is_empty() {
                    continue;
                }

                let outbound: Option<Response> = match serde_json::from_str::<Request>(&line) {
                    Err(e) => Some(Response::err(
                        Value::Null,
                        PARSE_ERROR,
                        format!("parse error: {e}"),
                    )),
                    Ok(req) => {
                        if req.method == "notifications/cancelled" {
                            cancel_in_flight(&in_flight, req.params.as_ref());
                            continue;
                        }
                        if req.method == "tools/call"
                            && let Some(id) = req.id.clone()
                        {
                            if in_flight.contains_key(&request_key(&id)) {
                                Some(Response::err(id, INVALID_REQUEST, REUSED_ID))
                            } else {
                                let (key, thread) =
                                    spawn_tool_call(req, &state, &writer, &events, &mut in_flight);
                                tool_threads.push(thread);
                                debug_assert!(in_flight.contains_key(&key));
                                continue;
                            }
                        } else {
                            handle(
                                &state,
                                req,
                                &ProgressWriter::disabled(),
                                &CancelToken::new(),
                            )
                        }
                    }
                };

                if let Some(resp) = outbound
                    && write_line(&writer, &resp).is_err()
                {
                    exit_code = 1;
                    draining = true;
                    cancel_every_in_flight(&in_flight);
                }
                if draining && in_flight.is_empty() {
                    break;
                }
            }
            Event::Eof | Event::ReadFailed => {
                reader_ended = true;
                draining = true;
                if matches!(event, Event::ReadFailed) {
                    exit_code = 1;
                    cancel_every_in_flight(&in_flight);
                }
                if in_flight.is_empty() {
                    break;
                }
            }
            Event::ToolDone(key) => {
                in_flight.remove(&key);
                if draining && in_flight.is_empty() {
                    break;
                }
            }
        }
    }

    drop(inbox);
    for thread in tool_threads {
        let _ = thread.join();
    }
    if reader_ended {
        let _ = reader_thread.join();
    } else {
        drop(reader_thread);
    }
    exit_code
}

fn cancel_every_in_flight(in_flight: &HashMap<String, CancelToken>) {
    for token in in_flight.values() {
        token.cancel();
    }
}

const REUSED_ID: &str =
    "request id is already in flight; MCP requires an id to be unique within a session";

fn spawn_tool_call(
    req: Request,
    state: &Arc<State>,
    writer: &SharedWriter,
    events: &Sender<Event>,
    in_flight: &mut HashMap<String, CancelToken>,
) -> (String, JoinHandle<()>) {
    let key = request_key(req.id.as_ref().expect("the caller checked for an id"));
    let cancel = CancelToken::new();
    in_flight.insert(key.clone(), cancel.clone());

    let progress = ProgressWriter::new(
        req.params
            .as_ref()
            .and_then(|p| p.get("_meta"))
            .and_then(|meta| meta.get("progressToken"))
            .cloned(),
        Arc::clone(writer),
    );

    let state = Arc::clone(state);
    let writer = Arc::clone(writer);
    let done = ToolDoneGuard {
        events: events.clone(),
        key: Some(key.clone()),
    };
    let thread = std::thread::spawn(move || {
        let _done = done;
        if let Some(resp) = handle(&state, req, &progress, &cancel) {
            let _ = write_line(&writer, &resp);
        }
    });
    (key, thread)
}

struct ToolDoneGuard {
    events: Sender<Event>,
    key: Option<String>,
}

impl Drop for ToolDoneGuard {
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            let _ = self.events.send(Event::ToolDone(key));
        }
    }
}

fn cancel_in_flight(in_flight: &HashMap<String, CancelToken>, params: Option<&Value>) {
    let Some(id) = params.and_then(|p| p.get("requestId")) else {
        return;
    };
    if let Some(token) = in_flight.get(&request_key(id)) {
        token.cancel();
    }
}

fn request_key(id: &Value) -> String {
    id.to_string()
}
