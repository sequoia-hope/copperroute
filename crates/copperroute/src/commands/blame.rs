use std::collections::BTreeMap;

use copper_board::Board;
use copper_board::structure::Unit;
use copper_gpu::{BlameOptions, blame};

use crate::ExitCode;
use crate::cli::{BlameArgs, Cli};
use crate::ops::load::{BoardSource, LoadRequest, load};

pub fn run(cli: &Cli, args: &BlameArgs) -> ExitCode {
    let mut request = LoadRequest::for_board(BoardSource::Path(args.input.clone()));
    request.discover_adjacent_rules = true;
    request.kicad_project = args.kicad_project.clone();
    request.session = args.ses.clone();
    request.settings = super::overrides(cli, &[]);
    let mut loaded = match load(&request) {
        Ok(loaded) => loaded,
        Err(error) => {
            tracing::error!("{error}");
            return ExitCode::Failure;
        }
    };
    let board = &mut loaded.board;
    for name in &args.lock {
        match board.components.get_by_name(name).map(|c| c.id) {
            Some(id) => board.components.get_mut(id).position_fixed = true,
            None => {
                tracing::error!("--lock {name}: no such part on the board");
                return ExitCode::UsageError;
            }
        }
    }

    let per_mm = board.communication.get_resolution(Unit::Mm);
    let options = BlameOptions {
        cell: args.cell_mm * per_mm,
        via_cost: 200,
        margin: args.margin_mm * per_mm,
        max_move: args.max_move_mm * per_mm,
        routed_copper: args.routed_copper,
    };
    let names: BTreeMap<i32, String> = board
        .components
        .get_all()
        .map(|c| (c.id, c.name.clone()))
        .collect();
    let fixed: Vec<i32> = board
        .components
        .get_all()
        .filter(|c| c.position_fixed)
        .map(|c| c.id)
        .collect();
    let name = |id: i32| names.get(&id).cloned().unwrap_or_else(|| id.to_string());
    let gaps: Vec<_> = copper_drc::DesignRulesChecker::new(board)
        .get_all_unconnected_items()
        .into_iter()
        .filter_map(|g| g.second_item.map(|second| (g.first_item, second)))
        .collect();
    let mut out = Vec::new();
    for (from, to) in gaps {
        let report = blame(board, from, to, &options, &|c| !fixed.contains(&c));
        out.push(serde_json::json!({
            "net": net_name(board, from),
            "from": item_name(board, from, &name),
            "to": item_name(board, to, &name),
            "walled_off": report.walled_off,
            "end_cells": [report.end_cells.0, report.end_cells.1],
            "path_cells": report.path_cells,
            "routed_cells": report.routed_cells,
            "pinned_cells": report.pinned_cells,
            "pinned_parts": report.pinned_parts.iter().map(|&c| name(c)).collect::<Vec<_>>(),
            "intruded_cells": report.intruded_cells,
            "moves_mm": report.blamed.iter().map(|b| serde_json::json!({
                "part": name(b.component),
                "dx": b.dx / per_mm,
                "dy": b.dy / per_mm,
            })).collect::<Vec<_>>(),
            "unmovable": report.unmovable.iter().map(|&c| name(c)).collect::<Vec<_>>(),
        }));
    }
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    ExitCode::Ok
}

fn net_name(board: &Board, item: copper_board::ItemId) -> String {
    board
        .get_item(item)
        .and_then(|i| i.net_nos().first().copied())
        .and_then(|net| board.rules.nets.get(net).map(|n| n.name.clone()))
        .unwrap_or_default()
}

fn item_name(board: &Board, item: copper_board::ItemId, name: &dyn Fn(i32) -> String) -> String {
    match board.get_item(item) {
        Some(i) if i.component_id() > 0 => name(i.component_id()),
        Some(i) => format!("{:?}", i.kind()).to_lowercase(),
        None => String::new(),
    }
}
