//! Negotiated congestion (PathFinder, McMurchie and Ebeling 1995) on the conservative grid.
//!
//! Fixed copper stays hard-blocked. Routes found during negotiation are not: each net claims a
//! zone of cells around its paths, wide enough that two nets outside each other's zones are
//! legal on the board, and entering a cell inside another net's zone costs a present-congestion
//! price plus an accumulated history price. A net's own zone is lifted before it re-routes.
//! Rounds repeat, with the present price rising and history accumulating where nets still
//! collide, until no path lies in another net's zone; only then is anything committed, so the
//! result is as legal as single-net grid routing.

use std::collections::{BTreeMap, BTreeSet};

use copper_board::items::Item;
use copper_board::{Board, ItemId};

use crate::grid::{GridProfile, ObstacleMap, RouteGrid};
use crate::route::{FieldSolver, commit, terminal_cells};
use crate::search::{GridPath, trace_back};

/// Half a cell diagonal, as a fraction of the cell: how far a step strays from its centre.
const STEP_REACH: f64 = std::f64::consts::FRAC_1_SQRT_2;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NegotiationOptions {
    pub cell: f64,
    pub via_cost: u32,
    pub max_rounds: usize,
    /// Price per other net covering a cell, in the first round; it grows by `present_growth`.
    pub present: f64,
    pub present_growth: f64,
    /// Added to a cell's history each round a path there collides.
    pub history_step: u32,
    pub max_seconds: Option<f64>,
    /// Worker threads for a round; above one, every net routes against the previous round's
    /// congestion at once (Jacobi PathFinder) instead of one after another (Gauss-Seidel).
    pub threads: usize,
}

impl Default for NegotiationOptions {
    fn default() -> Self {
        NegotiationOptions {
            cell: 1.0,
            via_cost: 200,
            max_rounds: 30,
            present: 20.0,
            present_growth: 1.6,
            history_step: 10,
            max_seconds: None,
            threads: 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NegotiationStats {
    pub connections: usize,
    pub rounds: usize,
    /// Connections whose paths still collided when negotiation stopped.
    pub conflicted: usize,
    pub committed: usize,
    pub unroutable: usize,
    pub vias: usize,
}

struct Connection {
    net: i32,
    from: ItemId,
    to: ItemId,
    class: usize,
    path: Option<GridPath>,
    zone: Vec<usize>,
}

/// Which nets cover each node, through per-net multiplicities so that two routes of one net
/// may overlap freely.
struct Zones {
    count: Vec<u16>,
    per_net: BTreeMap<i32, BTreeMap<usize, u32>>,
}

impl Zones {
    fn add(&mut self, net: i32, cells: &[usize]) {
        let mine = self.per_net.entry(net).or_default();
        for &c in cells {
            let m = mine.entry(c).or_insert(0);
            if *m == 0 {
                self.count[c] = self.count[c].saturating_add(1);
            }
            *m += 1;
        }
    }

    fn remove(&mut self, net: i32, cells: &[usize]) {
        let Some(mine) = self.per_net.get_mut(&net) else {
            return;
        };
        for &c in cells {
            if let Some(m) = mine.get_mut(&c) {
                *m -= 1;
                if *m == 0 {
                    mine.remove(&c);
                    self.count[c] = self.count[c].saturating_sub(1);
                }
            }
        }
    }

    /// Nets other than `net` covering node `c`.
    fn others(&self, net: i32, c: usize) -> u16 {
        let own = self.per_net.get(&net).is_some_and(|m| m.contains_key(&c));
        self.count[c] - u16::from(own)
    }
}

pub fn route_negotiated(
    board: &mut Board,
    solver: &mut dyn FieldSolver,
    options: &NegotiationOptions,
    stop: &dyn Fn() -> bool,
) -> NegotiationStats {
    let started = std::time::Instant::now();
    let out_of_time = || {
        stop()
            || options
                .max_seconds
                .is_some_and(|limit| started.elapsed().as_secs_f64() > limit)
    };
    let mut stats = NegotiationStats::default();

    let open_nets: BTreeSet<i32> = copper_drc::DesignRulesChecker::new(board)
        .get_all_unconnected_items()
        .into_iter()
        .filter_map(|g| board.get_item(g.first_item)?.net_nos().first().copied())
        .collect();
    let mut connections: Vec<Connection> = Vec::new();
    for net in open_nets {
        let class = board
            .rules
            .nets
            .get(net)
            .map_or(usize::MAX, |n| n.net_class.0);
        for (from, to) in spanning_connections(board, net) {
            connections.push(Connection {
                net,
                from,
                to,
                class,
                path: None,
                zone: Vec::new(),
            });
        }
    }
    stats.connections = connections.len();
    if connections.is_empty() {
        return stats;
    }

    let mut maps: BTreeMap<usize, ObstacleMap> = BTreeMap::new();
    for c in &connections {
        if !maps.contains_key(&c.class) {
            let mut map = ObstacleMap::new(board, GridProfile::for_net(board, c.net), options.cell);
            let ids: Vec<ItemId> = board.get_items().map(Item::id).collect();
            map.stamp(board, &ids);
            maps.insert(c.class, map);
        }
    }
    let sample = RouteGrid::view(maps.values().next().expect("one map per class"), 0);
    let (nx, ny, layers) = (sample.nx, sample.ny, sample.layers.len());
    let cells = nx * ny;
    let (trace_disc, via_disc) = zone_discs(board, &maps, options.cell);

    let mut zones = Zones {
        count: vec![0; layers * cells],
        per_net: BTreeMap::new(),
    };
    let mut history = vec![0u32; layers * cells];
    let mut present = options.present;
    let terminals: Vec<(Vec<usize>, Vec<usize>)> = connections
        .iter()
        .map(|c| {
            let grid = RouteGrid::view(&maps[&c.class], c.net);
            (
                terminal_cells(board, &grid, c.from),
                terminal_cells(board, &grid, c.to),
            )
        })
        .collect();

    for round in 0..options.max_rounds {
        if out_of_time() {
            break;
        }
        stats.rounds = round + 1;
        if options.threads > 1 {
            route_round_in_parallel(
                &mut connections,
                &maps,
                &terminals,
                &zones,
                &history,
                present,
                options,
                options
                    .max_seconds
                    .map(|limit| started + std::time::Duration::from_secs_f64(limit)),
            );
            zones = Zones {
                count: vec![0; layers * cells],
                per_net: BTreeMap::new(),
            };
            for c in connections.iter_mut() {
                c.zone = c
                    .path
                    .as_ref()
                    .map(|p| zone_of(p, nx, ny, layers, &trace_disc, &via_disc))
                    .unwrap_or_default();
                zones.add(c.net, &c.zone);
            }
        } else {
            for (k, c) in connections.iter_mut().enumerate() {
                if out_of_time() {
                    break;
                }
                zones.remove(c.net, &c.zone);
                let mut grid = RouteGrid::view(&maps[&c.class], c.net);
                grid.penalty = penalties(&zones, &history, present, c.net);
                let (sources, targets) = &terminals[k];
                let cost = solver.solve(&grid, sources, options.via_cost);
                c.path = trace_back(&grid, &cost, targets, options.via_cost);
                c.zone = c
                    .path
                    .as_ref()
                    .map(|p| zone_of(p, nx, ny, layers, &trace_disc, &via_disc))
                    .unwrap_or_default();
                zones.add(c.net, &c.zone);
            }
        }
        let mut colliding = 0;
        for c in &connections {
            let Some(path) = &c.path else {
                continue;
            };
            let mut collides = false;
            for &(layer, i, j) in path {
                let n = layer * cells + j * nx + i;
                if zones.others(c.net, n) > 0 {
                    history[n] += options.history_step;
                    collides = true;
                }
            }
            colliding += usize::from(collides);
        }
        stats.conflicted = colliding;
        if colliding == 0 {
            break;
        }
        present *= options.present_growth;
    }

    // Commit greedily: a route whose cells lie in no committed net's zone is legal against
    // everything committed so far, converged or not.
    let mut committed = Zones {
        count: vec![0; layers * cells],
        per_net: BTreeMap::new(),
    };
    for c in &connections {
        let Some(path) = &c.path else {
            stats.unroutable += 1;
            continue;
        };
        if path
            .iter()
            .any(|&(l, i, j)| committed.others(c.net, l * cells + j * nx + i) > 0)
        {
            continue;
        }
        if board.get_item(c.from).is_none()
            || board.get_item(c.to).is_none()
            || board.connected_set(c.from, c.net, false).contains(&c.to)
        {
            continue;
        }
        let grid = RouteGrid::view(&maps[&c.class], c.net);
        let existing: BTreeSet<ItemId> = board.get_items().map(Item::id).collect();
        let Some(vias) = commit(board, &grid, path, c.from, c.to) else {
            continue;
        };
        if board.connected_set(c.from, c.net, false).contains(&c.to) {
            committed.add(c.net, &c.zone);
            stats.committed += 1;
            stats.vias += vias;
        } else {
            let inserted: Vec<ItemId> = board
                .get_items()
                .map(Item::id)
                .filter(|id| !existing.contains(id))
                .collect();
            board.remove_items(inserted);
        }
    }
    stats
}

fn penalties(zones: &Zones, history: &[u32], present: f64, net: i32) -> Vec<u32> {
    (0..history.len())
        .map(|n| history[n] + (present * f64::from(zones.others(net, n))).round() as u32)
        .collect()
}

/// One Jacobi round: every connection routes on the CPU against the same congestion snapshot,
/// spread over `options.threads` workers.
#[allow(clippy::too_many_arguments)]
fn route_round_in_parallel(
    connections: &mut [Connection],
    maps: &BTreeMap<usize, ObstacleMap>,
    terminals: &[(Vec<usize>, Vec<usize>)],
    zones: &Zones,
    history: &[u32],
    present: f64,
    options: &NegotiationOptions,
    deadline: Option<std::time::Instant>,
) {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let jobs: Vec<(i32, usize)> = connections.iter().map(|c| (c.net, c.class)).collect();
    let paths: Vec<std::sync::Mutex<Option<GridPath>>> = (0..connections.len())
        .map(|_| std::sync::Mutex::new(None))
        .collect();
    std::thread::scope(|scope| {
        for _ in 0..options.threads {
            scope.spawn(|| {
                loop {
                    let k = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if k >= jobs.len() || deadline.is_some_and(|d| std::time::Instant::now() > d) {
                        break;
                    }
                    let (net, class) = jobs[k];
                    let mut grid = RouteGrid::view(&maps[&class], net);
                    grid.penalty = penalties(zones, history, present, net);
                    let (sources, targets) = &terminals[k];
                    let cost = crate::search::cost_field(&grid, sources, options.via_cost);
                    *paths[k].lock().expect("no worker panics") =
                        trace_back(&grid, &cost, targets, options.via_cost);
                }
            });
        }
    });
    for (c, path) in connections.iter_mut().zip(paths) {
        if let Some(path) = path.into_inner().expect("no worker panics") {
            c.path = Some(path);
        }
    }
}

/// The connections that would join all of a net's copper: a minimum spanning tree (Prim) over
/// its connected sets, each edge between the nearest anchor points of two sets.
fn spanning_connections(board: &Board, net: i32) -> Vec<(ItemId, ItemId)> {
    let anchors = |set: &BTreeSet<ItemId>| -> Vec<(ItemId, f64, f64)> {
        set.iter()
            .flat_map(|&id| {
                let points: Vec<copper_geometry::Point> = match board.get_item(id) {
                    Some(Item::Pin(_) | Item::Via(_)) => {
                        board.drill_center(id).into_iter().collect()
                    }
                    Some(Item::Trace(t)) => [t.first_corner(), t.last_corner()]
                        .into_iter()
                        .flatten()
                        .collect(),
                    _ => Vec::new(),
                };
                points.into_iter().map(move |p| {
                    let f = p.to_float();
                    (id, f.x, f.y)
                })
            })
            .collect()
    };
    let sets: Vec<Vec<(ItemId, f64, f64)>> = board
        .get_connected_sets(net)
        .iter()
        .map(anchors)
        .filter(|a| !a.is_empty())
        .collect();
    if sets.len() < 2 {
        return Vec::new();
    }
    let mut joined = vec![false; sets.len()];
    joined[0] = true;
    let mut edges = Vec::new();
    for _ in 1..sets.len() {
        let mut best: Option<(f64, usize, ItemId, ItemId)> = None;
        for (a, set_a) in sets.iter().enumerate().filter(|(a, _)| joined[*a]) {
            for (b, set_b) in sets.iter().enumerate().filter(|(b, _)| !joined[*b]) {
                for &(ia, xa, ya) in set_a {
                    for &(ib, xb, yb) in set_b {
                        let d = (xa - xb).hypot(ya - yb);
                        if best.is_none_or(|(bd, ..)| d < bd) {
                            best = Some((d, b, ia, ib));
                        }
                    }
                }
                let _ = a;
            }
        }
        let Some((_, b, ia, ib)) = best else {
            break;
        };
        joined[b] = true;
        edges.push((ia, ib));
    }
    edges
}

/// Cell offsets within the zone radius of a trace cell and of a via cell: the widest trace and
/// via on the board against the widest trace, at the largest clearance, plus each side's step
/// reach.
fn zone_discs(
    board: &Board,
    maps: &BTreeMap<usize, ObstacleMap>,
    h: f64,
) -> (Vec<(i32, i32)>, Vec<(i32, i32)>) {
    let widest = maps
        .values()
        .map(|m| f64::from(m.profile.half_width))
        .fold(0.0, f64::max);
    let via = maps
        .values()
        .filter_map(|m| m.profile.via.as_ref().map(|v| v.radius))
        .fold(0.0, f64::max);
    let constraints = copper_drc::constraints::resolve(board);
    let rule_values = constraints
        .netclass_clearance
        .values()
        .copied()
        .chain(constraints.min_clearance);
    let matrix = &board.rules.clearance_matrix;
    let matrix_max = (0..matrix.get_class_count())
        .flat_map(|a| (0..matrix.get_class_count()).map(move |b| (a, b)))
        .flat_map(|(a, b)| {
            (0..board.get_layer_count()).map(move |l| matrix.get_value(a, b, l, false))
        })
        .max()
        .unwrap_or(0);
    let clearance = f64::from(
        rule_values
            .chain(std::iter::once(matrix_max))
            .max()
            .unwrap_or(0),
    );
    let trace_reach = 2.0 * widest + clearance + 2.0 * STEP_REACH * h;
    let via_reach = via + widest.max(via) + clearance + 2.0 * STEP_REACH * h;
    let disc = |reach: f64| {
        let r = (reach / h).ceil() as i32;
        (-r..=r)
            .flat_map(|dj| (-r..=r).map(move |di| (di, dj)))
            .filter(|&(di, dj)| f64::from(di).hypot(f64::from(dj)) * h < reach + h)
            .collect()
    };
    (disc(trace_reach), disc(via_reach))
}

/// The nodes a path claims: a disc around each cell on its layer, and a via disc on every layer
/// where it changes layer. Deduplicated.
fn zone_of(
    path: &GridPath,
    nx: usize,
    ny: usize,
    layers: usize,
    trace_disc: &[(i32, i32)],
    via_disc: &[(i32, i32)],
) -> Vec<usize> {
    let cells = nx * ny;
    let mut out = BTreeSet::new();
    let mut stamp = |layer: usize, i: usize, j: usize, disc: &[(i32, i32)]| {
        for &(di, dj) in disc {
            let (a, b) = (i as i32 + di, j as i32 + dj);
            if a >= 0 && b >= 0 && (a as usize) < nx && (b as usize) < ny {
                out.insert(layer * cells + b as usize * nx + a as usize);
            }
        }
    };
    for (k, &(layer, i, j)) in path.iter().enumerate() {
        stamp(layer, i, j, trace_disc);
        if k > 0 && path[k - 1].0 != layer {
            for l in 0..layers {
                stamp(l, i, j, via_disc);
            }
        }
    }
    out.into_iter().collect()
}
