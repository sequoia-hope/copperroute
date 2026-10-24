use std::collections::{BTreeMap, BTreeSet};

use copper_board::items::Item;
use copper_board::{Board, ItemCtx, ItemId, Unit};
use copper_geometry::{FloatPoint, IntBox, Shape, ShapeOps, TileShape};

const MAX_CELLS_PER_LAYER: f64 = 1_000_000.0;
const CELLS_ACROSS_MIN_FEATURE: f64 = 4.0;
const HALF_CELL_DIAGONAL: f64 = std::f64::consts::FRAC_1_SQRT_2;
/// A Specctra design carries no zone clearance, only the net-class clearance the tracks were
/// routed with; KiCad fills its zones with its own zone clearance, 0.5 mm unless the board
/// says otherwise, so the pour retreats further from the tracks than the routing rules imply.
pub const ASSUMED_ZONE_CLEARANCE_MM: f64 = 0.5;

/// The copper clusters each plane net falls into once its pours are refilled around the routed
/// items. A pour keeps the rule clearance from every other-net item and drops copper narrower
/// than the net's trace width, so a pad the static plane rule calls connected can sit on an
/// island that the routed tracks have cut off from the rest of the net.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlaneConnectivity {
    clusters: BTreeMap<i32, usize>,
}

impl PlaneConnectivity {
    #[must_use]
    pub fn of(board: &Board) -> PlaneConnectivity {
        let ctx = board.ctx();
        let pours = pours_by_layer(board, &ctx);
        if pours.is_empty() {
            return PlaneConnectivity::default();
        }
        let plane_nets: BTreeSet<i32> = pours.values().flatten().map(|pour| pour.net).collect();
        let bounds = board.get_bounding_box_of_items(board.items_in_board_order());
        let zone_clearance_floor = ASSUMED_ZONE_CLEARANCE_MM * board_units_per_mm(board);
        let cell = cell_size(board, &pours, zone_clearance_floor, &bounds);
        let signal_layers: Vec<usize> = (0..board.get_layer_count())
            .filter(|layer| board.layer_structure().layers[*layer].is_signal)
            .collect();

        let mut copper = CopperLabels::default();
        for layer in &signal_layers {
            let mut raster = Raster::new(&bounds, cell);
            let layer_pours = pours.get(layer).map_or(&[][..], Vec::as_slice);
            paint_pours(&mut raster, layer_pours);
            clear_around_other_nets(
                &mut raster,
                board,
                &ctx,
                *layer,
                layer_pours,
                zone_clearance_floor,
            );
            paint_plane_net_items(&mut raster, board, &ctx, *layer, &plane_nets);
            copper.label_layer(*layer, &raster);
        }

        let mut clusters = BTreeMap::new();
        for net in plane_nets {
            let mut net_labels: Vec<u32> = Vec::new();
            let mut unlabelled_items = 0;
            for id in board.get_connectable_items(net) {
                let Some(item) = board.get_item(id) else {
                    continue;
                };
                if matches!(item, Item::ConductionArea(_)) {
                    continue;
                }
                let item_labels: Vec<u32> = seed_points(board, &ctx, id, item, &signal_layers)
                    .into_iter()
                    .filter_map(|(layer, point)| copper.label_at(layer, &point))
                    .collect();
                match item_labels.split_first() {
                    None => unlabelled_items += 1,
                    Some((first, rest)) => {
                        for label in rest {
                            copper.union(*first, *label);
                        }
                        net_labels.push(*first);
                    }
                }
            }
            let roots: BTreeSet<u32> = net_labels
                .into_iter()
                .map(|label| copper.find(label))
                .collect();
            clusters.insert(net, roots.len() + unlabelled_items);
        }
        PlaneConnectivity { clusters }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.clusters.is_empty()
    }

    #[must_use]
    pub fn cluster_count(&self, net_number: i32) -> Option<usize> {
        self.clusters.get(&net_number).copied()
    }

    #[must_use]
    pub fn missing_connection_count(&self) -> usize {
        self.clusters
            .values()
            .map(|count| count.saturating_sub(1))
            .sum()
    }

    #[must_use]
    pub fn splits_more_than(&self, other: &PlaneConnectivity) -> bool {
        self.clusters.iter().any(|(net, count)| {
            other
                .clusters
                .get(net)
                .is_none_or(|previous| count > previous)
        })
    }
}

/// One conduction area's pour. Where pours of different nets overlap on a layer, KiCad fills
/// the higher-priority zone and keeps the other clear of it; a Specctra design carries no
/// priorities, so the smaller pour is taken to be the one nested inside the larger.
struct Pour {
    id: ItemId,
    net: i32,
    clearance_class: usize,
    half_width: f64,
    footprint: f64,
    tiles: Vec<Outline>,
    edges: Vec<Shape>,
}

fn pours_by_layer(board: &Board, ctx: &ItemCtx<'_>) -> BTreeMap<usize, Vec<Pour>> {
    let mut pours: BTreeMap<usize, Vec<Pour>> = BTreeMap::new();
    for id in board.get_conduction_areas() {
        let Some(item @ Item::ConductionArea(area)) = board.get_item(id) else {
            continue;
        };
        if item.net_count() == 0 {
            continue;
        }
        let net = item.get_net_number(0);
        if board.rules.nets.get(net).is_none() {
            continue;
        }
        let layer = area.get_layer();
        let absolute = area.get_area(ctx);
        let edges = std::iter::once(absolute.get_border())
            .chain(absolute.get_holes())
            .collect();
        pours.entry(layer).or_default().push(Pour {
            id,
            net,
            clearance_class: item.clearance_class(),
            half_width: f64::from(board.rules.get_trace_half_width(net, layer)),
            footprint: box_area(&area.bounding_box(ctx)),
            tiles: area
                .split_to_convex(ctx)
                .unwrap_or_default()
                .iter()
                .filter_map(Outline::of_tile)
                .collect(),
            edges,
        });
    }
    for layer_pours in pours.values_mut() {
        layer_pours.sort_by(|a, b| b.footprint.total_cmp(&a.footprint).then(a.id.cmp(&b.id)));
    }
    pours
}

fn box_area(bounds: &IntBox) -> f64 {
    (f64::from(bounds.ur.x) - f64::from(bounds.ll.x)).max(0.0)
        * (f64::from(bounds.ur.y) - f64::from(bounds.ll.y)).max(0.0)
}

fn board_units_per_mm(board: &Board) -> f64 {
    let resolution = board.communication.resolution.max(1);
    f64::from(resolution) / Unit::scale(1.0, board.communication.unit, Unit::Mm)
}

fn cell_size(
    board: &Board,
    pours: &BTreeMap<usize, Vec<Pour>>,
    zone_clearance_floor: f64,
    bounds: &IntBox,
) -> f64 {
    let matrix = &board.rules.clearance_matrix;
    let mut min_feature = f64::INFINITY;
    for (layer, layer_pours) in pours {
        for pour in layer_pours {
            min_feature = min_feature.min(pour.half_width * 2.0);
            for class in 0..matrix.get_class_count() {
                let clearance = matrix.get_value(class, pour.clearance_class, *layer, false);
                if clearance > 0 {
                    min_feature = min_feature.min(f64::from(clearance).max(zone_clearance_floor));
                }
            }
        }
    }
    let coarsest_allowed = (box_area(bounds).max(1.0) / MAX_CELLS_PER_LAYER).sqrt();
    let finest_wanted = if min_feature.is_finite() {
        (min_feature / CELLS_ACROSS_MIN_FEATURE).max(1.0)
    } else {
        1.0
    };
    finest_wanted.max(coarsest_allowed)
}

/// A convex piece of copper, or a round one, in a form whose point distance costs a handful
/// of float operations per edge.
enum Outline {
    Disc { centre: FloatPoint, radius: f64 },
    Convex(Vec<FloatPoint>),
}

impl Outline {
    fn of_tile(tile: &TileShape) -> Option<Outline> {
        let corners = tile.corner_approx_arr();
        (corners.len() >= 3).then_some(Outline::Convex(corners))
    }

    fn of_shape(shape: &Shape) -> Vec<Outline> {
        match shape {
            Shape::Circle(circle) => vec![Outline::Disc {
                centre: circle.center.to_float(),
                radius: f64::from(circle.radius),
            }],
            Shape::Tile(tile) => Outline::of_tile(tile).into_iter().collect(),
            Shape::Polygon(_) => shape
                .split_to_convex()
                .unwrap_or_default()
                .iter()
                .filter_map(Outline::of_tile)
                .collect(),
        }
    }

    fn bounds(&self) -> (FloatPoint, FloatPoint) {
        match self {
            Outline::Disc { centre, radius } => (
                FloatPoint {
                    x: centre.x - radius,
                    y: centre.y - radius,
                },
                FloatPoint {
                    x: centre.x + radius,
                    y: centre.y + radius,
                },
            ),
            Outline::Convex(corners) => corners.iter().fold(
                (
                    FloatPoint {
                        x: f64::INFINITY,
                        y: f64::INFINITY,
                    },
                    FloatPoint {
                        x: f64::NEG_INFINITY,
                        y: f64::NEG_INFINITY,
                    },
                ),
                |(low, high), corner| {
                    (
                        FloatPoint {
                            x: low.x.min(corner.x),
                            y: low.y.min(corner.y),
                        },
                        FloatPoint {
                            x: high.x.max(corner.x),
                            y: high.y.max(corner.y),
                        },
                    )
                },
            ),
        }
    }

    fn distance(&self, point: &FloatPoint) -> f64 {
        match self {
            Outline::Disc { centre, radius } => (point.distance(centre) - radius).max(0.0),
            Outline::Convex(corners) => {
                let mut all_left = true;
                let mut all_right = true;
                let mut nearest = f64::INFINITY;
                for index in 0..corners.len() {
                    let from = corners[index];
                    let to = corners[(index + 1) % corners.len()];
                    let cross =
                        (to.x - from.x) * (point.y - from.y) - (to.y - from.y) * (point.x - from.x);
                    all_left &= cross >= 0.0;
                    all_right &= cross <= 0.0;
                    nearest = nearest.min(segment_distance(&from, &to, point));
                }
                if all_left || all_right { 0.0 } else { nearest }
            }
        }
    }
}

struct Raster {
    min_x: f64,
    min_y: f64,
    cell: f64,
    nx: usize,
    ny: usize,
    owner: Vec<i32>,
}

impl Raster {
    fn new(bounds: &IntBox, cell: f64) -> Raster {
        let min_x = f64::from(bounds.ll.x) - cell;
        let min_y = f64::from(bounds.ll.y) - cell;
        let nx = ((f64::from(bounds.ur.x) - min_x) / cell).ceil() as usize + 2;
        let ny = ((f64::from(bounds.ur.y) - min_y) / cell).ceil() as usize + 2;
        Raster {
            min_x,
            min_y,
            cell,
            nx,
            ny,
            owner: vec![0; nx * ny],
        }
    }

    fn axis_range(&self, low: f64, high: f64, min: f64, count: usize) -> std::ops::Range<usize> {
        let first = (((low - min) / self.cell).floor().max(0.0) as usize).min(count);
        let last = (((high - min) / self.cell).ceil().max(0.0) as usize + 1).min(count);
        first..last.max(first)
    }

    fn for_each_cell_in(
        &mut self,
        low: FloatPoint,
        high: FloatPoint,
        mut visit: impl FnMut(&mut i32, FloatPoint),
    ) {
        let xs = self.axis_range(low.x, high.x, self.min_x, self.nx);
        let ys = self.axis_range(low.y, high.y, self.min_y, self.ny);
        for iy in ys {
            let y = self.min_y + (iy as f64 + 0.5) * self.cell;
            for ix in xs.clone() {
                let x = self.min_x + (ix as f64 + 0.5) * self.cell;
                visit(&mut self.owner[iy * self.nx + ix], FloatPoint { x, y });
            }
        }
    }

    /// Visits every cell within `radius` of the segment with its distance to the segment's
    /// centre line, sweeping the segment in short chunks so a long diagonal does not scan the
    /// whole rectangle it spans.
    fn for_each_cell_near_segment(
        &mut self,
        from: FloatPoint,
        to: FloatPoint,
        radius: f64,
        mut visit: impl FnMut(&mut i32, f64),
    ) {
        let chunk = (radius * 2.0).max(self.cell);
        let chunks = (from.distance(&to) / chunk).ceil().max(1.0) as usize;
        for index in 0..chunks {
            let start = lerp(&from, &to, index as f64 / chunks as f64);
            let end = lerp(&from, &to, (index + 1) as f64 / chunks as f64);
            let (low, high) = inflated(&segment_bounds(&start, &end), radius);
            self.for_each_cell_in(low, high, |owner, centre| {
                let distance = segment_distance(&start, &end, &centre);
                if distance <= radius {
                    visit(owner, distance);
                }
            });
        }
    }

    fn for_each_cell_near_outline(
        &mut self,
        outline: &Outline,
        radius: f64,
        mut visit: impl FnMut(&mut i32, f64),
    ) {
        let (low, high) = inflated(&outline.bounds(), radius);
        self.for_each_cell_in(low, high, |owner, centre| {
            let distance = outline.distance(&centre);
            if distance <= radius {
                visit(owner, distance);
            }
        });
    }
}

fn inflated(bounds: &(FloatPoint, FloatPoint), radius: f64) -> (FloatPoint, FloatPoint) {
    (
        FloatPoint {
            x: bounds.0.x - radius,
            y: bounds.0.y - radius,
        },
        FloatPoint {
            x: bounds.1.x + radius,
            y: bounds.1.y + radius,
        },
    )
}

fn box_bounds(bounds: &IntBox) -> (FloatPoint, FloatPoint) {
    (
        FloatPoint {
            x: f64::from(bounds.ll.x),
            y: f64::from(bounds.ll.y),
        },
        FloatPoint {
            x: f64::from(bounds.ur.x),
            y: f64::from(bounds.ur.y),
        },
    )
}

fn segment_bounds(from: &FloatPoint, to: &FloatPoint) -> (FloatPoint, FloatPoint) {
    (
        FloatPoint {
            x: from.x.min(to.x),
            y: from.y.min(to.y),
        },
        FloatPoint {
            x: from.x.max(to.x),
            y: from.y.max(to.y),
        },
    )
}

fn lerp(from: &FloatPoint, to: &FloatPoint, t: f64) -> FloatPoint {
    FloatPoint {
        x: from.x + (to.x - from.x) * t,
        y: from.y + (to.y - from.y) * t,
    }
}

fn segment_distance(from: &FloatPoint, to: &FloatPoint, point: &FloatPoint) -> f64 {
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let length_square = dx * dx + dy * dy;
    let t = if length_square == 0.0 {
        0.0
    } else {
        (((point.x - from.x) * dx + (point.y - from.y) * dy) / length_square).clamp(0.0, 1.0)
    };
    point.distance(&FloatPoint {
        x: from.x + dx * t,
        y: from.y + dy * t,
    })
}

fn edges(corners: &[FloatPoint]) -> impl Iterator<Item = (FloatPoint, FloatPoint)> + '_ {
    corners.windows(2).map(|pair| (pair[0], pair[1]))
}

fn paint_pours(raster: &mut Raster, pours: &[Pour]) {
    for pour in pours {
        for tile in &pour.tiles {
            raster.for_each_cell_near_outline(tile, 0.0, |owner, _| *owner = pour.net);
        }
    }
    for pour in pours {
        for edge in &pour.edges {
            erode_along(raster, edge, pour.half_width, pour.net);
        }
    }
}

fn erode_along(raster: &mut Raster, edge: &Shape, half_width: f64, net: i32) {
    let clear = |owner: &mut i32| {
        if *owner == net {
            *owner = 0;
        }
    };
    let corners = edge.corner_approx_arr();
    if corners.len() >= 2 {
        for index in 0..corners.len() {
            let from = corners[index];
            let to = corners[(index + 1) % corners.len()];
            raster.for_each_cell_near_segment(from, to, half_width, |owner, _| clear(owner));
        }
        return;
    }
    let (low, high) = inflated(&box_bounds(&edge.bounding_box()), half_width);
    raster.for_each_cell_in(low, high, |owner, centre| {
        if edge.border_distance(&centre) <= half_width {
            clear(owner);
        }
    });
}

fn clear_around_other_nets(
    raster: &mut Raster,
    board: &Board,
    ctx: &ItemCtx<'_>,
    layer: usize,
    pours: &[Pour],
    zone_clearance_floor: f64,
) {
    if pours.is_empty() {
        return;
    }
    let matrix = &board.rules.clearance_matrix;
    for id in board.items_in_board_order() {
        let Some(item) = board.get_item(id) else {
            continue;
        };
        let outlines: Vec<Outline> = match item {
            Item::Trace(trace) if trace.get_layer() == layer => Vec::new(),
            Item::Via(via) if item.is_on_layer(layer, ctx) => via
                .get_shape_on_layer(layer, ctx)
                .map_or_else(Vec::new, |shape| Outline::of_shape(&shape)),
            Item::Pin(pin) if item.is_on_layer(layer, ctx) => pin
                .get_shape_on_layer(layer, ctx)
                .map_or_else(Vec::new, |shape| Outline::of_shape(&shape)),
            Item::ConductionArea(area) if area.get_layer() == layer => area
                .split_to_convex(ctx)
                .unwrap_or_default()
                .iter()
                .filter_map(Outline::of_tile)
                .collect(),
            Item::ObstacleArea(area) if area.get_layer() == layer => area
                .split_to_convex(ctx)
                .unwrap_or_default()
                .iter()
                .filter_map(Outline::of_tile)
                .collect(),
            _ => continue,
        };
        let footprint = match item {
            Item::ConductionArea(area) => Some(box_area(&area.bounding_box(ctx))),
            _ => None,
        };
        let keep_out: Vec<(i32, f64)> = pours
            .iter()
            .filter(|pour| !item.contains_net(pour.net))
            .filter(|pour| footprint.is_none_or(|own| pour.footprint > own))
            .map(|pour| {
                let clearance = f64::from(matrix.get_value(
                    item.clearance_class(),
                    pour.clearance_class,
                    layer,
                    false,
                ));
                let reach = match item {
                    Item::ObstacleArea(_) => pour.half_width,
                    _ => clearance.max(zone_clearance_floor) + pour.half_width,
                };
                (pour.net, reach)
            })
            .collect();
        if keep_out.is_empty() {
            continue;
        }
        let widest = keep_out.iter().map(|(_, reach)| *reach).fold(0.0, f64::max);
        let mut clear = |owner: &mut i32, distance: f64| {
            if keep_out
                .iter()
                .any(|(net, reach)| *net == *owner && distance <= *reach)
            {
                *owner = 0;
            }
        };
        if let Item::Trace(trace) = item {
            let half_width = f64::from(trace.get_half_width());
            for (from, to) in edges(&trace.polyline().corner_approx_arr()) {
                raster.for_each_cell_near_segment(
                    from,
                    to,
                    widest + half_width,
                    |owner, distance| clear(owner, distance - half_width),
                );
            }
        }
        for outline in &outlines {
            raster.for_each_cell_near_outline(outline, widest, &mut clear);
        }
    }
}

fn paint_plane_net_items(
    raster: &mut Raster,
    board: &Board,
    ctx: &ItemCtx<'_>,
    layer: usize,
    plane_nets: &BTreeSet<i32>,
) {
    let touch = raster.cell * HALF_CELL_DIAGONAL;
    for id in board.items_in_board_order() {
        let Some(item) = board.get_item(id) else {
            continue;
        };
        let Some(net) = item
            .net_nos()
            .iter()
            .copied()
            .find(|net| plane_nets.contains(net))
        else {
            continue;
        };
        match item {
            Item::Trace(trace) if trace.get_layer() == layer => {
                let reach = f64::from(trace.get_half_width()) + touch;
                for (from, to) in edges(&trace.polyline().corner_approx_arr()) {
                    raster.for_each_cell_near_segment(from, to, reach, |owner, _| *owner = net);
                }
            }
            Item::Via(via) if item.is_on_layer(layer, ctx) => {
                if let Some(shape) = via.get_shape_on_layer(layer, ctx) {
                    for outline in Outline::of_shape(&shape) {
                        raster.for_each_cell_near_outline(&outline, touch, |owner, _| *owner = net);
                    }
                }
            }
            Item::Pin(pin) if item.is_on_layer(layer, ctx) => {
                if let Some(shape) = pin.get_shape_on_layer(layer, ctx) {
                    for outline in Outline::of_shape(&shape) {
                        raster.for_each_cell_near_outline(&outline, touch, |owner, _| *owner = net);
                    }
                }
            }
            _ => {}
        }
    }
}

fn seed_points(
    board: &Board,
    ctx: &ItemCtx<'_>,
    id: ItemId,
    item: &Item,
    signal_layers: &[usize],
) -> Vec<(usize, FloatPoint)> {
    match item {
        Item::Trace(trace) => trace
            .polyline()
            .corner_approx_arr()
            .into_iter()
            .map(|corner| (trace.get_layer(), corner))
            .collect(),
        Item::Via(_) | Item::Pin(_) => {
            let Some(centre) = board.drill_center(id) else {
                return Vec::new();
            };
            let centre = centre.to_float();
            signal_layers
                .iter()
                .copied()
                .filter(|layer| item.is_on_layer(*layer, ctx))
                .map(|layer| (layer, centre))
                .collect()
        }
        _ => Vec::new(),
    }
}

#[derive(Default)]
struct CopperLabels {
    layers: BTreeMap<usize, LayerLabels>,
    parent: Vec<u32>,
}

struct LayerLabels {
    min_x: f64,
    min_y: f64,
    cell: f64,
    nx: usize,
    ny: usize,
    labels: Vec<u32>,
}

impl CopperLabels {
    fn label_layer(&mut self, layer: usize, raster: &Raster) {
        let mut labels = vec![0u32; raster.owner.len()];
        let mut stack: Vec<usize> = Vec::new();
        for start in 0..raster.owner.len() {
            if raster.owner[start] == 0 || labels[start] != 0 {
                continue;
            }
            let label = self.new_label();
            labels[start] = label;
            stack.push(start);
            while let Some(cell) = stack.pop() {
                let net = raster.owner[cell];
                let ix = cell % raster.nx;
                let iy = cell / raster.nx;
                let neighbours = [
                    (ix > 0).then(|| cell - 1),
                    (ix + 1 < raster.nx).then(|| cell + 1),
                    (iy > 0).then(|| cell - raster.nx),
                    (iy + 1 < raster.ny).then(|| cell + raster.nx),
                ];
                for next in neighbours.into_iter().flatten() {
                    if raster.owner[next] == net && labels[next] == 0 {
                        labels[next] = label;
                        stack.push(next);
                    }
                }
            }
        }
        self.layers.insert(
            layer,
            LayerLabels {
                min_x: raster.min_x,
                min_y: raster.min_y,
                cell: raster.cell,
                nx: raster.nx,
                ny: raster.ny,
                labels,
            },
        );
    }

    fn label_at(&self, layer: usize, point: &FloatPoint) -> Option<u32> {
        let grid = self.layers.get(&layer)?;
        let ix = ((point.x - grid.min_x) / grid.cell).floor();
        let iy = ((point.y - grid.min_y) / grid.cell).floor();
        if ix < 0.0 || iy < 0.0 || ix >= grid.nx as f64 || iy >= grid.ny as f64 {
            return None;
        }
        let label = grid.labels[iy as usize * grid.nx + ix as usize];
        (label != 0).then_some(label)
    }

    fn new_label(&mut self) -> u32 {
        if self.parent.is_empty() {
            self.parent.push(0);
        }
        let label = self.parent.len() as u32;
        self.parent.push(label);
        label
    }

    fn find(&mut self, label: u32) -> u32 {
        let mut root = label;
        while self.parent[root as usize] != root {
            root = self.parent[root as usize];
        }
        let mut current = label;
        while self.parent[current as usize] != root {
            let next = self.parent[current as usize];
            self.parent[current as usize] = root;
            current = next;
        }
        root
    }

    fn union(&mut self, a: u32, b: u32) {
        let root_a = self.find(a);
        let root_b = self.find(b);
        if root_a != root_b {
            self.parent[root_b as usize] = root_a;
        }
    }
}
