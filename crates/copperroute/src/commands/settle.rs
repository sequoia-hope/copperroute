use std::collections::{BTreeMap, BTreeSet};

use copper_board::items::Item;
use copper_board::structure::Unit;
use copper_geometry::IntVector;
use copper_morph::{MoveOracle, SettleOptions};

use crate::ExitCode;
use crate::cli::{Cli, SettleArgs};
use crate::ops::load::{BoardSource, LoadRequest, load};

pub fn run(cli: &Cli, args: &SettleArgs) -> ExitCode {
    let mut request = LoadRequest::for_board(BoardSource::Path(args.input.clone()));
    request.discover_adjacent_rules = true;
    request.kicad_project = args.kicad_project.clone();
    request.settings = super::overrides(cli, &[]);
    let mut loaded = match load(&request) {
        Ok(loaded) => loaded,
        Err(error) => {
            tracing::error!("{error}");
            return ExitCode::Failure;
        }
    };
    let board = &mut loaded.board;

    let designed: BTreeSet<_> = board
        .get_items()
        .filter(|i| is_wiring(i))
        .map(Item::id)
        .collect();
    let session = match std::fs::read(&args.ses) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::error!("Couldn't read {}: {error}", args.ses.display());
            return ExitCode::Failure;
        }
    };
    if let Err(error) = copper_dsn::ses_reader::read(&session[..], board, &loaded.transform) {
        tracing::error!("Couldn't import {}: {error}", args.ses.display());
        return ExitCode::Failure;
    }
    // The session reader fixes what it imports; the routing it carries is what settle may bend,
    // unlike wiring the design itself fixed.
    let routed: Vec<_> = board
        .get_items()
        .filter(|i| is_wiring(i) && !designed.contains(&i.id()))
        .map(Item::id)
        .collect();
    for id in routed {
        if let Some(item) = board.get_item_mut(id) {
            item.unfix();
        }
    }

    let per_mm = board.communication.get_resolution(Unit::Mm);
    let targets = match read_targets(&args.targets, board, per_mm) {
        Ok(targets) => targets,
        Err(error) => {
            tracing::error!("{error}");
            return ExitCode::UsageError;
        }
    };
    let oracle = MoveOracle {
        guard: args.guard_mm * per_mm,
        routable_copper_is_soft: args.soft,
    };
    let options = SettleOptions {
        step_mm: args.step_mm,
        max_cycles: args.max_cycles,
        ..SettleOptions::default()
    };
    let report = oracle.settle(board, &targets, &options);

    let mut bytes = Vec::new();
    if let Err(error) = copper_core::save_as_specctra_session_ses(
        board,
        &loaded.transform,
        &loaded.job.name,
        &mut bytes,
    )
    .map_err(|e| e.to_string())
    .and_then(|()| std::fs::write(&args.output, &bytes).map_err(|e| e.to_string()))
    {
        tracing::error!("Couldn't write {}: {error}", args.output.display());
        return ExitCode::Failure;
    }

    let names: BTreeMap<i32, String> = board
        .components
        .get_all()
        .map(|c| (c.id, c.name.clone()))
        .collect();
    let parts: serde_json::Map<String, serde_json::Value> = report
        .parts
        .iter()
        .map(|(id, outcome)| {
            let moves: serde_json::Map<String, serde_json::Value> = outcome
                .moves
                .iter()
                .map(|(kind, n)| (format!("{kind:?}").to_lowercase(), serde_json::json!(n)))
                .collect();
            (
                names.get(id).cloned().unwrap_or_else(|| id.to_string()),
                serde_json::json!({
                    "travelled_mm": outcome.travelled / per_mm,
                    "wanted_mm": outcome.wanted / per_mm,
                    "arrived": outcome.arrived,
                    "moves": moves,
                }),
            )
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({ "cycles": report.cycles, "parts": parts })
        )
        .unwrap_or_default()
    );
    if report.parts.values().all(|o| o.arrived) {
        ExitCode::Ok
    } else {
        ExitCode::Failure
    }
}

fn is_wiring(item: &Item) -> bool {
    item.is_trace() || matches!(item, Item::Via(_))
}

/// `{"J4": [-0.5, 0.0], ...}`: a displacement in millimetres per component reference.
fn read_targets(
    path: &std::path::Path,
    board: &copper_board::Board,
    per_mm: f64,
) -> Result<BTreeMap<i32, IntVector>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let raw: BTreeMap<String, [f64; 2]> =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    raw.into_iter()
        .map(|(name, [dx, dy])| {
            let component = board
                .components
                .get_all()
                .find(|c| c.name == name)
                .ok_or_else(|| format!("{}: no component {name}", path.display()))?;
            if component.position_fixed {
                return Err(format!("{}: {name} is locked in place", path.display()));
            }
            Ok((
                component.id,
                IntVector::new((dx * per_mm).round() as i32, (dy * per_mm).round() as i32),
            ))
        })
        .collect()
}
