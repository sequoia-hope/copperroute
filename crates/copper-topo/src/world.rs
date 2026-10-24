//! The obstacles, per layer: their outlines as triangulation samples, and an index of their
//! polygons for the exact questions the samples can't answer. Copper this router laid remembers
//! the connection it belongs to, so a search may cross it at a price and have it ripped.

use std::collections::{BTreeMap, HashMap};

use copper_board::items::Item;
use copper_board::structure::Unit;
use copper_board::{Board, ItemId};
use copper_board::DrcConstraints;
use copper_drc::constraints::netclass_name;
use copper_geometry::FloatPoint;
use spade::handles::{FixedFaceHandle, InnerTag};

pub type Face = FixedFaceHandle<InnerTag>;
use spade::{DelaunayTriangulation, Point2, PositionInTriangulation, Triangulation};

use crate::WireRules;
use crate::funnel::Gate;

#[derive(Clone)]
struct Obstacle {
    nets: Vec<i32>,
    /// The connection this copper was laid for, when a rip-up may remove it.
    conn: Option<u32>,
    class: usize,
    /// The DRC clearance rule of its net's class.
    rule: i32,
    traces: bool,
    vias: bool,
    layer: usize,
    polygon: Vec<FloatPoint>,
}

#[derive(Clone)]
struct Layer {
    cdt: DelaunayTriangulation<Point2<f64>>,
    /// The obstacles each vertex samples, by vertex index.
    owners: Vec<Vec<u32>>,
}

#[derive(Clone)]
pub struct World {
    obstacles: Vec<Obstacle>,
    layers: BTreeMap<usize, Layer>,
    cells: HashMap<(usize, i64, i64), Vec<u32>>,
    cell: f64,
    sample: f64,
    holes: Vec<(FloatPoint, f64, Option<u32>)>,
    hole_to_hole: f64,
    copper_layers: usize,
}

const CELL_MM: f64 = 1.0;
/// How many bends `clear_path` may add to one run.
const MAX_BENDS: usize = 200;
const BEND_SLACK: f64 = 1.02;

/// How near a wire's centre may come to a vertex: with everything kept, and with the copper of
/// `soft`, the connections that keep it nearer, ripped.
struct Reach {
    hard: f64,
    all: f64,
    soft: Vec<u32>,
}

pub type Conns = HashMap<ItemId, u32>;

impl World {
    pub fn build(board: &Board, sample: f64, constraints: &DrcConstraints, conns: &Conns) -> World {
        let bounds = board.get_bounding_box();
        let pad = 10.0 * sample;
        let frame = [
            (f64::from(bounds.ll.x) - pad, f64::from(bounds.ll.y) - pad),
            (f64::from(bounds.ur.x) + pad, f64::from(bounds.ll.y) - pad),
            (f64::from(bounds.ur.x) + pad, f64::from(bounds.ur.y) + pad),
            (f64::from(bounds.ll.x) - pad, f64::from(bounds.ur.y) + pad),
        ];
        let mut layers = BTreeMap::new();
        for layer in (0..board.get_layer_count()).filter(|&l| board.layer_structure().layers[l].is_signal) {
            let mut l = Layer { cdt: DelaunayTriangulation::new(), owners: Vec::new() };
            for (x, y) in frame {
                l.insert(FloatPoint::new(x, y), None);
            }
            layers.insert(layer, l);
        }
        let mut world = World {
            obstacles: Vec::new(),
            layers,
            cells: HashMap::new(),
            cell: CELL_MM * board.communication.get_resolution(Unit::Mm),
            sample,
            holes: Vec::new(),
            hole_to_hole: f64::from(constraints.hole_to_hole.unwrap_or(0).max(0)),
            copper_layers: board.get_layer_count(),
        };
        let ids: Vec<ItemId> = board.get_items().map(Item::id).collect();
        world.add_items(board, &ids, constraints, conns);
        world
    }

    pub fn add_items(&mut self, board: &Board, ids: &[ItemId], constraints: &DrcConstraints, conns: &Conns) {
        let ctx = board.ctx();
        for &id in ids {
            let Some(item) = board.get_item(id) else {
                continue;
            };
            let conn = conns.get(&id).copied();
            if let Some(hole) = copper_drc::checks::geometry::hole_of(board, id) {
                self.holes.push((hole.center, hole.radius, conn));
            }
            let traces = item.is_trace_obstacle(i32::MIN);
            let vias = traces || matches!(item, Item::ViaObstacleArea(_));
            if !traces && !vias {
                continue;
            }
            let nets = item.net_nos().to_vec();
            let rule = nets
                .first()
                .and_then(|&n| netclass_name(board, n))
                .and_then(|name| constraints.netclass_clearance.get(&name).copied())
                .unwrap_or(0);
            for index in 0..item.tile_shape_count(&ctx) {
                let layer = item.shape_layer(index, &ctx);
                let Some(shape) = board.item_tile_shape_ref(id, index) else {
                    continue;
                };
                let polygon = shape.corner_approx_arr();
                if polygon.is_empty() {
                    continue;
                }
                let at = self.obstacles.len() as u32;
                let (lo, hi) = extent(&polygon);
                for cx in (lo.x / self.cell).floor() as i64..=(hi.x / self.cell).floor() as i64 {
                    for cy in (lo.y / self.cell).floor() as i64..=(hi.y / self.cell).floor() as i64 {
                        self.cells.entry((layer, cx, cy)).or_default().push(at);
                    }
                }
                if traces && let Some(l) = self.layers.get_mut(&layer) {
                    for p in samples(&polygon, self.sample) {
                        l.insert(p, Some(at));
                    }
                }
                self.obstacles.push(Obstacle { nets: nets.clone(), conn, class: item.clearance_class(), rule, traces, vias, layer, polygon });
            }
        }
    }

    pub fn face_at(&self, layer: usize, p: &FloatPoint) -> Option<Face> {
        let cdt = &self.layers.get(&layer)?.cdt;
        match cdt.locate(Point2::new(p.x, p.y)) {
            PositionInTriangulation::OnFace(face) => Some(face),
            PositionInTriangulation::OnEdge(edge) => {
                let edge = cdt.directed_edge(edge);
                edge.face().as_inner().or_else(|| edge.rev().face().as_inner()).map(|f| f.fix())
            }
            PositionInTriangulation::OnVertex(vertex) => {
                cdt.vertex(vertex).out_edges().find_map(|e| e.face().as_inner()).map(|f| f.fix())
            }
            _ => None,
        }
    }

    /// The reach of the obstacles a vertex samples that block the wire's net, 0 when none does.
    fn reach(&self, board: &Board, wire: &WireRules<'_>, layer: usize, owners: &[u32]) -> Reach {
        let mut out = Reach { hard: 0.0, all: 0.0, soft: Vec::new() };
        let mut soft = Vec::new();
        for o in owners.iter().map(|&o| &self.obstacles[o as usize]) {
            if !o.traces || o.nets.contains(&wire.net) {
                continue;
            }
            let reach = wire.reach(board, o.class, o.rule, layer);
            out.all = out.all.max(reach);
            match o.conn {
                Some(c) => soft.push((c, reach)),
                None => out.hard = out.hard.max(reach),
            }
        }
        out.soft = soft.into_iter().filter(|&(_, r)| r > out.hard).map(|(c, _)| c).collect();
        out
    }

    fn near(&self, layer: usize, p: &FloatPoint, radius: f64) -> impl Iterator<Item = &Obstacle> {
        let (x0, x1) = (((p.x - radius) / self.cell).floor() as i64, ((p.x + radius) / self.cell).floor() as i64);
        let (y0, y1) = (((p.y - radius) / self.cell).floor() as i64, ((p.y + radius) / self.cell).floor() as i64);
        let mut ids: Vec<u32> = Vec::new();
        for cx in x0..=x1 {
            for cy in y0..=y1 {
                if let Some(found) = self.cells.get(&(layer, cx, cy)) {
                    ids.extend(found);
                }
            }
        }
        ids.sort_unstable();
        ids.dedup();
        ids.into_iter().map(|o| &self.obstacles[o as usize])
    }

    /// The faces a wire may step to from `face`, each with the stretch of the shared edge it may
    /// cross: the edge less each end's reach, if what is left is free copper. With `rip`, an edge
    /// also gives the wider stretch it would have with other connections' copper ripped, and the
    /// connections that would be.
    pub fn gates(&self, board: &Board, wire: &WireRules<'_>, layer: usize, face: Face, rip: bool) -> Vec<(Face, Gate, Vec<u32>)> {
        let Some(l) = self.layers.get(&layer) else {
            return Vec::new();
        };
        let face = l.cdt.face(face);
        let mut out = Vec::new();
        for edge in face.adjacent_edges() {
            let Some(next) = edge.rev().face().as_inner() else {
                continue;
            };
            let (u, v) = (edge.from(), edge.to());
            let (pu, pv) = (u.position(), v.position());
            let reach_u = self.reach(board, wire, layer, l.owners_of(u.fix().index()));
            let reach_v = self.reach(board, wire, layer, l.owners_of(v.fix().index()));
            let len = (pv.x - pu.x).hypot(pv.y - pu.y);
            let mut kinds = vec![(reach_u.all, reach_v.all, false)];
            if rip {
                kinds.push((reach_u.hard, reach_v.hard, true));
            }
            for (ru, rv, ripping) in kinds {
                if len <= ru + rv {
                    continue;
                }
                let (dx, dy) = ((pv.x - pu.x) / len, (pv.y - pu.y) / len);
                let right = FloatPoint::new(pu.x + dx * ru, pu.y + dy * ru);
                let left = FloatPoint::new(pv.x - dx * rv, pv.y - dy * rv);
                let mid = FloatPoint::new((left.x + right.x) / 2.0, (left.y + right.y) / 2.0);
                let Some(mut victims) = self.blockers(wire, layer, &mid, ripping) else {
                    continue;
                };
                if ripping {
                    victims.extend(&reach_u.soft);
                    victims.extend(&reach_v.soft);
                    victims.sort_unstable();
                    victims.dedup();
                    if victims.is_empty() {
                        continue;
                    }
                }
                out.push((
                    next.fix(),
                    Gate {
                        left,
                        right,
                        left_wrap: (rv > 0.0).then(|| (FloatPoint::new(pv.x, pv.y), rv)),
                        right_wrap: (ru > 0.0).then(|| (FloatPoint::new(pu.x, pu.y), ru)),
                    },
                    victims,
                ));
            }
        }
        out
    }

    /// Bends `points` out round the deepest cut into any obstacle's reach, until nothing is cut
    /// or `MAX_BENDS` bends are added; the ends stay put.
    pub fn clear_path(&self, board: &Board, wire: &WireRules<'_>, layer: usize, mut points: Vec<FloatPoint>) -> Vec<FloatPoint> {
        for _ in 0..MAX_BENDS {
            let mut worst: Option<(usize, FloatPoint, f64)> = None;
            for k in 0..points.len().saturating_sub(1) {
                let (a, b) = (points[k], points[k + 1]);
                let lo = FloatPoint::new(a.x.min(b.x), a.y.min(b.y));
                let hi = FloatPoint::new(a.x.max(b.x), a.y.max(b.y));
                let mid = FloatPoint::new((lo.x + hi.x) / 2.0, (lo.y + hi.y) / 2.0);
                let span = (hi.x - lo.x).max(hi.y - lo.y) / 2.0 + self.cell;
                for o in self.near(layer, &mid, span) {
                    if !o.traces || o.nets.contains(&wire.net) || o.layer != layer {
                        continue;
                    }
                    let reach = wire.reach(board, o.class, o.rule, layer);
                    let (d, on_segment, on_obstacle) = segment_to_polygon(&a, &b, &o.polygon);
                    let cut = reach - d;
                    if cut <= reach * 1e-3 || worst.as_ref().is_some_and(|w| cut <= w.2) {
                        continue;
                    }
                    let away = if d > 0.0 {
                        ((on_segment.x - on_obstacle.x) / d, (on_segment.y - on_obstacle.y) / d)
                    } else {
                        let (cx, cy) = centroid(&o.polygon);
                        let len = (b.x - a.x).hypot(b.y - a.y).max(1e-9);
                        let n = (-(b.y - a.y) / len, (b.x - a.x) / len);
                        if n.0 * (on_segment.x - cx) + n.1 * (on_segment.y - cy) >= 0.0 { n } else { (-n.0, -n.1) }
                    };
                    let bend = FloatPoint::new(on_obstacle.x + away.0 * reach * BEND_SLACK, on_obstacle.y + away.1 * reach * BEND_SLACK);
                    worst = Some((k, bend, cut));
                }
            }
            let Some((k, bend, _)) = worst else {
                break;
            };
            points.insert(k + 1, bend);
        }
        points
    }

    /// `None` when `p` is inside copper that blocks the wire, else the connections whose copper
    /// it is inside, which only `rip` lets it be.
    fn blockers(&self, wire: &WireRules<'_>, layer: usize, p: &FloatPoint, rip: bool) -> Option<Vec<u32>> {
        let mut victims = Vec::new();
        for o in self.near(layer, p, 0.0) {
            if !o.traces || o.nets.contains(&wire.net) || o.layer != layer || !inside(&o.polygon, p) {
                continue;
            }
            match o.conn {
                Some(c) if rip => victims.push(c),
                _ => return None,
            }
        }
        Some(victims)
    }

    /// The centre of `face` when a via of the wire's net fits there on every copper layer, with
    /// the connections whose copper must be ripped for it, which only `rip` allows.
    pub fn via_site(&self, board: &Board, wire: &WireRules<'_>, layer: usize, face: Face, rip: bool) -> Option<(FloatPoint, Vec<u32>)> {
        let via = wire.profile.via.as_ref()?;
        let l = self.layers.get(&layer)?;
        let centre = l.cdt.face(face).center();
        let c = FloatPoint::new(centre.x, centre.y);
        let mut victims = Vec::new();
        for copper in 0..self.copper_layers {
            for o in self.near(copper, &c, self.cell) {
                if !o.vias || o.nets.contains(&wire.net) || o.layer != copper {
                    continue;
                }
                let need = wire.via_reach(board, o.class, o.rule, copper)?;
                if distance(&o.polygon, &c) < need {
                    match o.conn {
                        Some(conn) if rip => victims.push(conn),
                        _ => return None,
                    }
                }
            }
        }
        for (h, r, conn) in &self.holes {
            if (h.x - c.x).hypot(h.y - c.y) - r - via.drill_radius < self.hole_to_hole {
                match conn {
                    Some(conn) if rip => victims.push(*conn),
                    _ => return None,
                }
            }
        }
        victims.sort_unstable();
        victims.dedup();
        Some((c, victims))
    }
}

impl Layer {
    fn insert(&mut self, p: FloatPoint, owner: Option<u32>) {
        let Ok(handle) = self.cdt.insert(Point2::new(p.x, p.y)) else {
            return;
        };
        let index = handle.index();
        if self.owners.len() <= index {
            self.owners.resize(index + 1, Vec::new());
        }
        if let Some(o) = owner
            && !self.owners[index].contains(&o)
        {
            self.owners[index].push(o);
        }
    }

    fn owners_of(&self, index: usize) -> &[u32] {
        self.owners.get(index).map_or(&[], Vec::as_slice)
    }
}

fn extent(polygon: &[FloatPoint]) -> (FloatPoint, FloatPoint) {
    let mut lo = polygon[0];
    let mut hi = polygon[0];
    for p in polygon {
        lo = FloatPoint::new(lo.x.min(p.x), lo.y.min(p.y));
        hi = FloatPoint::new(hi.x.max(p.x), hi.y.max(p.y));
    }
    (lo, hi)
}

/// Points along the polygon's outline, its corners among them, no further apart than `spacing`.
fn samples(polygon: &[FloatPoint], spacing: f64) -> Vec<FloatPoint> {
    let mut out = Vec::new();
    for (k, a) in polygon.iter().enumerate() {
        let b = polygon[(k + 1) % polygon.len()];
        let n = ((a.x - b.x).hypot(a.y - b.y) / spacing).ceil().max(1.0) as usize;
        for i in 0..n {
            let t = i as f64 / n as f64;
            out.push(FloatPoint::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t));
        }
    }
    out
}

fn inside(polygon: &[FloatPoint], p: &FloatPoint) -> bool {
    let mut odd = false;
    let mut j = polygon.len() - 1;
    for i in 0..polygon.len() {
        let (a, b) = (polygon[i], polygon[j]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            odd = !odd;
        }
        j = i;
    }
    odd
}

fn distance(polygon: &[FloatPoint], p: &FloatPoint) -> f64 {
    if inside(polygon, p) {
        return 0.0;
    }
    (0..polygon.len())
        .map(|k| {
            let (a, b) = (polygon[k], polygon[(k + 1) % polygon.len()]);
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            let len2 = dx * dx + dy * dy;
            let t = if len2 == 0.0 { 0.0 } else { (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0) };
            (a.x + t * dx - p.x).hypot(a.y + t * dy - p.y)
        })
        .fold(f64::INFINITY, f64::min)
}

fn centroid(polygon: &[FloatPoint]) -> (f64, f64) {
    let n = polygon.len() as f64;
    (polygon.iter().map(|p| p.x).sum::<f64>() / n, polygon.iter().map(|p| p.y).sum::<f64>() / n)
}

/// The distance from segment `a`-`b` to the polygon (0 when they meet), with the nearest point
/// on each.
fn segment_to_polygon(a: &FloatPoint, b: &FloatPoint, polygon: &[FloatPoint]) -> (f64, FloatPoint, FloatPoint) {
    if inside(polygon, a) {
        return (0.0, *a, *a);
    }
    let mut best = (f64::INFINITY, *a, *a);
    for k in 0..polygon.len() {
        let (c, d) = (polygon[k], polygon[(k + 1) % polygon.len()]);
        if let Some(x) = crossing(a, b, &c, &d) {
            return (0.0, x, x);
        }
        for (p, s0, s1, p_on_ab) in [(*a, c, d, false), (*b, c, d, false), (c, *a, *b, true), (d, *a, *b, true)] {
            let f = foot(&s0, &s1, &p);
            let dist = (f.x - p.x).hypot(f.y - p.y);
            if dist < best.0 {
                best = if p_on_ab { (dist, f, p) } else { (dist, p, f) };
            }
        }
    }
    best
}

fn foot(a: &FloatPoint, b: &FloatPoint, p: &FloatPoint) -> FloatPoint {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 { 0.0 } else { (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0) };
    FloatPoint::new(a.x + t * dx, a.y + t * dy)
}

fn crossing(a: &FloatPoint, b: &FloatPoint, c: &FloatPoint, d: &FloatPoint) -> Option<FloatPoint> {
    let r = (b.x - a.x, b.y - a.y);
    let s = (d.x - c.x, d.y - c.y);
    let denom = r.0 * s.1 - r.1 * s.0;
    if denom == 0.0 {
        return None;
    }
    let t = ((c.x - a.x) * s.1 - (c.y - a.y) * s.0) / denom;
    let u = ((c.x - a.x) * r.1 - (c.y - a.y) * r.0) / denom;
    ((0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u)).then(|| FloatPoint::new(a.x + t * r.0, a.y + t * r.1))
}
