//! Which movable parts stand in the way of an open connection, and how far each must move to
//! clear it. The connection is routed on its net's grid with movable parts' copper passable at a
//! price that grows with how deeply the path would intrude into its clearance; the intrusions on
//! the cheapest path become per-part displacements. Only copper that belongs to no part and is not
//! routed copper (outline, keepouts, planes) is a wall.

use std::collections::{BTreeMap, BTreeSet};

use copper_board::items::Item;
use copper_board::{Board, ItemId};
use copper_drc::constraints::{netclass_name, resolve};
use copper_geometry::{FloatPoint, TileShape};

use crate::grid::{GridProfile, ObstacleMap, RouteGrid};
use crate::route::terminal_cells;
use crate::search::{STRAIGHT, cost_field, trace_back};

/// Cost of entering a cell inside a movable part's clearance, plus a share per cell of intrusion.
const INTRUSION_BASE: u32 = 2 * STRAIGHT;
const INTRUSION_PER_CELL: f64 = 2.0 * STRAIGHT as f64;
/// Cost of entering the clearance of a part that may not move, or that owns an end: passable
/// so a path can leave a crowded pad, but nothing moving can clear it.
const PINNED_PART: u32 = 8 * STRAIGHT;
const SOLVER_SWEEPS: usize = 50;

#[derive(Debug, Clone, PartialEq)]
pub struct Blame {
    pub component: i32,
    /// The displacement that clears the path, in board units.
    pub dx: f64,
    pub dy: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlameOptions {
    /// Cell size, in board units.
    pub cell: f64,
    pub via_cost: u32,
    /// Clearance kept beyond the rules, in board units.
    pub margin: f64,
    /// Displacements longer than this are dropped, in board units.
    pub max_move: f64,
    /// Cost of entering a cell other nets' routed copper claims, which the router may rip;
    /// `None` makes routed copper a wall, so only moving parts can open the path.
    pub routed_copper: Option<u32>,
}

/// What stands between the two ends of an open connection on its cheapest path.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BlameReport {
    /// No path at all: walls alone separate the ends.
    pub walled_off: bool,
    /// Grid cells each end offers a path, before any search; none means the end is buried.
    pub end_cells: (usize, usize),
    pub path_cells: usize,
    /// Path cells inside other nets' routed copper clearance.
    pub routed_cells: usize,
    /// Path cells inside the clearance of parts that may not move or own an end, with those
    /// parts.
    pub pinned_cells: usize,
    pub pinned_parts: Vec<i32>,
    /// Path cells inside movable parts' clearance.
    pub intruded_cells: usize,
    /// Movable parts in the way, with the displacement that clears the path; parts whose
    /// displacement is unbounded or too long are left out.
    pub blamed: Vec<Blame>,
    /// Movable parts in the way that no sensible translation clears.
    pub unmovable: Vec<i32>,
}

struct Intruder {
    component: i32,
    movable: bool,
    routing_layer: usize,
    shape: TileShape,
    /// Trace half width plus clearance.
    reach: f64,
}

/// The parts `movable` allows to move that block the cheapest route between `from` and `to`,
/// with the displacement each needs. Parts owning either end are not blamed: moving them moves
/// the end too.
pub fn blame(
    board: &mut Board,
    from: ItemId,
    to: ItemId,
    options: &BlameOptions,
    movable: &dyn Fn(i32) -> bool,
) -> BlameReport {
    let mut report = BlameReport::default();
    let Some(net) = board
        .get_item(from)
        .and_then(|i| i.net_nos().first().copied())
    else {
        report.walled_off = true;
        return report;
    };
    let ends: BTreeSet<i32> = [from, to]
        .iter()
        .filter_map(|&id| board.get_item(id).map(Item::component_id))
        .collect();
    let profile = GridProfile::for_net(board, net);
    let (mut hard, mut routed, mut pinned, mut parts) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for item in board.get_items() {
        let component = item.component_id();
        if component > 0 && movable(component) && !ends.contains(&component) {
            parts.push(item.id());
        } else if component > 0 {
            pinned.push(item.id());
        } else if matches!(item, Item::Trace(_) | Item::Via(_)) {
            match options.routed_copper {
                Some(_) => routed.push(item.id()),
                None => hard.push(item.id()),
            }
        } else {
            hard.push(item.id());
        }
    }
    let mut hard_map = ObstacleMap::new(board, profile.clone(), options.cell);
    hard_map.stamp(board, &hard);
    let mut routed_map = ObstacleMap::new(board, profile.clone(), options.cell);
    routed_map.stamp(board, &routed);
    let mut pinned_map = ObstacleMap::new(board, profile.clone(), options.cell);
    pinned_map.stamp(board, &pinned);
    let mut grid = RouteGrid::view(&hard_map, net);
    let routed_grid = RouteGrid::view(&routed_map, net);
    let pinned_grid = RouteGrid::view(&pinned_map, net);

    let mut intruders = intruders(board, &profile, net, &parts, true);
    let mut depth = vec![f64::NEG_INFINITY; grid.blocked.len()];
    for intruder in &intruders {
        stamp_depth(&grid, intruder, options.margin, &mut depth);
    }
    intruders.extend(self::intruders(board, &profile, net, &pinned, false));
    grid.penalty = (0..grid.blocked.len())
        .map(|n| {
            let mut penalty = 0;
            if routed_grid.blocked[n] {
                penalty += options.routed_copper.unwrap_or(0);
            }
            if pinned_grid.blocked[n] {
                penalty += PINNED_PART;
            }
            if depth[n] > 0.0 {
                penalty += INTRUSION_BASE + (INTRUSION_PER_CELL * depth[n] / grid.h).round() as u32;
            }
            penalty
        })
        .collect();

    let sources = terminal_cells(board, &grid, from);
    let targets = terminal_cells(board, &grid, to);
    report.end_cells = (sources.len(), targets.len());
    let cost = cost_field(&grid, &sources, options.via_cost);
    let Some(path) = trace_back(&grid, &cost, &targets, options.via_cost) else {
        report.walled_off = true;
        return report;
    };
    report.path_cells = path.len();

    let mut constraints: BTreeMap<i32, Vec<(FloatPoint, f64)>> = BTreeMap::new();
    let mut pinned_parts: BTreeSet<i32> = BTreeSet::new();
    for &(layer, i, j) in &path {
        let n = grid.index(layer, i, j);
        report.routed_cells += usize::from(routed_grid.blocked[n]);
        report.pinned_cells += usize::from(pinned_grid.blocked[n]);
        if pinned_grid.blocked[n] {
            let at = grid.centre(i, j);
            pinned_parts.extend(
                intruders
                    .iter()
                    .filter(|x| !x.movable && x.routing_layer == layer)
                    .filter(|x| x.shape.distance(&at) < x.reach + grid.h)
                    .map(|x| x.component),
            );
        }
        if depth[n] <= 0.0 {
            continue;
        }
        report.intruded_cells += 1;
        let at = grid.centre(i, j);
        for intruder in intruders
            .iter()
            .filter(|x| x.movable && x.routing_layer == layer)
        {
            if let Some(push) = clearing_push(intruder, &at, options.margin) {
                constraints
                    .entry(intruder.component)
                    .or_default()
                    .push(push);
            }
        }
    }
    report.pinned_parts = pinned_parts.into_iter().collect();
    for (component, pushes) in constraints {
        let (dx, dy) = satisfy(&pushes);
        if dx.hypot(dy) <= options.max_move {
            report.blamed.push(Blame { component, dx, dy });
        } else {
            report.unmovable.push(component);
        }
    }
    report
}

fn intruders(
    board: &mut Board,
    profile: &GridProfile,
    net: i32,
    ids: &[ItemId],
    movable: bool,
) -> Vec<Intruder> {
    let constraints = resolve(board);
    let mut out = Vec::new();
    for &id in ids {
        let (component, class, other_rule, layers) = {
            let ctx = board.ctx();
            let Some(item) = board.get_item(id) else {
                continue;
            };
            if !item.is_trace_obstacle(i32::MIN) || item.net_nos().contains(&net) {
                continue;
            }
            let other_rule = (item.net_count() > 0)
                .then(|| netclass_name(board, item.get_net_number(0)))
                .flatten()
                .and_then(|name| constraints.netclass_clearance.get(&name).copied())
                .unwrap_or(0);
            let layers: Vec<usize> = (0..item.tile_shape_count(&ctx))
                .map(|i| item.shape_layer(i, &ctx))
                .collect();
            (
                item.component_id(),
                item.clearance_class(),
                other_rule,
                layers,
            )
        };
        for (index, layer) in layers.into_iter().enumerate() {
            let Some(routing_layer) = profile.layers.iter().position(|&l| l == layer) else {
                continue;
            };
            let Some(shape) = board.item_tile_shape(id, index) else {
                continue;
            };
            let reach = f64::from(profile.half_width)
                + profile.clearance(
                    board,
                    &constraints,
                    profile.trace_clearance_class,
                    class,
                    layer,
                    other_rule,
                );
            out.push(Intruder {
                component,
                movable,
                routing_layer,
                shape,
                reach,
            });
        }
    }
    out
}

/// Records, per cell, the deepest intrusion of a trace centred there into `intruder`'s clearance.
fn stamp_depth(grid: &RouteGrid, intruder: &Intruder, margin: f64, depth: &mut [f64]) {
    let reach = intruder.reach + margin;
    let bb = intruder.shape.bounding_box();
    let lo_i = ((f64::from(bb.ll.x) - reach - grid.x0) / grid.h)
        .floor()
        .max(0.0) as usize;
    let lo_j = ((f64::from(bb.ll.y) - reach - grid.y0) / grid.h)
        .floor()
        .max(0.0) as usize;
    let hi_i = (((f64::from(bb.ur.x) + reach - grid.x0) / grid.h).ceil() as usize).min(grid.nx - 1);
    let hi_j = (((f64::from(bb.ur.y) + reach - grid.y0) / grid.h).ceil() as usize).min(grid.ny - 1);
    for j in lo_j..=hi_j {
        for i in lo_i..=hi_i {
            let d = reach - intruder.shape.distance(&grid.centre(i, j));
            let n = grid.index(intruder.routing_layer, i, j);
            depth[n] = depth[n].max(d);
        }
    }
}

/// The direction to move `intruder` and how far along it, so a trace centred at `at` clears it.
fn clearing_push(intruder: &Intruder, at: &FloatPoint, margin: f64) -> Option<(FloatPoint, f64)> {
    let reach = intruder.reach + margin;
    let distance = intruder.shape.distance(at);
    if distance >= reach {
        return None;
    }
    if distance > 0.0 {
        let near = intruder.shape.nearest_point_approx(at)?;
        let n = unit(near.x - at.x, near.y - at.y)?;
        return Some((n, reach - distance));
    }
    let bb = intruder.shape.bounding_box();
    let n = unit(
        (f64::from(bb.ll.x) + f64::from(bb.ur.x)) / 2.0 - at.x,
        (f64::from(bb.ll.y) + f64::from(bb.ur.y)) / 2.0 - at.y,
    )?;
    let behind = [
        (bb.ll.x, bb.ll.y),
        (bb.ll.x, bb.ur.y),
        (bb.ur.x, bb.ll.y),
        (bb.ur.x, bb.ur.y),
    ]
    .into_iter()
    .map(|(x, y)| (at.x - f64::from(x)) * n.x + (at.y - f64::from(y)) * n.y)
    .fold(0.0, f64::max);
    Some((n, reach + behind))
}

fn unit(x: f64, y: f64) -> Option<FloatPoint> {
    let length = x.hypot(y);
    (length > 0.0).then(|| FloatPoint::new(x / length, y / length))
}

/// A displacement `v` with `v · n >= d` for every push, by cyclic projection; pushes from both
/// sides of the path cannot all be met, and the displacement then grows past any sensible move.
fn satisfy(pushes: &[(FloatPoint, f64)]) -> (f64, f64) {
    let (mut x, mut y) = (0.0, 0.0);
    for _ in 0..SOLVER_SWEEPS {
        let mut moved = false;
        for (n, d) in pushes {
            let short = d - (x * n.x + y * n.y);
            if short > 1e-6 {
                x += short * n.x;
                y += short * n.y;
                moved = true;
            }
        }
        if !moved {
            return (x, y);
        }
    }
    (f64::INFINITY, f64::INFINITY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opposing_pushes_have_no_displacement() {
        let pushes = [
            (FloatPoint::new(1.0, 0.0), 1.0),
            (FloatPoint::new(-1.0, 0.0), 1.0),
        ];
        assert!(satisfy(&pushes).0.is_infinite());
    }

    #[test]
    fn perpendicular_pushes_add_up() {
        let pushes = [
            (FloatPoint::new(1.0, 0.0), 2.0),
            (FloatPoint::new(0.0, 1.0), 3.0),
        ];
        let (x, y) = satisfy(&pushes);
        assert!((x - 2.0).abs() < 1e-6 && (y - 3.0).abs() < 1e-6);
    }
}
