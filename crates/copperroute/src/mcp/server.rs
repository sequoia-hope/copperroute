use super::jsonrpc::{METHOD_NOT_FOUND, Notification, Request, Response, RpcError};
use copper_core::CancelToken;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub const PROTOCOL_VERSION: &str = "2025-06-18";

pub type SharedWriter = Arc<Mutex<dyn Write + Send>>;

pub type ToolHandler = Box<
    dyn Fn(&State, Value, &ProgressWriter, &CancelToken) -> Result<Value, RpcError> + Send + Sync,
>;

#[derive(Clone)]
pub struct ProgressWriter {
    inner: Option<ProgressTarget>,
}

#[derive(Clone)]
struct ProgressTarget {
    token: Value,
    writer: SharedWriter,
}

impl ProgressWriter {
    pub fn disabled() -> ProgressWriter {
        ProgressWriter { inner: None }
    }

    pub fn new(token: Option<Value>, writer: SharedWriter) -> ProgressWriter {
        ProgressWriter {
            inner: token.map(|token| ProgressTarget { token, writer }),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.inner.is_some()
    }

    pub fn progress(&self, progress: f64, total: Option<f64>, message: Option<&str>) {
        let Some(target) = self.inner.as_ref() else {
            return;
        };
        let mut params = json!({ "progressToken": target.token, "progress": progress });
        if let Some(total) = total {
            params["total"] = json!(total);
        }
        if let Some(message) = message {
            params["message"] = json!(message);
        }
        let _ = write_line(
            &target.writer,
            &Notification::new("notifications/progress", params),
        );
    }
}

pub(super) fn write_line<T: serde::Serialize>(
    writer: &SharedWriter,
    message: &T,
) -> std::io::Result<()> {
    let text = serde_json::to_string(message).expect("an outbound message serializes");
    let Ok(mut guard) = writer.lock() else {
        return Err(std::io::Error::other(
            "the stdout writer's lock is poisoned",
        ));
    };
    guard.write_all(text.as_bytes())?;
    guard.write_all(b"\n")?;
    guard.flush()
}

#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

pub struct State {
    tools: BTreeMap<String, (ToolDef, ToolHandler)>,
    pub initialized: AtomicBool,
    pub overrides: crate::ops::SettingsOverrides,
    pub memory_ceiling: Option<u64>,
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

impl State {
    pub fn new() -> Self {
        Self {
            tools: BTreeMap::new(),
            initialized: AtomicBool::new(false),
            overrides: crate::ops::SettingsOverrides::default(),
            memory_ceiling: None,
        }
    }

    pub fn with_overrides(overrides: crate::ops::SettingsOverrides) -> Self {
        Self {
            overrides,
            ..Self::new()
        }
    }

    pub fn register_tool(&mut self, def: ToolDef, handler: ToolHandler) {
        self.tools.insert(def.name.clone(), (def, handler));
    }
}

pub fn handle(
    state: &State,
    req: Request,
    progress: &ProgressWriter,
    cancel: &CancelToken,
) -> Option<Response> {
    let id = req.id.clone()?;
    let params = req.params.unwrap_or(Value::Null);
    let resp = match req.method.as_str() {
        "initialize" => {
            state.initialized.store(true, Ordering::SeqCst);
            Response::ok(
                id,
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "copperroute", "version": env!("CARGO_PKG_VERSION") },
                }),
            )
        }
        "ping" => Response::ok(id, json!({})),
        "tools/list" => {
            let tools: Vec<Value> = state
                .tools
                .values()
                .map(|(d, _)| {
                    json!({
                        "name": d.name, "description": d.description, "inputSchema": d.input_schema,
                    })
                })
                .collect();
            Response::ok(id, json!({ "tools": tools }))
        }
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match state.tools.get(name) {
                None => Response::err(
                    id,
                    super::jsonrpc::INVALID_PARAMS,
                    format!("unknown tool: {name}"),
                ),
                Some((_, h)) => match invoke(state, h, args, progress, cancel) {
                    Ok(Ok(v)) => Response::ok(
                        id,
                        json!({
                            "content": [{ "type": "text", "text": v.to_string() }],
                            "structuredContent": v,
                            "isError": false,
                        }),
                    ),
                    Ok(Err(e)) => Response::ok(
                        id,
                        json!({
                            "content": [{ "type": "text", "text": e.message }],
                            "isError": true,
                        }),
                    ),
                    Err(e) => Response::from_error(id, e),
                },
            }
        }
        m => Response::err(id, METHOD_NOT_FOUND, format!("method not found: {m}")),
    };
    Some(resp)
}

/// line never happened. `std::panic::catch_unwind` is a safe function, so `#![forbid(unsafe_code)]`
fn invoke(
    state: &State,
    handler: &ToolHandler,
    args: Value,
    progress: &ProgressWriter,
    cancel: &CancelToken,
) -> Result<Result<Value, RpcError>, RpcError> {
    std::panic::catch_unwind(AssertUnwindSafe(|| handler(state, args, progress, cancel))).map_err(
        |payload| {
            RpcError::internal(format!(
                "tool panicked: {}",
                panic_message(payload.as_ref())
            ))
        },
    )
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "a panic payload of an unknown type".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn req(id: i64, method: &str, params: serde_json::Value) -> Request {
        Request {
            jsonrpc: "2.0".into(),
            id: Some(json!(id)),
            method: method.into(),
            params: Some(params),
        }
    }

    fn answer(state: &State, req: Request) -> Option<Response> {
        handle(state, req, &ProgressWriter::disabled(), &CancelToken::new())
    }

    fn echo_tool() -> (ToolDef, ToolHandler) {
        (
            ToolDef {
                name: "echo".into(),
                description: "echo".into(),
                input_schema: json!({"type": "object"}),
            },
            Box::new(|_state, args, _progress, _cancel| Ok(args)),
        )
    }

    #[test]
    fn initialize_returns_server_info_and_tools_capability() {
        let st = State::new();
        let resp = answer(
            &st,
            req(
                1,
                "initialize",
                json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}),
            ),
        )
        .unwrap();
        let r = resp.result.unwrap();
        assert_eq!(r["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(r["serverInfo"]["name"], "copperroute");
        assert!(r["capabilities"]["tools"].is_object());
        assert!(r.get("serverName").is_none() && r.get("serverVersion").is_none());
    }

    #[test]
    fn ping_returns_empty_object() {
        let st = State::new();
        let resp = answer(&st, req(2, "ping", json!({}))).unwrap();
        assert_eq!(resp.result.unwrap(), json!({}));
    }

    #[test]
    fn notifications_get_no_response() {
        let st = State::new();
        let n = Request {
            jsonrpc: "2.0".into(),
            id: None,
            method: "notifications/initialized".into(),
            params: None,
        };
        assert!(answer(&st, n).is_none());
    }

    #[test]
    fn tools_list_is_empty_by_default_and_lists_registered() {
        let mut st = State::new();
        let resp = answer(&st, req(3, "tools/list", json!({}))).unwrap();
        assert_eq!(resp.result.unwrap()["tools"], json!([]));
        let (def, handler) = echo_tool();
        st.register_tool(def, handler);
        let resp = answer(&st, req(4, "tools/list", json!({}))).unwrap();
        assert_eq!(resp.result.unwrap()["tools"][0]["name"], "echo");
    }

    #[test]
    fn tools_call_dispatches_and_wraps_content() {
        let mut st = State::new();
        let (def, handler) = echo_tool();
        st.register_tool(def, handler);
        let resp = answer(
            &st,
            req(
                5,
                "tools/call",
                json!({"name": "echo", "arguments": {"a": 1}}),
            ),
        )
        .unwrap();
        let r = resp.result.unwrap();
        assert_eq!(r["isError"], false);
        assert_eq!(r["structuredContent"], json!({"a": 1}));
        assert_eq!(r["content"][0]["type"], "text");
    }

    #[test]
    fn unknown_method_and_unknown_tool_error() {
        let st = State::new();
        let resp = answer(&st, req(6, "nope", json!({}))).unwrap();
        assert_eq!(resp.error.unwrap().code, -32601);
        let resp = answer(
            &st,
            req(7, "tools/call", json!({"name": "missing", "arguments": {}})),
        )
        .unwrap();
        assert_eq!(resp.error.unwrap().code, -32602);
    }

    #[test]
    fn a_tool_failure_is_an_is_error_result_and_a_tool_panic_is_a_minus_32603() {
        let mut st = State::new();
        st.register_tool(
            ToolDef {
                name: "fails".into(),
                description: "fails".into(),
                input_schema: json!({"type": "object"}),
            },
            Box::new(|_, _, _, _| Err(RpcError::invalid_params("no good"))),
        );
        st.register_tool(
            ToolDef {
                name: "panics".into(),
                description: "panics".into(),
                input_schema: json!({"type": "object"}),
            },
            Box::new(|_, _, _, _| panic!("boom")),
        );

        let failed = answer(&st, req(8, "tools/call", json!({"name": "fails"}))).unwrap();
        let r = failed.result.unwrap();
        assert_eq!(r["isError"], true);
        assert_eq!(r["content"][0]["text"], "no good");
        assert!(r.get("structuredContent").is_none());

        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let panicked = answer(&st, req(9, "tools/call", json!({"name": "panics"}))).unwrap();
        std::panic::set_hook(hook);
        assert!(panicked.result.is_none());
        let e = panicked.error.unwrap();
        assert_eq!(e.code, super::super::jsonrpc::INTERNAL_ERROR);
        assert!(e.message.contains("boom"), "{}", e.message);
    }

    #[test]
    fn a_progress_writer_with_no_token_drops_everything() {
        let sink: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let disabled = ProgressWriter::disabled();
        assert!(!disabled.is_enabled());
        disabled.progress(1.0, Some(2.0), Some("ignored"));

        let enabled = ProgressWriter::new(Some(json!("tok")), shared(Arc::clone(&sink)));
        assert!(enabled.is_enabled());
        enabled.progress(1.0, None, None);
        enabled.progress(2.0, Some(5.0), Some("half"));

        let text = String::from_utf8(sink.lock().unwrap().clone()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        let first: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["method"], "notifications/progress");
        assert_eq!(first["params"]["progressToken"], "tok");
        assert_eq!(first["params"]["progress"], 1.0);
        assert!(first["params"].get("total").is_none());
        assert!(first["params"].get("message").is_none());
        assert!(first.get("id").is_none());
        let second: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["params"]["total"], 5.0);
        assert_eq!(second["params"]["message"], "half");
    }

    fn shared(buffer: Arc<Mutex<Vec<u8>>>) -> SharedWriter {
        struct Tee(Arc<Mutex<Vec<u8>>>);
        impl Write for Tee {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        Arc::new(Mutex::new(Tee(buffer)))
    }
}
