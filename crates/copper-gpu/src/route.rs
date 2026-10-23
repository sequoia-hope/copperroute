//! Sequential grid routing: each open connection is routed on a fresh conservative grid of its
//! net, with everything already on the board as obstacles, and committed as traces and vias.

use std::collections::{BTreeMap, BTreeSet};

use copper_board::items::Item;
use copper_board::{Board, FixedState, ItemId};
use copper_geometry::{FloatPoint, IntPoint, Point, Polyline};

use crate::grid::{GridProfile, ObstacleMap, RouteGrid};
use crate::search::{GridPath, cost_field, trace_back};

/// How far past a pad an escape stub may reach, in cells, and how many stubs are tried.
const STUB_CELLS: f64 = 3.0;
const STUB_CANDIDATES: usize = 24;

/// Computes the cost field for a grid; the CPU reference or the GPU kernel.
pub trait FieldSolver {
    fn solve(&mut self, grid: &RouteGrid, sources: &[usize], via_cost: u32) -> Vec<u32>;
}

pub struct CpuSolver;

impl FieldSolver for CpuSolver {
    fn solve(&mut self, grid: &RouteGrid, sources: &[usize], via_cost: u32) -> Vec<u32> {
        cost_field(grid, sources, via_cost)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridOptions {
    /// Cell size, in board units.
    pub cell: f64,
    pub via_cost: u32,
    /// Connections to attempt before giving up, so a board that cannot close does not spin.
    pub max_attempts: usize,
    /// Wall-clock budget for grid routing, leaving the rest of the job to whatever follows.
    pub max_seconds: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GridStats {
    pub routed: usize,
    pub failed: usize,
    pub vias: usize,
    /// Paths found whose copper did not join the two items (counted in `failed`, and removed).
    pub not_joined: usize,
}

pub fn route_open_connections(
    board: &mut Board,
    solver: &mut dyn FieldSolver,
    options: &GridOptions,
    stop: &dyn Fn() -> bool,
) -> GridStats {
    let started = std::time::Instant::now();
    let out_of_time = || {
        stop()
            || options
                .max_seconds
                .is_some_and(|limit| started.elapsed().as_secs_f64() > limit)
    };
    let mut stats = GridStats::default();
    let mut given_up: BTreeSet<(ItemId, ItemId)> = BTreeSet::new();
    let mut maps: BTreeMap<usize, ObstacleMap> = BTreeMap::new();
    let mut attempts = 0;
    'rounds: loop {
        let gaps: Vec<(ItemId, ItemId)> = copper_drc::DesignRulesChecker::new(board)
            .get_all_unconnected_items()
            .into_iter()
            .filter_map(|g| g.second_item.map(|second| (g.first_item, second)))
            .filter(|pair| !given_up.contains(pair))
            .collect();
        if gaps.is_empty() {
            break;
        }
        let routed_before = stats.routed;
        for (from, to) in gaps {
            if out_of_time() || attempts >= options.max_attempts {
                break 'rounds;
            }
            attempts += 1;
            let Some(net) = board
                .get_item(from)
                .and_then(|i| i.net_nos().first().copied())
            else {
                given_up.insert((from, to));
                continue;
            };
            if board.get_item(to).is_none() || board.connected_set(from, net, false).contains(&to) {
                continue;
            }
            let class = board
                .rules
                .nets
                .get(net)
                .map_or(usize::MAX, |n| n.net_class.0);
            let map = maps.entry(class).or_insert_with(|| {
                let mut map =
                    ObstacleMap::new(board, GridProfile::for_net(board, net), options.cell);
                let ids: Vec<ItemId> = board.get_items().map(Item::id).collect();
                map.stamp(board, &ids);
                map
            });
            let grid = RouteGrid::view(map, net);
            let sources = terminal_cells(board, &grid, from);
            let targets = terminal_cells(board, &grid, to);
            let cost = solver.solve(&grid, &sources, options.via_cost);
            let Some(path) = trace_back(&grid, &cost, &targets, options.via_cost) else {
                stats.failed += 1;
                given_up.insert((from, to));
                continue;
            };
            let existing: BTreeSet<ItemId> = board.get_items().map(Item::id).collect();
            let vias = commit(board, &grid, &path, from, to);
            let inserted: Vec<ItemId> = board
                .get_items()
                .map(Item::id)
                .filter(|id| !existing.contains(id))
                .collect();
            if board.connected_set(from, net, false).contains(&to) {
                stats.vias += vias;
                stats.routed += 1;
                for map in maps.values_mut() {
                    map.stamp(board, &inserted);
                }
            } else {
                board.remove_items(inserted);
                stats.failed += 1;
                stats.not_joined += 1;
                given_up.insert((from, to));
            }
        }
        if stats.routed == routed_before {
            break;
        }
    }
    stats
}

/// Free cells whose centre lies on the item, on the grid's routing layers.
pub fn terminal_cells(board: &mut Board, grid: &RouteGrid, item: ItemId) -> Vec<usize> {
    let layers: Vec<usize> = {
        let ctx = board.ctx();
        let Some(it) = board.get_item(item) else {
            return Vec::new();
        };
        (0..it.tile_shape_count(&ctx))
            .map(|i| it.shape_layer(i, &ctx))
            .collect()
    };
    let ends: Vec<FloatPoint> = match board.get_item(item) {
        Some(Item::Trace(trace)) => [trace.first_corner(), trace.last_corner()]
            .into_iter()
            .flatten()
            .map(|p| p.to_float())
            .collect(),
        _ => Vec::new(),
    };
    let mut cells = Vec::new();
    for (index, layer) in layers.into_iter().enumerate() {
        let Some(routing_layer) = grid.layers.iter().position(|&l| l == layer) else {
            continue;
        };
        let Some(shape) = board.item_tile_shape(item, index) else {
            continue;
        };
        let bb = copper_geometry::ShapeOps::bounding_box(&shape);
        let lo_i = ((f64::from(bb.ll.x) - grid.x0) / grid.h).floor().max(0.0) as usize;
        let lo_j = ((f64::from(bb.ll.y) - grid.y0) / grid.h).floor().max(0.0) as usize;
        let hi_i = (((f64::from(bb.ur.x) - grid.x0) / grid.h).ceil() as usize).min(grid.nx - 1);
        let hi_j = (((f64::from(bb.ur.y) - grid.y0) / grid.h).ceil() as usize).min(grid.ny - 1);
        for j in lo_j..=hi_j {
            for i in lo_i..=hi_i {
                let index = grid.index(routing_layer, i, j);
                let centre = grid.centre(i, j);
                let on_item = shape.distance(&centre) == 0.0;
                let near_end = ends.is_empty()
                    || ends
                        .iter()
                        .any(|e| (e.x - centre.x).hypot(e.y - centre.y) <= grid.h);
                if !grid.blocked[index] && on_item && near_end {
                    cells.push(index);
                }
            }
        }
        let anchors: Vec<FloatPoint> = if ends.is_empty() {
            board
                .drill_center(item)
                .map(|c| c.to_float())
                .into_iter()
                .collect()
        } else {
            ends.clone()
        };
        let reach =
            f64::from((bb.ur.x - bb.ll.x).max(bb.ur.y - bb.ll.y)) / 2.0 + STUB_CELLS * grid.h;
        for anchor in anchors {
            cells.extend(escape_stubs(
                board,
                grid,
                routing_layer,
                layer,
                &anchor,
                reach,
            ));
        }
    }
    cells.sort_unstable();
    cells.dedup();
    cells
}

/// Free cells within `reach` of `anchor` that a straight trace from the anchor reaches legally,
/// nearest first and at most `STUB_CANDIDATES` tried: how a path escapes a pad too small to hold
/// a free cell centre of its own. Legality is the board's exact check, with the trace widened by
/// however much the DRC's net-class rule exceeds the router's clearance matrix.
fn escape_stubs(
    board: &mut Board,
    grid: &RouteGrid,
    routing_layer: usize,
    layer: usize,
    anchor: &FloatPoint,
    reach: f64,
) -> Vec<usize> {
    let constraints = copper_drc::constraints::resolve(board);
    let strictest = constraints
        .netclass_clearance
        .values()
        .copied()
        .chain(constraints.min_clearance)
        .max()
        .unwrap_or(0);
    let matrix = board.rules.clearance_matrix.get_value(
        grid.trace_clearance_class,
        grid.trace_clearance_class,
        layer,
        false,
    );
    let half_width = grid.half_width + (strictest - matrix).max(0);
    let r = (reach / grid.h).ceil() as i64;
    let ci = ((anchor.x - grid.x0) / grid.h - 0.5).round() as i64;
    let cj = ((anchor.y - grid.y0) / grid.h - 0.5).round() as i64;
    let mut candidates: Vec<(f64, usize, FloatPoint)> = Vec::new();
    for j in (cj - r).max(0)..=(cj + r).min(grid.ny as i64 - 1) {
        for i in (ci - r).max(0)..=(ci + r).min(grid.nx as i64 - 1) {
            let index = grid.index(routing_layer, i as usize, j as usize);
            if grid.blocked[index] {
                continue;
            }
            let centre = grid.centre(i as usize, j as usize);
            let d = (centre.x - anchor.x).hypot(centre.y - anchor.y);
            if d <= reach {
                candidates.push((d, index, centre));
            }
        }
    }
    candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
    let start = Point::Int(IntPoint::new(
        anchor.x.round() as i32,
        anchor.y.round() as i32,
    ));
    let mut found = Vec::new();
    for (_, index, centre) in candidates.into_iter().take(STUB_CANDIDATES) {
        let end = Point::Int(IntPoint::new(
            centre.x.round() as i32,
            centre.y.round() as i32,
        ));
        let Some(shape) = Polyline::from_points(&[start.clone(), end]).offset_shape(half_width, 0)
        else {
            continue;
        };
        if board.check_trace_shape(&shape, layer, &[grid.net], grid.trace_clearance_class, None) {
            found.push(index);
        }
    }
    found
}

/// The exact point a trace must end on to contact `item`: a drill item's centre, or the end
/// corner of a trace nearest `near` (the board's contact rule).
fn anchor(board: &Board, item: ItemId, near: &FloatPoint) -> Option<Point> {
    match board.get_item(item)? {
        Item::Pin(_) | Item::Via(_) => board.drill_center(item),
        Item::Trace(trace) => [trace.first_corner(), trace.last_corner()]
            .into_iter()
            .flatten()
            .min_by(|a, b| {
                let (a, b) = (a.to_float(), b.to_float());
                (a.x - near.x)
                    .hypot(a.y - near.y)
                    .total_cmp(&(b.x - near.x).hypot(b.y - near.y))
            }),
        _ => None,
    }
}

/// Inserts the path as traces (collinear runs merged) and vias, its ends extended to the
/// terminals' anchors; returns the vias inserted.
pub(crate) fn commit(
    board: &mut Board,
    grid: &RouteGrid,
    path: &GridPath,
    from: ItemId,
    to: ItemId,
) -> usize {
    let point = |&(_, i, j): &(usize, usize, usize)| {
        let c = grid.centre(i, j);
        Point::Int(IntPoint::new(c.x.round() as i32, c.y.round() as i32))
    };
    let mut runs: Vec<(usize, Vec<Point>)> = Vec::new();
    let mut vias = Vec::new();
    let mut run: Vec<(usize, usize, usize)> = Vec::new();
    let close = |run: &[(usize, usize, usize)], runs: &mut Vec<(usize, Vec<Point>)>| {
        let mut points = vec![point(&run[0])];
        for k in 1..run.len().saturating_sub(1) {
            let (a, b, c) = (run[k - 1], run[k], run[k + 1]);
            let turn = (b.1 as i64 - a.1 as i64, b.2 as i64 - a.2 as i64)
                != (c.1 as i64 - b.1 as i64, c.2 as i64 - b.2 as i64);
            if turn {
                points.push(point(&b));
            }
        }
        if run.len() > 1 {
            points.push(point(&run[run.len() - 1]));
        }
        runs.push((run[0].0, points));
    };
    for step in path {
        if let Some(last) = run.last().copied() {
            if last.0 != step.0 {
                close(&run, &mut runs);
                vias.push(point(step));
                run.clear();
            }
        }
        run.push(*step);
    }
    close(&run, &mut runs);

    if let (Some(first), Some(start)) = (runs.first_mut(), path.first()) {
        if let Some(a) = anchor(board, from, &grid.centre(start.1, start.2)) {
            if first.1.first() != Some(&a) {
                first.1.insert(0, a);
            }
        }
    }
    if let (Some(last), Some(end)) = (runs.last_mut(), path.last()) {
        if let Some(a) = anchor(board, to, &grid.centre(end.1, end.2)) {
            if last.1.last() != Some(&a) {
                last.1.push(a);
            }
        }
    }
    for (layer, points) in runs {
        if points.len() >= 2 {
            board.insert_trace(
                Polyline::from_points(&points),
                grid.layers[layer],
                grid.half_width,
                vec![grid.net],
                grid.trace_clearance_class,
                FixedState::Unfixed,
            );
        }
    }
    let mut inserted = 0;
    if let Some(via) = &grid.via {
        for at in vias {
            if board
                .insert_via(
                    via.padstack,
                    at,
                    vec![grid.net],
                    via.clearance_class,
                    FixedState::Unfixed,
                    false,
                )
                .is_ok()
            {
                inserted += 1;
            }
        }
    }
    inserted
}
