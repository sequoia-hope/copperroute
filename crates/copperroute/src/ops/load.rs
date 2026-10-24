use std::path::{Path, PathBuf};

use copper_board::Board;
use copper_core::{FileFormat, RoutingJob, SessionId};
use copper_dsn::{BoardMetadata, CoordinateTransform};
use copper_settings::sources::DsnFileSettings;
use copper_settings::{HostEnvironment, RouterSettings, SettingsSource};

use super::OpError;
use super::settings::{self, SettingsOverrides};

#[derive(Debug, Clone)]
pub enum BoardSource {
    Path(PathBuf),
    Text { text: String, name: String },
}

#[derive(Debug, Clone)]
pub struct LoadRequest {
    pub source: BoardSource,
    pub rules: Option<PathBuf>,
    pub discover_adjacent_rules: bool,
    pub session: Option<PathBuf>,
    pub kicad_project: Option<PathBuf>,
    pub settings: SettingsOverrides,
}

impl LoadRequest {
    pub fn for_board(source: BoardSource) -> LoadRequest {
        LoadRequest {
            source,
            rules: None,
            discover_adjacent_rules: false,
            session: None,
            kicad_project: None,
            settings: SettingsOverrides::default(),
        }
    }
}

#[derive(Debug)]
pub struct Loaded {
    pub job: RoutingJob,
    pub board: Board,
    pub transform: CoordinateTransform,
    pub metadata: Option<BoardMetadata>,
    pub settings: RouterSettings,
    pub warnings: Vec<String>,
}

pub fn load(request: &LoadRequest) -> Result<Loaded, OpError> {
    let mut job = RoutingJob::new(SessionId::NIL);
    match &request.source {
        BoardSource::Path(path) => job.set_input(path).map_err(|error| {
            OpError::Input(format!(
                "Couldn't load the input file '{}': {error}",
                path.display()
            ))
        })?,
        BoardSource::Text { text, name } => {
            job.set_input_bytes(Some(text.as_bytes()));
            if let Some(input) = job.input.as_mut() {
                let extension = match input.format.default_extension() {
                    "" => "dsn",
                    extension => extension,
                };
                input.set_filename(Some(&format!("{name}.{extension}")));
            }
            job.name = name.clone();
        }
    }

    let input = job.get_input().expect("the input was just set");
    let format = input.format;
    if !matches!(
        format,
        FileFormat::Dsn | FileFormat::KicadDesignJson | FileFormat::KicadPcb
    ) {
        return Err(OpError::Input(format!(
            "'{}' is not a board: only Specctra DSN, KiCad board JSON and KiCad boards are accepted, got {}",
            input.get_filename(),
            format.name()
        )));
    }
    let dsn_source = DsnFileSettings::new(input.get_data(), input.get_filename());

    if let Some(rules) = request.rules.as_deref() {
        if !rules.exists() {
            tracing::warn!("Rules file {} not found", rules.display());
        } else if let Err(error) = job.set_rules(rules) {
            tracing::warn!("Couldn't load rules file '{}': {error}", rules.display());
        }
    }
    let explicit_rules: Option<Vec<u8>> = job
        .rules
        .as_ref()
        .map(|rules| rules.get_data().to_vec())
        .filter(|data| !data.is_empty());
    let scheduler_rules = if request.discover_adjacent_rules {
        read_scheduler_rules(&job, request.rules.as_deref())
    } else {
        explicit_rules.clone()
    };

    let kicad_pcb_path = (format == FileFormat::KicadPcb)
        .then(|| PathBuf::from(job.get_input().expect("input is set").get_absolute_path()));
    let kicad_project_path = copper_settings::resolve_kicad_project_path(
        request.kicad_project.as_deref(),
        kicad_pcb_path.as_deref(),
    );
    let kicad_project_text = read_kicad_project(
        kicad_project_path.as_deref(),
        request.kicad_project.is_some(),
    );

    let (mut parsed, embedded_net_classes) = if format == FileFormat::KicadPcb {
        parse_kicad_pcb(&job, kicad_project_text.as_deref())?
    } else {
        (copper_core::parse_board_if_needed(&job)?, 0)
    };
    if format == FileFormat::KicadPcb && kicad_project_path.is_none() && embedded_net_classes == 0 {
        let defaults = copper_dsn::kicad::pcb::default_net_class();
        parsed.warnings.push(format!(
            "No KiCad project file found for {}: routing with built-in default design rules \
             ({} mm clearance, {} mm track, {}/{} mm via) instead of the board's own rules. \
             Pass --kicad-project or place a .kicad_pro beside the board.",
            job.get_input().expect("input is set").get_filename(),
            defaults.clearance,
            defaults.traceWidth,
            defaults.viaDiameter,
            defaults.viaDrill,
        ));
    }
    let mut board = parsed.board;
    let transform = parsed.transform;

    let host = HostEnvironment::detect();
    let mut settings = settings::resolve(
        &request.settings,
        dsn_source.get_settings(),
        explicit_rules.as_deref(),
        scheduler_rules.as_deref(),
        Some(&board),
        &host,
    )?;
    copper_core::apply_router_settings_for_loaded_board(&mut board, &mut settings);
    copper_core::apply_immediate_post_load_processing(&mut board);

    if let Some(bytes) = scheduler_rules.as_deref() {
        match copper_dsn::rules_reader::read(bytes, &job.name, &mut board, &transform, None) {
            Ok(_) => tracing::info!(
                "Rules loaded from {}",
                request.rules.as_deref().map_or_else(
                    || "the rules file beside the input".to_string(),
                    |path| path.display().to_string()
                )
            ),
            Err(error) => tracing::error!("Failed to apply rules from rules file: {error}"),
        }
    }
    apply_kicad_project_constraints(kicad_project_text.as_deref(), &mut board, &transform);
    if let Some(path) = std::env::var_os("COPPERROUTE_PAD_CLEARANCE_JSON") {
        let floors: Vec<PadFloor> = serde_json::from_slice(&std::fs::read(&path)?)
            .map_err(|error| OpError::Input(format!("Invalid pad clearance metadata: {error}")))?;
        apply_pad_clearance_metadata(&mut board, &floors)?;
    }
    import_session(request.session.as_deref(), &mut board, &transform);

    job.router_settings = settings.clone();
    job.drc_settings = copper_settings::DesignRulesCheckerSettings::default();
    Ok(Loaded {
        job,
        board,
        transform,
        metadata: parsed.metadata,
        settings,
        warnings: parsed.warnings,
    })
}

fn read_scheduler_rules(job: &RoutingJob, explicit: Option<&Path>) -> Option<Vec<u8>> {
    if let Some(rules) = job.rules.as_ref() {
        let data = rules.get_data();
        if !data.is_empty() {
            return Some(data.to_vec());
        }
    }
    let dsn_path = job.get_input().and_then(|input| {
        (input.format == FileFormat::Dsn).then(|| PathBuf::from(input.get_absolute_path()))
    });
    let path = copper_settings::resolve_scheduler_rules_path(None, explicit, dsn_path.as_deref())?;
    match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) => {
            tracing::warn!("Failed to read rules file {}: {error}", path.display());
            None
        }
    }
}

fn read_kicad_project(project: Option<&Path>, explicit: bool) -> Option<String> {
    let project = project?;
    match std::fs::read_to_string(project) {
        Ok(text) => {
            if explicit {
                tracing::info!("Using KiCad project file {}", project.display());
            } else {
                tracing::info!(
                    "Found KiCad project file {} next to the board; using its design rules",
                    project.display()
                );
            }
            Some(text)
        }
        Err(error) => {
            tracing::warn!("KiCad project file {} not read: {error}", project.display());
            None
        }
    }
}

fn apply_kicad_project_constraints(
    project_text: Option<&str>,
    board: &mut Board,
    transform: &CoordinateTransform,
) {
    let Some(text) = project_text else {
        return;
    };
    match copper_drc::apply_kicad_project(text, board, transform) {
        Ok(()) => tracing::info!("KiCad project design rules applied"),
        Err(error) => tracing::error!("Failed to apply KiCad project design rules: {error}"),
    }
}

/// Net classes live in the `.kicad_pro`, not the `.kicad_pcb` (KiCad 6+), so they must be
/// resolved onto the board's routing rules before the native reader converts the parsed board
/// into a `Board` and falls back to `copper_dsn::kicad::pcb::default_net_class()`.
fn parse_kicad_pcb(
    job: &RoutingJob,
    project_text: Option<&str>,
) -> Result<(copper_core::ParsedBoard, usize), OpError> {
    let input = job.get_input().expect("caller has already set the input");
    let data = input.get_data().to_vec();
    let name = input.get_filename_without_extension();
    let text = String::from_utf8_lossy(&data).into_owned();
    let mut imported =
        copper_dsn::kicad::read_pcb(&text, &name, &copper_dsn::kicad::pcb::default_net_class())
            .map_err(|error| OpError::Input(error.to_string()))?;
    let embedded_net_classes = imported.embedded_net_classes;
    if let Some(project_text) = project_text {
        match copper_dsn::kicad::apply_net_classes(&mut imported.board, project_text) {
            Ok(()) => tracing::info!("KiCad project net classes applied"),
            Err(error) => {
                tracing::error!("Failed to apply KiCad project net classes: {error}");
                imported.warnings.push(format!(
                    "KiCad project net classes could not be applied, so the board routed with \
                     default net-class rules: {error}"
                ));
            }
        }
    }
    let mut parsed =
        copper_core::parse_board_result(copper_dsn::kicad::read_board_json(imported.board, None))?;
    parsed.warnings.extend(imported.warnings);
    Ok((parsed, embedded_net_classes))
}

fn import_session(session: Option<&Path>, board: &mut Board, transform: &CoordinateTransform) {
    let Some(session) = session else {
        return;
    };
    let bytes = match std::fs::read(session) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!("Session file {} not read: {error}", session.display());
            return;
        }
    };
    if session.to_string_lossy().to_lowercase().ends_with(".json") {
        match copper_dsn::kicad::import_session(&String::from_utf8_lossy(&bytes), board) {
            Ok(()) => tracing::info!("KiCad JSON session loaded from {}", session.display()),
            Err(error) => tracing::error!("Failed to load session file: {error}"),
        }
        return;
    }
    match super::fixed_wiring::apply_sidecar(session, board) {
        Ok(0) => {}
        Ok(n) => tracing::info!("{n} fixed items superseded by {}", session.display()),
        Err(error) => tracing::error!("{error}"),
    }
    match copper_dsn::ses_reader::read(&bytes[..], board, transform) {
        Ok(summary) => tracing::info!(
            "Session loaded from {}: {} components moved, {} wires, {} vias imported, {} errors",
            session.display(),
            summary.components_moved,
            summary.wires_imported,
            summary.vias_imported,
            summary.errors_encountered
        ),
        Err(error) => tracing::error!("Failed to load session file: {error}"),
    }
}

#[derive(serde::Deserialize)]
struct PadFloor {
    component: String,
    pad: String,
    x_um: f64,
    y_um: f64,
    front_um: f64,
    back_um: f64,
    copper_um: Option<f64>,
    hole_diameter_um: Option<f64>,
}

fn apply_pad_clearance_metadata(board: &mut Board, floors: &[PadFloor]) -> Result<(), OpError> {
    let copper_floors = floors
        .iter()
        .map(|floor| {
            let value = floor.copper_um.unwrap_or(0.0);
            let distance = copper_board::Unit::scale(
                value * f64::from(board.communication.resolution.max(1)),
                copper_board::Unit::Um,
                board.communication.unit,
            );
            if !distance.is_finite() || distance < 0.0 || distance > f64::from(i32::MAX) {
                return Err(OpError::Input(format!(
                    "Invalid copper clearance for {}.{}",
                    floor.component, floor.pad
                )));
            }
            Ok(distance.ceil() as i32)
        })
        .collect::<Result<Vec<_>, OpError>>()?;
    let hole_diameters = floors
        .iter()
        .map(|floor| {
            floor
                .hole_diameter_um
                .map(|diameter| {
                    let value = copper_board::Unit::scale(
                        diameter * f64::from(board.communication.resolution.max(1)),
                        copper_board::Unit::Um,
                        board.communication.unit,
                    );
                    if !value.is_finite() || value <= 0.0 || value > f64::from(i32::MAX) {
                        return Err(OpError::Input(format!(
                            "Invalid hole diameter for {}.{}",
                            floor.component, floor.pad
                        )));
                    }
                    Ok(value)
                })
                .transpose()
        })
        .collect::<Result<Vec<_>, OpError>>()?;
    let mut matched = std::collections::BTreeSet::new();
    for id in board.get_pins() {
        let Some(copper_board::items::Item::Pin(pin)) = board.get_item(id) else {
            continue;
        };
        let component = &board.components.get(pin.hdr.get_component_id()).name;
        let ctx = board.ctx();
        let center = pin.get_center(&ctx).to_float();
        let Some(name) = pin.name(&ctx) else { continue };
        if let Some((index, floor)) = floors.iter().enumerate().find(|(_, f)| {
            f.component == *component
                && (((center.x - f64::from(board.clearance_override_board_units(f.x_um))).abs()
                    <= 2.0
                    && (center.y + f64::from(board.clearance_override_board_units(f.y_um))).abs()
                        <= 2.0)
                    || (f.pad == name
                        && floors
                            .iter()
                            .filter(|other| other.component == *component && other.pad == name)
                            .count()
                            == 1))
        }) {
            matched.insert(index);
            let mut values = vec![0; board.get_layer_count()];
            values[0] = board.clearance_override_board_units(floor.front_um);
            let last = values.len() - 1;
            values[last] = board.clearance_override_board_units(floor.back_um);
            for (layer, value) in values.iter_mut().enumerate() {
                *value = board
                    .solder_mask_clearance_limit(id, layer, *value)
                    .max(copper_floors[index]);
            }
            board.raise_pin_clearance(id, &values);
        }
    }
    let holes: Vec<_> = board
        .get_items()
        .filter_map(|item| {
            let copper_board::Item::ObstacleArea(area) = item else {
                return None;
            };
            if item.component_id() <= 0 {
                return None;
            }
            let copper_geometry::Area::Shape(copper_geometry::Shape::Circle(circle)) =
                area.get_area(&board.ctx())
            else {
                return None;
            };
            Some((
                item.id(),
                board.components.get(item.component_id()).name.clone(),
                circle.center,
                circle.radius,
                area.get_layer(),
            ))
        })
        .collect();
    for (id, component, center, radius, layer) in holes {
        let mut minimum = vec![0; board.get_layer_count()];
        for (index, floor) in floors.iter().enumerate() {
            let Some(diameter) = hole_diameters[index] else {
                continue;
            };
            if floor.component != component
                || (f64::from(center.x)
                    - f64::from(board.clearance_override_board_units(floor.x_um)))
                .abs()
                    > 2.0
                || (f64::from(center.y)
                    + f64::from(board.clearance_override_board_units(floor.y_um)))
                .abs()
                    > 2.0
                || diameter > 2.0 * f64::from(radius)
            {
                continue;
            }
            let extra = (0.5 * diameter + f64::from(copper_floors[index]) - f64::from(radius))
                .ceil()
                .max(0.0);
            if extra > f64::from(i32::MAX) {
                return Err(OpError::Input(
                    "Hole clearance exceeds board coordinate range".into(),
                ));
            }
            minimum[layer] = minimum[layer].max(extra as i32);
            matched.insert(index);
        }
        board.raise_obstacle_clearance(id, &minimum);
    }
    for (index, floor) in floors.iter().enumerate() {
        if !matched.contains(&index) {
            tracing::warn!(
                "Pad clearance metadata did not match {}.{}",
                floor.component,
                floor.pad
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod pad_clearance_tests {
    use super::*;
    use serde_json::json;

    fn hole_board() -> Box<Board> {
        let input = r#"(pcb hole
          (resolution um 10) (unit um)
          (structure (layer F.Cu (type signal)) (layer B.Cu (type signal))
            (boundary (rect pcb 0 -10000 10000 0))
            (rule (width 250) (clearance 250)))
          (placement (component HOLE (place H1 5000 -5000 front 0)))
          (library (image HOLE
            (keepout "" (circle F.Cu 3250))
            (keepout "" (circle B.Cu 3250)))) (network))"#;
        let copper_dsn::BoardReadResult::Success {
            board: Some(board), ..
        } = copper_dsn::read_board(
            input.as_bytes(),
            None,
            None,
            &copper_dsn::DsnReadOptions::default(),
        )
        else {
            panic!("hole fixture must import")
        };
        assert!(
            board.get_pins().is_empty(),
            "mechanical holes import as keepouts"
        );
        board
    }

    #[test]
    fn local_hole_rule_accounts_for_existing_keepout_padding() {
        let mut board = hole_board();
        let floors: Vec<PadFloor> = serde_json::from_value(json!([{
            "component":"H1", "pad":"", "x_um":5000, "y_um":5000,
            "front_um":0, "back_um":0, "copper_um":1725, "hole_diameter_um":2750
        }]))
        .unwrap();
        let x = board.clearance_override_board_units(8000.0);
        let y = -board.clearance_override_board_units(5000.0);
        let probe = copper_geometry::TileShape::Box(copper_geometry::IntBox::from_coords(
            x - 1,
            y - 1,
            x + 1,
            y + 1,
        ));
        for layer in 0..board.get_layer_count() {
            let hits = board.overlapping_items_with_clearance(&probe, Some(layer), &[1], 1);
            assert!(!hits.iter().any(|id| matches!(
                board.get_item(*id),
                Some(copper_board::Item::ObstacleArea(_))
            )));
        }
        apply_pad_clearance_metadata(&mut board, &floors).unwrap();
        let expected = board.clearance_override_board_units(1475.0);
        let obstacles: Vec<_> = board
            .get_items()
            .filter_map(|item| {
                if let copper_board::Item::ObstacleArea(area) = item {
                    Some((item.id(), item.clearance_class(), area.get_layer()))
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(obstacles.len(), 2);
        for (id, clearance_class, layer) in obstacles {
            assert!(
                board
                    .overlapping_items_with_clearance(&probe, Some(layer), &[1], 1)
                    .contains(&id),
                "the expanded requirement must reach obstacle queries"
            );
            assert_eq!(
                board
                    .rules
                    .clearance_matrix
                    .get_value(clearance_class, 1, layer, false),
                expected,
                "1.725mm local rule minus 0.250mm padding already in the keepout"
            );
        }
    }

    #[test]
    fn unrelated_or_unverified_hole_metadata_preserves_keepouts() {
        for (component, x, diameter) in [
            ("H1", 5000, None),
            ("other", 5000, Some(2750.0)),
            ("H1", 7000, Some(2750.0)),
            ("H1", 5000, Some(4000.0)),
        ] {
            let mut board = hole_board();
            let before: Vec<_> = board
                .get_items()
                .map(|item| (item.id(), item.clearance_class()))
                .collect();
            let floors: Vec<PadFloor> = serde_json::from_value(json!([{
                "component":component, "pad":"", "x_um":x, "y_um":5000,
                "front_um":0, "back_um":0, "copper_um":1725, "hole_diameter_um":diameter
            }]))
            .unwrap();
            let classes = board.rules.clearance_matrix.get_class_count();
            apply_pad_clearance_metadata(&mut board, &floors).unwrap();
            assert_eq!(classes, board.rules.clearance_matrix.get_class_count());
            assert_eq!(
                before,
                board
                    .get_items()
                    .map(|item| (item.id(), item.clearance_class()))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn hole_metadata_preserves_a_higher_existing_requirement() {
        let mut board = hole_board();
        let ids: Vec<_> = board
            .get_items()
            .filter(|item| matches!(item, copper_board::Item::ObstacleArea(_)))
            .map(|item| item.id())
            .collect();
        assert_eq!(ids.len(), 2);
        let minimum = board.clearance_override_board_units(2000.0);
        for id in &ids {
            board.raise_obstacle_clearance(*id, &vec![minimum; board.get_layer_count()]);
        }
        let floors: Vec<PadFloor> = serde_json::from_value(json!([{
            "component":"H1", "pad":"", "x_um":5000, "y_um":5000,
            "front_um":0, "back_um":0, "copper_um":1725, "hole_diameter_um":2750
        }]))
        .unwrap();
        apply_pad_clearance_metadata(&mut board, &floors).unwrap();
        for id in ids {
            let item = board.get_item(id).unwrap();
            for layer in 0..board.get_layer_count() {
                assert_eq!(
                    board
                        .rules
                        .clearance_matrix
                        .get_value(item.clearance_class(), 1, layer, false),
                    minimum
                );
            }
        }
    }

    #[test]
    fn invalid_hole_diameters_are_rejected_before_matching() {
        for diameter in [-1.0, 0.0, f64::NAN, f64::INFINITY, 1e30] {
            let mut board = hole_board();
            let floor = PadFloor {
                component: "absent".into(),
                pad: "".into(),
                x_um: 0.0,
                y_um: 0.0,
                front_um: 0.0,
                back_um: 0.0,
                copper_um: Some(1725.0),
                hole_diameter_um: Some(diameter),
            };
            assert!(apply_pad_clearance_metadata(&mut board, &[floor]).is_err());
        }
    }

    #[test]
    fn copper_metadata_is_not_limited_by_the_mask_gap_cap() {
        let input = json!({"resolution":10000,"layers":[{"name":"F.Cu"},{"name":"B.Cu"}],
          "nets":[{"id":1,"name":"A"},{"id":2,"name":"B"}],
          "components":[{"reference":"J1","position":{"x":0,"y":0},"layer":"F.Cu","pads":[{"name":"1","netName":"A","shape":"rect","size":{"x":1,"y":1},"layers":["F.Cu","B.Cu"]}]},
          {"reference":"J2","position":{"x":1.2,"y":0},"layer":"F.Cu","pads":[{"name":"1","netName":"B","shape":"rect","size":{"x":1,"y":1},"layers":["F.Cu","B.Cu"]}]}]});
        let copper_dsn::BoardReadResult::Success {
            board: Some(mut board),
            ..
        } = copper_dsn::kicad::read_board(&input.to_string(), None)
        else {
            panic!("fixture must load")
        };
        let floors:Vec<PadFloor>=serde_json::from_value(json!([{"component":"J1","pad":"1","x_um":0,"y_um":0,"front_um":400,"back_um":400,"copper_um":300}])).unwrap();
        let pin = board
            .get_pins()
            .into_iter()
            .find(|id| {
                let item = board.get_item(*id).unwrap();
                board.components.get(item.component_id()).name == "J1"
            })
            .unwrap();
        assert_eq!(board.solder_mask_clearance_limit(pin, 0, 4000), 2000);
        apply_pad_clearance_metadata(&mut board, &floors).unwrap();
        let class = board.get_item(pin).unwrap().clearance_class();
        for layer in 0..2 {
            assert!(
                board
                    .rules
                    .clearance_matrix
                    .get_value(class, 1, layer, false)
                    >= 3000
            );
        }
    }
    #[test]
    fn invalid_copper_metadata_is_rejected_even_without_matching_pads() {
        for value in [-1.0, f64::INFINITY, f64::NAN, 1e30] {
            let input = json!({"resolution":10000,"layers":[{"name":"F.Cu"},{"name":"B.Cu"}]});
            let copper_dsn::BoardReadResult::Success {
                board: Some(mut board),
                ..
            } = copper_dsn::kicad::read_board(&input.to_string(), None)
            else {
                panic!("fixture must load")
            };
            let floor = PadFloor {
                component: "absent".into(),
                pad: "1".into(),
                x_um: 0.0,
                y_um: 0.0,
                front_um: 0.0,
                back_um: 0.0,
                copper_um: Some(value),
                hole_diameter_um: None,
            };
            assert!(apply_pad_clearance_metadata(&mut board, &[floor]).is_err());
        }
    }
}

/// Unfixes the fixed traces and vias on `nets` (net names, or `all`) so the router may rip and
/// reroute them. Returns how many items were retired.
pub fn retire_fixed(board: &mut copper_board::Board, nets: &[String]) -> Result<usize, String> {
    if nets.is_empty() {
        return Ok(0);
    }
    let all = nets.iter().any(|n| n == "all");
    let mut numbers = std::collections::BTreeSet::new();
    if !all {
        for name in nets {
            let found = board.rules.nets.get_by_name(name);
            if found.is_empty() {
                return Err(format!("--retire-fixed {name}: no such net on the board"));
            }
            numbers.extend(found.iter().map(|net| net.net_number));
        }
    }
    let retired: Vec<copper_board::ItemId> = board
        .get_items()
        .filter(|item| {
            (item.is_trace() || matches!(item, copper_board::Item::Via(_)))
                && item.is_user_fixed()
                && (all || item.net_nos().iter().any(|n| numbers.contains(n)))
        })
        .map(copper_board::Item::id)
        .collect();
    for &id in &retired {
        if let Some(item) = board.get_item_mut(id) {
            item.set_fixed_state(copper_board::structure::FixedState::Unfixed);
        }
    }
    Ok(retired.len())
}
