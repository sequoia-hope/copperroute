#![forbid(unsafe_code)]
//! Topological routing. Each layer is a Delaunay triangulation of the copper's outlines, sampled
//! closely enough that no wire fits between two neighbouring samples of one obstacle, so the
//! outlines are walls. A connection searches the triangles for a channel wide enough for it,
//! changing layers where a via fits; its copper is the shortest path through that channel that
//! keeps the clearances (the funnel algorithm, each bend pushed out round the corner it wraps),
//! which is a rubber band pulled taut. Committed copper joins the triangulations, so later wires
//! go round it.

mod funnel;
mod world;

use std::cmp::Ordering;
use std::collections::{BTreeSet, BinaryHeap, HashMap};

use copper_board::items::Item;
use copper_board::{Board, FixedState, ItemId};
use copper_board::DrcConstraints;
use copper_drc::constraints::resolve;
use copper_geometry::{FloatPoint, IntPoint, Point, Polyline};
use copper_gpu::grid::GridProfile;

use funnel::{Bend, Gate};
use world::{Face, World};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TopoOptions {
    /// Spacing of the samples along each obstacle's outline, in board units.
    pub sample: f64,
    /// What a via costs, as a length in board units.
    pub via_cost: f64,
    pub max_seconds: Option<f64>,
    /// Triangles one search may expand before it gives the connection up.
    pub max_expansions: usize,
    /// Orders to try the connections in; the board keeps the one that routes most.
    pub orders: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TopoStats {
    pub connections: usize,
    pub routed: usize,
    /// No channel wide enough was found.
    pub no_channel: usize,
    /// A channel was found but its copper broke a clearance or didn't join the two items.
    pub rejected: usize,
    pub vias: usize,
}

pub fn route_topological(board: &mut Board, options: &TopoOptions, stop: &dyn Fn() -> bool) -> TopoStats {
    let started = std::time::Instant::now();
    let out_of_time = || stop() || options.max_seconds.is_some_and(|limit| started.elapsed().as_secs_f64() > limit);
    let constraints = resolve(board);
    let mut gaps: Vec<(f64, ItemId, ItemId)> = copper_drc::DesignRulesChecker::new(board)
        .get_all_unconnected_items()
        .into_iter()
        .filter_map(|g| {
            let second = g.second_item?;
            let a = board.drill_center(g.first_item)?.to_float();
            let b = board.drill_center(second)?.to_float();
            Some(((a.x - b.x).hypot(a.y - b.y), g.first_item, second))
        })
        .collect();
    gaps.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut best: Option<(TopoStats, Board)> = None;
    for order in 0..options.orders.max(1) {
        if order > 0 && out_of_time() {
            break;
        }
        let mut trial = board.clone();
        let stats = route_in_order(&mut trial, &constraints, &reorder(&gaps, order), options, &out_of_time);
        tracing::info!(
            "topo order {order}: {} of {} connections routed ({} without a channel, {} rejected), {} vias, {:.1} s",
            stats.routed,
            stats.connections,
            stats.no_channel,
            stats.rejected,
            stats.vias,
            started.elapsed().as_secs_f64()
        );
        if best.as_ref().is_none_or(|(b, _)| (stats.routed, std::cmp::Reverse(stats.vias)) > (b.routed, std::cmp::Reverse(b.vias))) {
            best = Some((stats, trial));
        }
    }
    let Some((stats, routed)) = best else {
        return TopoStats::default();
    };
    *board = routed;
    stats
}

/// The connections in the order a trial routes them: shortest first, then longest first, then
/// shuffled with the trial's number as seed.
fn reorder(gaps: &[(f64, ItemId, ItemId)], order: usize) -> Vec<(ItemId, ItemId)> {
    let mut out: Vec<(ItemId, ItemId)> = gaps.iter().map(|&(_, a, b)| (a, b)).collect();
    match order {
        0 => {}
        1 => out.reverse(),
        _ => {
            let mut state = order as u64 * 0x9e37_79b9_7f4a_7c15;
            for i in (1..out.len()).rev() {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                out.swap(i, (state % (i as u64 + 1)) as usize);
            }
        }
    }
    out
}

fn route_in_order(
    board: &mut Board,
    constraints: &DrcConstraints,
    gaps: &[(ItemId, ItemId)],
    options: &TopoOptions,
    out_of_time: &dyn Fn() -> bool,
) -> TopoStats {
    let mut world = World::build(board, options.sample, constraints);
    let mut stats = TopoStats { connections: gaps.len(), ..TopoStats::default() };
    for &(from, to) in gaps {
        if out_of_time() {
            break;
        }
        let Some(net) = board.get_item(from).and_then(|i| i.net_nos().first().copied()) else {
            continue;
        };
        if board.get_item(to).is_none() || board.connected_set(from, net, false).contains(&to) {
            continue;
        }
        let mut found = false;
        let mut committed = None;
        for widen in MARGINS {
            let wire = WireRules::new(board, constraints, net, options.sample * widen);
            let Some(route) = search(board, &world, &wire, from, to, options) else {
                break;
            };
            found = true;
            committed = commit(board, &world, &wire, &route, from, to);
            if committed.is_some() {
                break;
            }
        }
        match committed {
            Some((added, vias)) => {
                stats.routed += 1;
                stats.vias += vias;
                world.add_items(board, &added, constraints);
            }
            None if found => stats.rejected += 1,
            None => stats.no_channel += 1,
        }
    }
    stats
}

/// What one net's wire needs: its width, clearances and via.
struct WireRules<'a> {
    net: i32,
    profile: GridProfile,
    constraints: &'a DrcConstraints,
    /// How far a sampled outline may bulge between its samples.
    margin: f64,
}

impl<'a> WireRules<'a> {
    fn new(board: &Board, constraints: &'a DrcConstraints, net: i32, margin: f64) -> WireRules<'a> {
        WireRules { net, profile: GridProfile::for_net(board, net), constraints, margin }
    }

    /// Narrower half-widths a leg at a pad may neck down to, never below the board's minimum.
    fn necked_widths(&self) -> Vec<i32> {
        let Some(floor) = self.constraints.min_track_width.map(|w| (w + 1) / 2) else {
            return Vec::new();
        };
        let full = self.profile.half_width;
        let mut widths: Vec<i32> = [full * 3 / 4, full / 2].into_iter().filter(|&w| w > floor).collect();
        if floor < full {
            widths.push(floor);
        }
        widths
    }

    fn half_width(&self) -> f64 {
        f64::from(self.profile.half_width)
    }

    /// How near the wire's centre may come to an obstacle of `class` whose net has `rule`.
    fn reach(&self, board: &Board, class: usize, rule: i32, layer: usize) -> f64 {
        let own = self.profile.trace_clearance_class;
        let clearance = self
            .profile
            .clearance(board, self.constraints, own, class, layer, rule)
            .max(f64::from(board.clearance_value(own, class, layer)));
        self.half_width() + clearance + self.margin
    }

    fn via_reach(&self, board: &Board, class: usize, rule: i32, layer: usize) -> Option<f64> {
        let via = self.profile.via.as_ref()?;
        let clearance = self
            .profile
            .clearance(board, self.constraints, via.clearance_class, class, layer, rule)
            .max(f64::from(board.clearance_value(via.clearance_class, class, layer)));
        Some(via.radius + clearance + self.margin)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct State {
    layer: usize,
    face: Face,
}

#[derive(Clone)]
enum Step {
    Start(FloatPoint),
    Cross(Gate),
    Via(FloatPoint),
}

struct Open {
    f: f64,
    g: f64,
    state: State,
    at: FloatPoint,
}

impl PartialEq for Open {
    fn eq(&self, other: &Self) -> bool {
        self.f == other.f
    }
}
impl Eq for Open {}
impl PartialOrd for Open {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Open {
    fn cmp(&self, other: &Self) -> Ordering {
        other.f.total_cmp(&self.f)
    }
}

/// One layer's run of a route: where it starts, the gates it passes, where it ends.
struct Run {
    layer: usize,
    from: FloatPoint,
    gates: Vec<Gate>,
    to: FloatPoint,
}

struct Route {
    runs: Vec<Run>,
}

const HEURISTIC_WEIGHT: f64 = 1.2;
/// The margins a connection is tried with, in samples, widening each time its copper clashes.
const MARGINS: [f64; 3] = [0.5, 1.5, 3.0];

fn search(board: &Board, world: &World, wire: &WireRules<'_>, from: ItemId, to: ItemId, options: &TopoOptions) -> Option<Route> {
    let ends = |item: ItemId| -> Vec<(State, FloatPoint)> {
        let net = wire.net;
        let mut out = Vec::new();
        for id in board.connected_set(item, net, false) {
            let Some(centre) = board.drill_center(id).map(|c| c.to_float()) else {
                continue;
            };
            let Some(it) = board.get_item(id) else {
                continue;
            };
            let ctx = board.ctx();
            let layers: BTreeSet<usize> = (0..it.tile_shape_count(&ctx)).map(|i| it.shape_layer(i, &ctx)).collect();
            for layer in layers.into_iter().filter(|l| wire.profile.layers.contains(l)) {
                if let Some(face) = world.face_at(layer, &centre) {
                    out.push((State { layer, face }, centre));
                }
            }
        }
        out
    };
    let sources = ends(from);
    let targets = ends(to);
    if sources.is_empty() || targets.is_empty() {
        return None;
    }
    let goal: HashMap<State, FloatPoint> = targets.iter().copied().collect();
    let goal_points: Vec<FloatPoint> = targets.iter().map(|(_, p)| *p).collect();
    let h = |p: &FloatPoint| {
        goal_points.iter().map(|q| (p.x - q.x).hypot(p.y - q.y)).fold(f64::INFINITY, f64::min) * HEURISTIC_WEIGHT
    };

    let mut best: HashMap<State, f64> = HashMap::new();
    let mut parent: HashMap<State, (Option<State>, Step)> = HashMap::new();
    let mut open = BinaryHeap::new();
    for (state, at) in &sources {
        best.insert(*state, 0.0);
        parent.insert(*state, (None, Step::Start(*at)));
        open.push(Open { f: h(at), g: 0.0, state: *state, at: *at });
    }
    let mut via_ok: HashMap<State, Option<FloatPoint>> = HashMap::new();
    let mut expanded = 0;
    while let Some(Open { g, state, at, .. }) = open.pop() {
        if best.get(&state).is_some_and(|b| *b < g) {
            continue;
        }
        if let Some(end) = goal.get(&state) {
            return Some(unwind(&parent, state, *end));
        }
        expanded += 1;
        if expanded > options.max_expansions {
            return None;
        }
        let mut push = |next: State, g2: f64, at2: FloatPoint, step: Step| {
            if best.get(&next).is_none_or(|b| g2 < *b) {
                best.insert(next, g2);
                parent.insert(next, (Some(state), step));
                open.push(Open { f: g2 + h(&at2), g: g2, state: next, at: at2 });
            }
        };
        for (face, gate) in world.gates(board, wire, state.layer, state.face) {
            let p = gate.nearest(&at);
            let next = State { layer: state.layer, face };
            push(next, g + (p.x - at.x).hypot(p.y - at.y), p, Step::Cross(gate));
        }
        let via = *via_ok.entry(state).or_insert_with(|| world.via_site(board, wire, state.layer, state.face));
        if let Some(site) = via {
            for &layer in &wire.profile.layers {
                if layer == state.layer {
                    continue;
                }
                if let Some(face) = world.face_at(layer, &site) {
                    let g2 = g + (site.x - at.x).hypot(site.y - at.y) + options.via_cost;
                    push(State { layer, face }, g2, site, Step::Via(site));
                }
            }
        }
    }
    None
}

fn unwind(parent: &HashMap<State, (Option<State>, Step)>, last: State, end: FloatPoint) -> Route {
    let mut steps = Vec::new();
    let mut at = Some(last);
    while let Some(state) = at {
        let (prev, step) = parent[&state].clone();
        steps.push((state.layer, step));
        at = prev;
    }
    steps.reverse();
    let mut runs: Vec<Run> = Vec::new();
    for (layer, step) in steps {
        match step {
            Step::Start(p) => runs.push(Run { layer, from: p, gates: Vec::new(), to: p }),
            Step::Cross(gate) => runs.last_mut().expect("a route starts").gates.push(gate),
            Step::Via(site) => {
                runs.last_mut().expect("a route starts").to = site;
                runs.push(Run { layer, from: site, gates: Vec::new(), to: site });
            }
        }
    }
    runs.last_mut().expect("a route starts").to = end;
    Route { runs }
}

/// Inserts the route's traces and vias; returns the items added and the vias among them, or
/// `None`, leaving the board as it was, when its copper breaks a clearance or doesn't join.
fn commit(board: &mut Board, world: &World, wire: &WireRules<'_>, route: &Route, from: ItemId, to: ItemId) -> Option<(Vec<ItemId>, usize)> {
    let existing: BTreeSet<ItemId> = board.get_items().map(Item::id).collect();
    let round = |p: &FloatPoint| Point::Int(IntPoint::new(p.x.round() as i32, p.y.round() as i32));
    let mut vias = 0;
    for (k, run) in route.runs.iter().enumerate() {
        let pulled: Vec<FloatPoint> = funnel::pull(&run.from, &run.gates, &run.to)
            .into_iter()
            .flat_map(|bend: Bend| bend.points())
            .collect();
        let corners = world.clear_path(board, wire, run.layer, pulled);
        let mut points: Vec<Point> = Vec::new();
        for c in &corners {
            let q = round(c);
            if points.last() != Some(&q) {
                points.push(q);
            }
        }
        let fits = |board: &mut Board, pair: &[Point], half_width: i32| {
            Polyline::from_points(pair)
                .offset_shape(half_width, 0)
                .is_some_and(|shape| board.check_trace_shape(&shape, run.layer, &[wire.net], wire.profile.trace_clearance_class, None))
        };
        let last = points.len().saturating_sub(2);
        let mut widths: Vec<i32> = Vec::with_capacity(points.len());
        for (segment, pair) in points.windows(2).enumerate() {
            let at_pad = (k == 0 && segment == 0) || (k + 1 == route.runs.len() && segment == last);
            let narrower = if at_pad { wire.necked_widths() } else { Vec::new() };
            let Some(width) = std::iter::once(wire.profile.half_width).chain(narrower).find(|&w| fits(board, pair, w)) else {
                tracing::debug!(
                    "topo: net {} rejected: segment {segment} of {} on layer {} clashes, {:?} to {:?}",
                    wire.net,
                    points.len() - 1,
                    run.layer,
                    pair[0],
                    pair[1]
                );
                if tracing::enabled!(tracing::Level::DEBUG) {
                    explain_clash(board, wire, run.layer, &pair[0].to_float(), &pair[1].to_float());
                }
                let added: Vec<ItemId> = board.get_items().map(Item::id).filter(|id| !existing.contains(id)).collect();
                board.remove_items(added);
                return None;
            };
            widths.push(width);
        }
        let mut start = 0;
        while start < widths.len() {
            let mut end = start + 1;
            while end < widths.len() && widths[end] == widths[start] {
                end += 1;
            }
            board.insert_trace(
                Polyline::from_points(&points[start..=end]),
                run.layer,
                widths[start],
                vec![wire.net],
                wire.profile.trace_clearance_class,
                FixedState::Unfixed,
            );
            start = end;
        }
        if k + 1 < route.runs.len() {
            let via = wire.profile.via.as_ref()?;
            match board.insert_via(via.padstack, round(&run.to), vec![wire.net], via.clearance_class, FixedState::Unfixed, false) {
                Ok(id) if keeps_clearance(board, id) => vias += 1,
                _ => {
                    tracing::debug!("topo: net {} rejected: its via clashes", wire.net);
                    let added: Vec<ItemId> = board.get_items().map(Item::id).filter(|id| !existing.contains(id)).collect();
                    board.remove_items(added);
                    return None;
                }
            }
        }
    }
    let added: Vec<ItemId> = board.get_items().map(Item::id).filter(|id| !existing.contains(id)).collect();
    if !board.connected_set(from, wire.net, false).contains(&to) {
        tracing::debug!("topo: net {} rejected: its copper doesn't join", wire.net);
        board.remove_items(added);
        return None;
    }
    Some((added, vias))
}

fn keeps_clearance(board: &mut Board, id: ItemId) -> bool {
    let (shapes, nets, class) = {
        let ctx = board.ctx();
        let Some(item) = board.get_item(id) else {
            return true;
        };
        let shapes: Vec<(usize, usize)> = (0..item.tile_shape_count(&ctx)).map(|i| (i, item.shape_layer(i, &ctx))).collect();
        (shapes, item.net_nos().to_vec(), item.clearance_class())
    };
    shapes.into_iter().all(|(index, layer)| {
        board
            .item_tile_shape(id, index)
            .is_none_or(|shape| board.check_trace_shape(&shape, layer, &nets, class, None))
    })
}

fn explain_clash(board: &Board, wire: &WireRules<'_>, layer: usize, a: &FloatPoint, b: &FloatPoint) {
    let ctx = board.ctx();
    for item in board.get_items() {
        if !item.is_trace_obstacle(wire.net) {
            continue;
        }
        for index in 0..item.tile_shape_count(&ctx) {
            if item.shape_layer(index, &ctx) != layer {
                continue;
            }
            let Some(shape) = board.item_tile_shape_ref(item.id(), index) else {
                continue;
            };
            let polygon = shape.corner_approx_arr();
            let gap = polygon
                .iter()
                .map(|p| segment_distance(a, b, p))
                .chain((0..polygon.len()).map(|k| {
                    let (c, d) = (polygon[k], polygon[(k + 1) % polygon.len()]);
                    segment_distance(&c, &d, a).min(segment_distance(&c, &d, b))
                }))
                .fold(f64::INFINITY, f64::min);
            let need = wire.half_width() + f64::from(board.clearance_value(wire.profile.trace_clearance_class, item.clearance_class(), layer));
            if gap < need {
                tracing::debug!(
                    "  {:?} nets {:?} class {} at {:.0} of {:.0} needed; obstacle {}",
                    item.kind(),
                    item.net_nos(),
                    item.clearance_class(),
                    gap,
                    need,
                    item.is_trace_obstacle(i32::MIN)
                );
            }
        }
    }
}

fn segment_distance(a: &FloatPoint, b: &FloatPoint, p: &FloatPoint) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 { 0.0 } else { (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0) };
    (a.x + t * dx - p.x).hypot(a.y + t * dy - p.y)
}
