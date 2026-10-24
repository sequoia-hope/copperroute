//! A session can only add wiring, and it leaves the design's system-fixed wiring out. When bending
//! or retiring changes that wiring, the session carries the new geometry and a sidecar lists the
//! fixed items it supersedes, which a reader removes from the design first.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use copper_board::structure::FixedState;
use copper_board::{Board, Item, ItemId};
use copper_dsn::CoordinateTransform;
use copper_geometry::Point;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Wiring {
    Trace {
        layer: usize,
        half_width: i32,
        nets: Vec<i32>,
        corners: Vec<(i32, i32)>,
    },
    Via {
        nets: Vec<i32>,
        center: (i32, i32),
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Superseded {
    /// Board coordinates, for readers that load the same design.
    pub wiring: Wiring,
    /// The same item in design (DSN) units, with its layer and net names.
    pub layer: Option<String>,
    pub nets: Vec<String>,
    pub dsn_points: Vec<[f64; 2]>,
}

pub fn system_fixed_wiring(board: &Board) -> BTreeSet<Wiring> {
    board
        .get_items()
        .filter(|item| item.get_fixed_state() == FixedState::SystemFixed)
        .filter_map(|item| signature(board, item))
        .collect()
}

fn signature(board: &Board, item: &Item) -> Option<Wiring> {
    let mut nets = item.net_nos().to_vec();
    nets.sort_unstable();
    match item {
        Item::Trace(trace) => {
            let polyline = trace.polyline();
            let corners = (0..polyline.corner_count())
                .filter_map(|i| match polyline.corner(i) {
                    Some(Point::Int(p)) => Some((p.x, p.y)),
                    _ => None,
                })
                .collect();
            Some(Wiring::Trace {
                layer: trace.get_layer(),
                half_width: trace.get_half_width(),
                nets,
                corners,
            })
        }
        Item::Via(_) => match board.drill_center(item.id()) {
            Some(Point::Int(c)) => Some(Wiring::Via {
                nets,
                center: (c.x, c.y),
            }),
            _ => None,
        },
        _ => None,
    }
}

/// Readies `board` for writing a session against a design whose system-fixed wiring was
/// `original`: changed fixed wiring becomes user fixed so the session carries it, and the original
/// items no longer on the board are returned.
pub fn supersede(
    board: &mut Board,
    original: &BTreeSet<Wiring>,
    transform: &CoordinateTransform,
) -> Vec<Superseded> {
    let changed: Vec<ItemId> = board
        .get_items()
        .filter(|item| item.get_fixed_state() == FixedState::SystemFixed)
        .filter(|item| signature(board, item).is_none_or(|s| !original.contains(&s)))
        .map(Item::id)
        .collect();
    for id in changed {
        if let Some(item) = board.get_item_mut(id) {
            item.set_fixed_state(FixedState::UserFixed);
        }
    }
    let kept = system_fixed_wiring(board);
    original
        .iter()
        .filter(|wiring| !kept.contains(wiring))
        .map(|wiring| describe(board, wiring, transform))
        .collect()
}

fn describe(board: &Board, wiring: &Wiring, transform: &CoordinateTransform) -> Superseded {
    let net_names = |nets: &[i32]| {
        nets.iter()
            .filter_map(|n| board.rules.nets.get(*n).map(|net| net.name.clone()))
            .collect()
    };
    let to_dsn = |(x, y): (i32, i32)| {
        transform.board_to_dsn_point(&copper_geometry::FloatPoint::new(f64::from(x), f64::from(y)))
    };
    match wiring {
        Wiring::Trace {
            layer,
            nets,
            corners,
            ..
        } => Superseded {
            wiring: wiring.clone(),
            layer: Some(board.layer_structure().layers[*layer].name.clone()),
            nets: net_names(nets),
            dsn_points: corners.iter().copied().map(to_dsn).collect(),
        },
        Wiring::Via { nets, center } => Superseded {
            wiring: wiring.clone(),
            layer: None,
            nets: net_names(nets),
            dsn_points: vec![to_dsn(*center)],
        },
    }
}

/// Where the sidecar of a session lives: `out.ses` → `out.superseded.json`.
pub fn sidecar_path(session: &Path) -> PathBuf {
    session.with_extension("superseded.json")
}

/// Writes the sidecar, or removes a stale one when nothing is superseded.
pub fn write_sidecar(session: &Path, superseded: &[Superseded]) -> std::io::Result<()> {
    let path = sidecar_path(session);
    if superseded.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        };
    }
    let text = serde_json::to_string_pretty(superseded).map_err(std::io::Error::other)?;
    std::fs::write(path, text)
}

/// Removes the design's fixed items a sidecar lists, every copy of each. Returns how many of the
/// listed items were found.
fn remove_superseded(board: &mut Board, superseded: &[Superseded]) -> usize {
    let wanted: BTreeSet<&Wiring> = superseded.iter().map(|s| &s.wiring).collect();
    let mut found = BTreeSet::new();
    let mut doomed = Vec::new();
    for item in board.get_items() {
        if let Some(s) = signature(board, item)
            && wanted.contains(&s)
        {
            doomed.push(item.id());
            found.insert(s);
        }
    }
    for &id in &doomed {
        if let Some(item) = board.get_item_mut(id) {
            item.set_fixed_state(FixedState::Unfixed);
        }
        board.remove_item(id);
    }
    found.len()
}

/// Removes what the sidecar beside `session` supersedes, if there is one.
pub fn apply_sidecar(session: &Path, board: &mut Board) -> Result<usize, String> {
    let path = sidecar_path(session);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let superseded: Vec<Superseded> =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let removed = remove_superseded(board, &superseded);
    if removed != superseded.len() {
        return Err(format!(
            "{}: {} of {} superseded items are not on the board",
            path.display(),
            superseded.len() - removed,
            superseded.len()
        ));
    }
    Ok(removed)
}
