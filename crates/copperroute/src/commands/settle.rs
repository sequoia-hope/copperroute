use std::collections::{BTreeMap, BTreeSet};

use copper_board::items::Item;
use copper_board::structure::Unit;
use copper_geometry::IntVector;
use copper_geometry::FloatPoint;
use copper_morph::{Dipole, MoveKind, MoveOracle, SettleOptions, dipole_targets};

use crate::ExitCode;
use crate::cli::{Cli, SettleArgs};
use crate::ops::fixed_wiring;
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
    let mut locked = Vec::new();
    for name in &args.lock {
        match board.components.get_by_name(name).map(|c| c.id) {
            Some(id) if board.components.get_mut(id).position_fixed => {}
            Some(id) => {
                board.components.get_mut(id).position_fixed = true;
                locked.push(id);
            }
            None => {
                tracing::error!("--lock {name}: no such part on the board");
                return ExitCode::UsageError;
            }
        }
    }

    let original = fixed_wiring::system_fixed_wiring(board);
    if let Err(error) = fixed_wiring::apply_sidecar(&args.ses, board) {
        tracing::error!("{error}");
        return ExitCode::Failure;
    }
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

    match crate::ops::load::retire_fixed(board, &args.retire_fixed) {
        Ok(0) => {}
        Ok(n) => tracing::info!("retired {n} fixed trace(s) and via(s)"),
        Err(error) => {
            tracing::error!("{error}");
            return ExitCode::UsageError;
        }
    }

    let per_mm = board.communication.get_resolution(Unit::Mm);
    let mut targets = match &args.targets {
        Some(path) => match read_targets(path, board, per_mm) {
            Ok(targets) => targets,
            Err(error) => {
                tracing::error!("{error}");
                return ExitCode::UsageError;
            }
        },
        None => BTreeMap::new(),
    };
    let dipoles = match args
        .dipole
        .iter()
        .map(|d| parse_dipole(d, per_mm))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(dipoles) => dipoles,
        Err(error) => {
            tracing::error!("{error}");
            return ExitCode::UsageError;
        }
    };
    for (component, v) in dipole_targets(board, &dipoles, 0.005 * per_mm) {
        let target = targets.entry(component).or_insert(IntVector::ZERO);
        *target = IntVector::new(target.x + v.x, target.y + v.y);
    }
    let mut placed = Vec::new();
    for name in &args.near_partners {
        match board.components.get_by_name(name) {
            Some(c) if !c.position_fixed => placed.push(c.id),
            Some(_) => {
                tracing::error!("--near-partners {name}: the part is locked in place");
                return ExitCode::UsageError;
            }
            None => {
                tracing::error!("--near-partners {name}: no such part on the board");
                return ExitCode::UsageError;
            }
        }
    }
    if targets.is_empty() && placed.is_empty() {
        tracing::error!("Nothing to move: give --targets, --dipole or --near-partners");
        return ExitCode::UsageError;
    }

    let recording = match &args.frames {
        Some(dir) => match super::frames::record(dir, args.max_frames, dipole_overlay(&dipoles)) {
            Ok(recording) => {
                let moving: Vec<i32> = targets.keys().chain(&placed).copied().collect();
                recording
                    .writer
                    .borrow_mut()
                    .write(board, &["start"], &moving, &IntVector::ZERO);
                Some(recording)
            }
            Err(error) => {
                tracing::error!("Couldn't record frames in {}: {error}", dir.display());
                return ExitCode::Failure;
            }
        },
        None => None,
    };

    let oracle = MoveOracle {
        guard: args.guard_mm * per_mm,
        routable_copper_is_soft: args.soft,
        fixed_copper_bends: args.bend_fixed,
    };
    for &component in &placed {
        let found = oracle.place_near_partners(
            board,
            component,
            (2.0 * per_mm) as i32,
            (0.1 * per_mm) as i32,
            64,
        );
        tracing::info!("near partners: {component} moved by {found:?}");
    }
    let only = args.only.as_deref().map(|kind| match kind {
        "warp" => MoveKind::Warp,
        "shove" => MoveKind::Shove,
        "push" => MoveKind::Push,
        _ => MoveKind::Group,
    });
    let options = SettleOptions {
        step_mm: args.step_mm,
        max_cycles: args.max_cycles,
        only,
        ..SettleOptions::default()
    };
    let report = oracle.settle(board, &targets, &options);
    if let Some(recording) = &recording {
        let moved: Vec<i32> = report.parts.keys().copied().collect();
        recording
            .writer
            .borrow_mut()
            .write(board, &["end"], &moved, &IntVector::ZERO);
    }

    // A session that marks a part `lock_type position` is one KiCad refuses to import.
    for &id in &locked {
        board.components.get_mut(id).position_fixed = false;
    }
    let superseded = fixed_wiring::supersede(board, &original, &loaded.transform);
    let mut bytes = Vec::new();
    if let Err(error) = copper_core::save_as_specctra_session_ses(
        board,
        &loaded.transform,
        &loaded.job.name,
        &mut bytes,
    )
    .map_err(|e| e.to_string())
    .and_then(|()| std::fs::write(&args.output, &bytes).map_err(|e| e.to_string()))
    .and_then(|()| {
        fixed_wiring::write_sidecar(&args.output, &superseded).map_err(|e| e.to_string())
    }) {
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

/// `x,y,dx,dy[,radius[,decay]]` in millimetres.
fn parse_dipole(text: &str, per_mm: f64) -> Result<Dipole, String> {
    let values: Vec<f64> = text
        .split(',')
        .map(|v| v.trim().parse::<f64>())
        .collect::<Result<_, _>>()
        .map_err(|e| format!("--dipole {text}: {e}"))?;
    if !(4..=6).contains(&values.len()) {
        return Err(format!("--dipole {text}: expected x,y,dx,dy[,radius[,decay]]"));
    }
    Ok(Dipole {
        center: FloatPoint {
            x: values[0] * per_mm,
            y: values[1] * per_mm,
        },
        vector: (values[2] * per_mm, values[3] * per_mm),
        radius: values.get(4).copied().unwrap_or(3.0) * per_mm,
        decay: values.get(5).copied().unwrap_or(2.0),
    })
}

/// Each dipole as its radius and an arrow one radius long along its push.
fn dipole_overlay(dipoles: &[Dipole]) -> String {
    let mut out = String::new();
    for d in dipoles {
        let (cx, cy) = (d.center.x, -d.center.y);
        let length = d.vector.0.hypot(d.vector.1).max(1e-9);
        let (ux, uy) = (d.vector.0 / length, -d.vector.1 / length);
        let (tx, ty) = (cx + ux * d.radius, cy + uy * d.radius);
        let w = d.radius / 40.0;
        out.push_str(&format!(
            r##"<g class="dipole" fill="none" stroke="#ffd84d" stroke-width="{w}"><circle cx="{cx}" cy="{cy}" r="{r}" stroke-dasharray="{a} {a}"/><line x1="{cx}" y1="{cy}" x2="{tx}" y2="{ty}"/><path d="M {tx} {ty} L {lx} {ly} M {tx} {ty} L {rx} {ry}"/></g>"##,
            r = d.radius,
            a = w * 4.0,
            lx = tx - ux * w * 8.0 - uy * w * 5.0,
            ly = ty - uy * w * 8.0 + ux * w * 5.0,
            rx = tx - ux * w * 8.0 + uy * w * 5.0,
            ry = ty - uy * w * 8.0 - ux * w * 5.0,
        ));
    }
    out
}
