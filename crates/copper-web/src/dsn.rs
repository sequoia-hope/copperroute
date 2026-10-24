use copper_board::Board;
use copper_settings::SettingsSource;
use wasm_bindgen::prelude::*;

fn load(text: &str, name: &str) -> Result<copper_core::ParsedBoard, String> {
    copper_core::parse_board_result(copper_dsn::read_board(
        text.as_bytes(),
        None,
        Some(name),
        &copper_dsn::DsnReadOptions::default(),
    ))
    .map_err(|e| e.to_string())
}

#[wasm_bindgen]
pub fn preview_dsn(text: &str, name: &str) -> Result<String, JsValue> {
    console_error_panic_hook::set_once();
    let loaded = load(text, name).map_err(|e| JsValue::from_str(&e))?;
    Ok(serde_json::json!({"svg": copper_core::preview::board_svg(&loaded.board, &[], ""), "layers": layers(&loaded.board), "warnings": loaded.warnings}).to_string())
}

#[wasm_bindgen]
pub fn route_dsn(
    text: &str,
    name: &str,
    passes: u32,
    seconds: u32,
    callback: js_sys::Function,
) -> Result<String, JsValue> {
    route(text, name, passes, seconds, callback).map_err(|e| JsValue::from_str(&e))
}

fn route(
    text: &str,
    name: &str,
    passes: u32,
    seconds: u32,
    callback: js_sys::Function,
) -> Result<String, String> {
    console_error_panic_hook::set_once();
    if !(1..=100).contains(&passes) || !(1..=600).contains(&seconds) {
        return Err("Use 1–100 passes and a 1–600 second time limit".into());
    }
    let mut loaded = load(text, name)?;
    if !loaded.warnings.is_empty() {
        return Err(loaded.warnings.join("\n"));
    }
    let source = copper_settings::sources::DsnFileSettings::new(text.as_bytes(), name);
    let mut settings = copper_settings::resolve_headless(
        &copper_settings::SettingsInputs {
            dsn: source.get_settings(),
            ..Default::default()
        },
        Some(&loaded.board),
        &copper_settings::HostEnvironment::with_processors(1),
    );
    settings.set_run_router(true);
    settings.set_run_optimizer(false);
    settings.set_max_passes(Some(passes as i32));
    // Match the PCB route-from-scratch workflow, including protected routing.
    let routing = loaded
        .board
        .get_traces()
        .into_iter()
        .chain(loaded.board.get_vias())
        .collect::<Vec<_>>();
    for id in &routing {
        if let Some(item) = loaded.board.get_item_mut(*id) {
            item.set_fixed_state(copper_board::FixedState::Unfixed);
        }
    }
    if !loaded.board.remove_items(routing) {
        return Err("Could not remove all existing routing".into());
    }
    let mut observer = DsnProgress(callback);
    observer.send(serde_json::json!({"type":"preview", "svg":copper_core::preview::board_svg(&loaded.board, &[], ""), "warnings":[]}));
    let sink = copper_core::SyncProgressSink::noop();
    let mut ctx = copper_core::Ctx::new(&settings, &sink);
    ctx.cancel = ctx
        .cancel
        .with_deadline_from(copper_core::Deadline::in_seconds(seconds as i64));
    let result =
        copper_core::RoutingPipeline::run_with_progress(&mut loaded.board, &ctx, &mut observer)
            .map_err(|e| e.to_string())?;
    let mut dsn = Vec::new();
    copper_dsn::dsn_writer::write_with_settings(
        &loaded.board,
        &loaded.transform,
        &mut dsn,
        name,
        false,
        Some(&copper_dsn::parser::DsnRouterSettings::from(&settings)),
    )
    .map_err(|e| e.to_string())?;
    let mut ses = Vec::new();
    copper_dsn::ses_writer::write(&loaded.board, &loaded.transform, &mut ses, name)
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "svg":copper_core::preview::board_svg(&loaded.board, &[], ""), "dsn":String::from_utf8(dsn).map_err(|e|e.to_string())?,
        "ses":String::from_utf8(ses).map_err(|e|e.to_string())?,
        "incomplete":result.incomplete_count(), "violations":result.violation_count(),
        "passes":result.pipeline.router_passes_completed, "timedOut":result.timed_out,
        "warnings":["DSN rules and layer settings are imported. Import the SES into the source PCB editor and run its DRC; source-editor metadata is not part of DSN."]
    }).to_string())
}
struct DsnProgress(js_sys::Function);
impl DsnProgress {
    fn send(&mut self, value: serde_json::Value) {
        let _ = self
            .0
            .call1(&JsValue::NULL, &JsValue::from_str(&value.to_string()));
    }
}
impl copper_core::ProgressSink for DsnProgress {
    fn on_board_update(&mut self, board: &Board, event: &copper_core::RoutingEvent) {
        if let copper_core::RoutingEvent::BoardUpdated { counters } = event {
            self.send(serde_json::json!({"type":"progress", "svg":copper_core::preview::board_svg(board, &[], ""), "pass":counters.pass_count, "incomplete":counters.incomplete_count,"routed":counters.routed_count}));
        }
    }
}
fn layers(board: &Board) -> Vec<serde_json::Value> {
    board.layer_structure().layers.iter().enumerate().map(|(i,l)| serde_json::json!({"name":l.name,"label":l.name,"type":if l.is_signal {"signal"} else {"power"},"color":copper_core::preview::layer_color(board,i)})).collect()
}
