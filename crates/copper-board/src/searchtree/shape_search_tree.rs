use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use copper_geometry::bounding_directions::ShapeBoundingDirections;
use copper_geometry::regular_tile_shape::RegularTileShape;
use copper_geometry::{Circle, Line, Point, Polyline, Shape, ShapeOps, TileShape};

use crate::datastructures::{LeafId, ShapeTree, TreeEntry};
use crate::ids::{ItemId, RoomId, TreeId, TreeObject};
use crate::items::{Item, ItemCtx, PolylineTrace};
use crate::library::Padstack;
use crate::rules::{BoardRules, ClearanceMatrix};
use crate::structure::AngleRestriction;

const DRILL_HOLE_CLEARANCE_MARGIN: i32 = 10;

pub trait ItemLookup {
    fn item(&self, id: ItemId) -> Option<&Item>;
}

impl ItemLookup for crate::board::item_store::ItemStore<Item> {
    fn item(&self, id: ItemId) -> Option<&Item> {
        self.get(&id)
    }
}

impl ItemLookup for BTreeMap<ItemId, Item> {
    #[inline]
    fn item(&self, id: ItemId) -> Option<&Item> {
        self.get(&id)
    }
}

pub trait RoomLookup {
    fn room_tree_shape(&self, id: RoomId) -> Option<&TileShape>;

    fn room_shape_layer(&self, id: RoomId) -> Option<usize>;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NoRooms;

impl RoomLookup for NoRooms {
    fn room_tree_shape(&self, _id: RoomId) -> Option<&TileShape> {
        None
    }

    fn room_shape_layer(&self, _id: RoomId) -> Option<usize> {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct EntrySortedByClearance {
    clearance: i32,
    entry_id: u64,
    entry: TreeEntry<TreeObject>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShapeSearchTree {
    id: TreeId,
    angle: AngleRestriction,
    tree: ShapeTree<TreeObject>,
    compensated_clearance_class: usize,
}

impl std::fmt::Display for ShapeSearchTree {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.get_key())
    }
}

impl ShapeSearchTree {
    pub fn new(
        id: TreeId,
        angle: AngleRestriction,
        compensated_clearance_class: usize,
    ) -> ShapeSearchTree {
        ShapeSearchTree {
            id,
            angle,
            tree: ShapeTree::new(Self::bounding_directions_for(angle)),
            compensated_clearance_class,
        }
    }

    pub fn bounding_directions_for(angle: AngleRestriction) -> ShapeBoundingDirections {
        match angle {
            AngleRestriction::NinetyDegree => ShapeBoundingDirections::Orthogonal,
            AngleRestriction::None | AngleRestriction::FortyFiveDegree => {
                ShapeBoundingDirections::FortyfiveDegree
            }
        }
    }

    pub fn id(&self) -> TreeId {
        self.id
    }

    pub fn angle(&self) -> AngleRestriction {
        self.angle
    }

    pub fn compensated_clearance_class(&self) -> usize {
        self.compensated_clearance_class
    }

    pub fn tree(&self) -> &ShapeTree<TreeObject> {
        &self.tree
    }

    pub fn size(&self) -> usize {
        self.tree.leaf_count()
    }

    pub fn get_key(&self) -> String {
        let class = match self.angle {
            AngleRestriction::None => "ShapeSearchTree",
            AngleRestriction::FortyFiveDegree => "ShapeSearchTree45Degree",
            AngleRestriction::NinetyDegree => "ShapeSearchTree90Degree",
        };
        let directions = match Self::bounding_directions_for(self.angle) {
            ShapeBoundingDirections::Orthogonal => "Orthogonal",
            ShapeBoundingDirections::FortyfiveDegree => "FortyfiveDegree",
        };
        format!(
            "{class}_{directions}_cc{}",
            self.compensated_clearance_class
        )
    }

    pub fn is_clearance_compensation_used(&self) -> bool {
        self.compensated_clearance_class > 0
    }

    pub fn clearance_compensation_value(
        &self,
        clearance_class_index: usize,
        layer: usize,
        rules: &BoardRules,
    ) -> i32 {
        if clearance_class_index == 0 {
            return 0;
        }
        let matrix = &rules.clearance_matrix;
        let result = matrix.get_value(
            clearance_class_index,
            self.compensated_clearance_class,
            layer,
            false,
        ) - matrix
            .clearance_compensation_value(self.compensated_clearance_class, layer);
        result.max(0)
    }

    pub fn compensated_half_width(&self, trace: &PolylineTrace, rules: &BoardRules) -> i32 {
        trace.get_half_width()
            + self.clearance_compensation_value(
                trace.hdr.clearance_class(),
                trace.get_layer(),
                rules,
            )
    }
}

impl ShapeSearchTree {
    pub fn calculate_tree_shapes(&self, item: &Item, ctx: &ItemCtx<'_>) -> Vec<Option<TileShape>> {
        match item {
            Item::Via(_) | Item::Pin(_) => self.calculate_drill_item_tree_shapes(item, ctx),
            Item::ObstacleArea(_)
            | Item::ConductionArea(_)
            | Item::ViaObstacleArea(_)
            | Item::ComponentObstacleArea(_) => self.calculate_obstacle_area_tree_shapes(item, ctx),
            Item::BoardOutline(_) => self.calculate_board_outline_tree_shapes(item, ctx),
            Item::Trace(trace) => self.calculate_trace_tree_shapes(trace, ctx),
            Item::ComponentOutline(_) => Vec::new(),
        }
    }

    fn calculate_drill_item_tree_shapes(
        &self,
        item: &Item,
        ctx: &ItemCtx<'_>,
    ) -> Vec<Option<TileShape>> {
        let count = item.tile_shape_count(ctx);
        (0..count)
            .map(|i| {
                let current_shape = drill_item_shape(item, i, ctx)
                    .or_else(|| self.drill_hole_obstacle(item, ctx))?;
                let layer = item.shape_layer(i, ctx);
                let offset_width =
                    self.clearance_compensation_value(
                        item.header().clearance_class(),
                        layer,
                        ctx.rules,
                    ) + self.drill_hole_clearance_delta(item, &current_shape, layer, ctx);
                let offset_width = f64::from(offset_width);
                Some(match self.angle {
                    AngleRestriction::NinetyDegree => {
                        TileShape::Box(current_shape.bounding_box().offset(offset_width))
                    }
                    AngleRestriction::FortyFiveDegree => {
                        let octagon = current_shape.bounding_octagon()?;
                        let tile = if TileShape::Octagon(octagon).is_int_box() {
                            TileShape::Box(current_shape.bounding_box())
                        } else {
                            TileShape::Octagon(octagon)
                        };
                        let offset = tile.offset(offset_width);
                        TileShape::Octagon(offset.bounding_octagon()?)
                    }
                    AngleRestriction::None => {
                        let tile = match ctx.rules.trace_angle_restriction {
                            AngleRestriction::NinetyDegree => {
                                TileShape::Box(current_shape.bounding_box())
                            }
                            AngleRestriction::FortyFiveDegree => {
                                TileShape::Octagon(current_shape.bounding_octagon()?)
                            }
                            AngleRestriction::None => current_shape.bounding_tile(),
                        };
                        tile.enlarge(offset_width)
                    }
                })
            })
            .collect()
    }

    fn drill_hole_obstacle(&self, item: &Item, ctx: &ItemCtx<'_>) -> Option<Shape> {
        if ctx.rules.get_hole_clearance() <= 0 {
            return None;
        }
        let padstack = drill_item_padstack(item, ctx)?;
        let drill_radius = padstack.drill_radius();
        if drill_radius <= 0.0 {
            return None;
        }
        let center = match drill_item_center(item, ctx) {
            Point::Int(p) => p,
            other => other.to_float().round(),
        };
        Some(Shape::Circle(Circle::new(
            center,
            drill_radius.ceil() as i32,
        )))
    }

    fn drill_hole_clearance_delta(
        &self,
        item: &Item,
        shape: &Shape,
        layer: usize,
        ctx: &ItemCtx<'_>,
    ) -> i32 {
        let hole_clearance = ctx.rules.get_hole_clearance();
        if hole_clearance <= 0 {
            return 0;
        }
        let Some(padstack) = drill_item_padstack(item, ctx) else {
            return 0;
        };
        let drill_radius = padstack.drill_radius();
        if drill_radius <= 0.0 {
            return 0;
        }
        let copper_radius = if padstack.hole_only {
            drill_radius
        } else {
            let from_shape = shape.border_distance(&drill_item_center(item, ctx).to_float());
            if from_shape > 0.0 {
                from_shape
            } else {
                match padstack.get_shape(layer as i32) {
                    None => drill_radius,
                    Some(pad_shape) => {
                        pad_shape.border_distance(&copper_geometry::FloatPoint::ZERO)
                    }
                }
            }
        };
        let clearance_class = if self.compensated_clearance_class > 0 {
            self.compensated_clearance_class
        } else {
            BoardRules::default_clearance_class()
        };
        let copper_clearance = ctx.rules.clearance_matrix.get_value(
            item.header().clearance_class(),
            clearance_class,
            layer,
            false,
        );
        let raw =
            (drill_radius + f64::from(hole_clearance) + f64::from(DRILL_HOLE_CLEARANCE_MARGIN)
                - copper_radius
                - f64::from(copper_clearance))
            .ceil();
        (raw as i32).max(0)
    }

    fn calculate_obstacle_area_tree_shapes(
        &self,
        item: &Item,
        ctx: &ItemCtx<'_>,
    ) -> Vec<Option<TileShape>> {
        let Some(convex_shapes) = obstacle_area_split_to_convex(item, ctx) else {
            return Vec::new();
        };
        let offset_width = f64::from(self.clearance_compensation_value(
            item.header().clearance_class(),
            item.first_layer(ctx),
            ctx.rules,
        ));
        convex_shapes
            .iter()
            .flat_map(|piece| {
                piece
                    .enlarge(offset_width)
                    .divide_into_sections(ctx.max_tree_shape_width)
            })
            .map(|piece| Some(self.regularise(piece)))
            .collect()
    }

    fn calculate_board_outline_tree_shapes(
        &self,
        item: &Item,
        ctx: &ItemCtx<'_>,
    ) -> Vec<Option<TileShape>> {
        let Item::BoardOutline(outline) = item else {
            unreachable!("calculate_board_outline_tree_shapes is only reached for BoardOutline")
        };
        let layer_count = ctx.rules.layer_structure().count();
        let clearance_class = item.header().clearance_class();
        let mut result: Vec<Option<TileShape>> = Vec::new();
        if outline.keepout_outside_outline_generated() {
            let Some(convex_shapes) = outline.keepout_convex_pieces(ctx) else {
                return Vec::new();
            };
            for layer_index in 0..layer_count {
                let offset_width = f64::from(self.clearance_compensation_value(
                    clearance_class,
                    layer_index,
                    ctx.rules,
                ));
                for piece in convex_shapes {
                    result.push(Some(self.regularise(piece.enlarge(offset_width))));
                }
            }
        } else {
            let half_width = outline.get_half_width();
            for layer_index in 0..layer_count {
                let cmp_value =
                    self.clearance_compensation_value(clearance_class, layer_index, ctx.rules);
                for shape_index in 0..outline.shape_count() {
                    let Some(outline_shape) = outline.get_shape(shape_index) else {
                        continue;
                    };
                    let ops = outline_shape.as_ops();
                    let border_line_count = ops.border_line_count();
                    if border_line_count == 0 {
                        continue;
                    }
                    let mut previous = ops.border_line(border_line_count - 1);
                    for i in 0..border_line_count {
                        let current = ops.border_line(i);
                        let next = ops.border_line((i + 1) % border_line_count);
                        let cuts = outline.piece_cuts(shape_index, i);
                        let caps: Vec<Option<Line>> = std::iter::once(previous)
                            .chain(cuts.into_iter().map(Some))
                            .chain(std::iter::once(next))
                            .collect();
                        for ends in caps.windows(2) {
                            let shape = match (ends[0], current, ends[1]) {
                                (Some(a), Some(b), Some(c)) => Polyline::from_lines(vec![a, b, c])
                                    .ok()
                                    .and_then(|polyline| {
                                        polyline.offset_shape(half_width + cmp_value, 0)
                                    }),
                                _ => None,
                            };
                            result.push(shape.map(|shape| self.regularise(shape)));
                        }
                        previous = current;
                    }
                }
            }
        }
        result
    }

    fn calculate_trace_tree_shapes(
        &self,
        trace: &PolylineTrace,
        ctx: &ItemCtx<'_>,
    ) -> Vec<Option<TileShape>> {
        let offset_width = self.compensated_half_width(trace, ctx.rules);
        (0..trace.tile_shape_count())
            .map(|i| self.offset_shape(trace.polyline(), offset_width, i))
            .collect()
    }

    fn regularise(&self, shape: TileShape) -> TileShape {
        match self.angle {
            AngleRestriction::None => shape,
            AngleRestriction::FortyFiveDegree => {
                shape.bounding_octagon().map_or(shape, TileShape::Octagon)
            }
            AngleRestriction::NinetyDegree => TileShape::Box(shape.bounding_box()),
        }
    }

    pub fn offset_shape(
        &self,
        polyline: &Polyline,
        half_width: i32,
        no: usize,
    ) -> Option<TileShape> {
        match self.angle {
            AngleRestriction::NinetyDegree => {
                polyline.offset_box(half_width, no).map(TileShape::Box)
            }
            _ => polyline.offset_shape(half_width, no),
        }
    }

    pub fn offset_shapes(
        &self,
        polyline: &Polyline,
        half_width: i32,
        from_no: usize,
        to_no: usize,
    ) -> Vec<TileShape> {
        match self.angle {
            AngleRestriction::NinetyDegree => {
                let to_no = to_no.min(polyline.lines().len().saturating_sub(1));
                (from_no..to_no.saturating_sub(1))
                    .map(|j| {
                        TileShape::Box(polyline.offset_box(half_width, j).expect(
                            "ShapeSearchTree90Degree.offsetShapes: `j + 1 <= lines.len() - 2` \
                             holds after the clamp, so the line segment always exists",
                        ))
                    })
                    .collect()
            }
            _ => polyline.offset_shapes_between(half_width, from_no, to_no),
        }
    }
}

fn drill_item_shape(item: &Item, index: usize, ctx: &ItemCtx<'_>) -> Option<Shape> {
    match item {
        Item::Via(via) => via.get_shape(index, ctx),
        Item::Pin(pin) => pin.get_shape(index, ctx),
        _ => unreachable!("drill_item_shape is only reached for Via and Pin"),
    }
}

fn drill_item_padstack<'a>(item: &Item, ctx: &ItemCtx<'a>) -> Option<&'a Padstack> {
    match item {
        Item::Via(via) => via.get_padstack(ctx),
        Item::Pin(pin) => pin.get_padstack(ctx),
        _ => unreachable!("drill_item_padstack is only reached for Via and Pin"),
    }
}

fn drill_item_center(item: &Item, ctx: &ItemCtx<'_>) -> Point {
    match item {
        Item::Via(via) => via.get_center(),
        Item::Pin(pin) => pin.get_center(ctx),
        _ => unreachable!("drill_item_center is only reached for Via and Pin"),
    }
}

fn obstacle_area_split_to_convex<'a>(item: &'a Item, ctx: &ItemCtx<'_>) -> Option<&'a [TileShape]> {
    match item {
        Item::ObstacleArea(a) => a.split_to_convex(ctx),
        Item::ConductionArea(a) => a.split_to_convex(ctx),
        Item::ViaObstacleArea(a) => a.split_to_convex(ctx),
        Item::ComponentObstacleArea(a) => a.split_to_convex(ctx),
        _ => unreachable!("obstacle_area_split_to_convex is only reached for the area variants"),
    }
}

impl ShapeSearchTree {
    pub fn insert_item(&mut self, item: &mut Item, ctx: &ItemCtx<'_>) {
        self.fill_tree_shapes(item, ctx);
        let shapes = item
            .header()
            .get_precalculated_tree_shapes(self.id)
            .expect("fill_tree_shapes always leaves a shape array for this tree")
            .to_vec();
        if shapes.is_empty() {
            item.set_tree_entries(self.id, Vec::new());
            return;
        }
        let entries = self
            .tree
            .insert_tiles_opt(TreeObject::Item(item.id()), &shapes);
        item.set_tree_entries(self.id, entries);
    }

    pub fn remove_item(&mut self, item: &mut Item) {
        if let Some(entries) = item.get_search_tree_entries(self.id) {
            let entries = entries.to_vec();
            self.tree.remove_opt(&entries);
        }
    }

    pub fn insert_room(&mut self, room: RoomId, shape: &TileShape) -> Option<LeafId> {
        let bounds = self.tree.bounding_shape(shape)?;
        Some(self.tree.insert_leaf(TreeObject::Room(room), 0, bounds))
    }

    pub fn remove_room(&mut self, leaf: Option<LeafId>) {
        self.tree.remove_leaf_opt(leaf);
    }

    fn fill_tree_shapes(&self, item: &mut Item, ctx: &ItemCtx<'_>) {
        if item
            .header()
            .get_precalculated_tree_shapes(self.id)
            .is_none()
        {
            let shapes = self.calculate_tree_shapes(item, ctx);
            item.set_precalculated_tree_shapes(self.id, shapes);
        }
    }

    pub fn get_tree_shape<'a>(
        &self,
        item: &'a Item,
        index: usize,
        ctx: &ItemCtx<'_>,
    ) -> Option<Cow<'a, TileShape>> {
        if let Some(shapes) = item.header().get_precalculated_tree_shapes(self.id)
            && index < shapes.len()
        {
            return shapes[index].as_ref().map(Cow::Borrowed);
        }
        let mut shapes = self.calculate_tree_shapes(item, ctx);
        if index >= shapes.len() {
            return None;
        }
        shapes.swap_remove(index).map(Cow::Owned)
    }

    fn tree_shape_of<'a>(
        &self,
        entry: TreeEntry<TreeObject>,
        items: &'a impl ItemLookup,
        rooms: &'a impl RoomLookup,
        ctx: &ItemCtx<'_>,
    ) -> Cow<'a, TileShape> {
        match entry.object {
            TreeObject::Item(id) => {
                let item = items.item(id).unwrap_or_else(|| {
                    panic!("ShapeSearchTree: item {id} has a leaf but is not in the item list")
                });
                self.get_tree_shape(item, entry.shape_index, ctx)
                    .unwrap_or_else(|| {
                        panic!(
                            "ShapeSearchTree: item {id} has a leaf for shape {} in tree {:?} but \
                             no shape for it — Java NPEs here too",
                            entry.shape_index, self.id
                        )
                    })
            }
            TreeObject::Room(id) => Cow::Borrowed(rooms.room_tree_shape(id).unwrap_or_else(|| {
                panic!(
                    "ShapeSearchTree: expansion room {id:?} has a leaf in tree {:?} but the \
                     supplied RoomLookup does not resolve it — Java NPEs here too",
                    self.id
                )
            })),
        }
    }
}

impl ShapeSearchTree {
    pub fn overlapping_objects(
        &self,
        shape: &TileShape,
        layer: Option<usize>,
        ignore_net_nos: &[i32],
        items: &impl ItemLookup,
        ctx: &ItemCtx<'_>,
    ) -> BTreeSet<TreeObject> {
        self.overlapping_objects_with_rooms(shape, layer, ignore_net_nos, items, &NoRooms, ctx)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn overlapping_objects_with_rooms(
        &self,
        shape: &TileShape,
        layer: Option<usize>,
        ignore_net_nos: &[i32],
        items: &impl ItemLookup,
        rooms: &impl RoomLookup,
        ctx: &ItemCtx<'_>,
    ) -> BTreeSet<TreeObject> {
        self.overlapping_tree_entries_with_rooms(shape, layer, ignore_net_nos, items, rooms, ctx)
            .into_iter()
            .map(|entry| entry.object)
            .collect()
    }

    pub fn overlapping_tree_entries(
        &self,
        shape: &TileShape,
        layer: Option<usize>,
        ignore_net_nos: &[i32],
        items: &impl ItemLookup,
        ctx: &ItemCtx<'_>,
    ) -> Vec<TreeEntry<TreeObject>> {
        self.overlapping_tree_entries_with_rooms(shape, layer, ignore_net_nos, items, &NoRooms, ctx)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn overlapping_tree_entries_with_rooms(
        &self,
        shape: &TileShape,
        layer: Option<usize>,
        ignore_net_nos: &[i32],
        items: &impl ItemLookup,
        rooms: &impl RoomLookup,
        ctx: &ItemCtx<'_>,
    ) -> Vec<TreeEntry<TreeObject>> {
        let Some(bounds) = self.tree.bounding_shape(shape) else {
            return Vec::new();
        };
        let is_45_degree = matches!(shape, TileShape::Octagon(_));
        self.tree
            .overlaps(&bounds)
            .into_iter()
            .filter(|entry| {
                if self.ignore_object(*entry, layer, ignore_net_nos, items, rooms, ctx) {
                    return false;
                }
                let current_shape = self.tree_shape_of(*entry, items, rooms, ctx);
                if is_45_degree && matches!(*current_shape, TileShape::Octagon(_)) {
                    return true;
                }
                current_shape.intersects(shape)
            })
            .collect()
    }

    fn ignore_object(
        &self,
        entry: TreeEntry<TreeObject>,
        layer: Option<usize>,
        ignore_net_nos: &[i32],
        items: &impl ItemLookup,
        rooms: &impl RoomLookup,
        ctx: &ItemCtx<'_>,
    ) -> bool {
        match entry.object {
            TreeObject::Item(id) => {
                let Some(item) = items.item(id) else {
                    return true;
                };
                if let Some(layer) = layer
                    && item.shape_layer(entry.shape_index, ctx) != layer
                {
                    return true;
                }
                ignore_net_nos
                    .iter()
                    .any(|net_no| !item.is_obstacle_for_net(*net_no))
            }
            TreeObject::Room(id) => {
                let room_layer = rooms.room_shape_layer(id).unwrap_or_else(|| {
                    panic!(
                        "ShapeSearchTree: expansion room {id:?} has a leaf in tree {:?} but the \
                         supplied RoomLookup does not resolve it — Java NPEs here too",
                        self.id
                    )
                });
                layer.is_some_and(|layer| room_layer != layer)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn overlapping_tree_entries_with_clearance(
        &self,
        shape: &TileShape,
        layer: Option<usize>,
        ignore_net_nos: &[i32],
        clearance_class_index: usize,
        items: &impl ItemLookup,
        ctx: &ItemCtx<'_>,
        entry_counter: &mut u64,
    ) -> Vec<TreeEntry<TreeObject>> {
        let matrix: &ClearanceMatrix = &ctx.rules.clearance_matrix;
        let bounds = self
            .tree
            .bounding_shape(shape)
            .unwrap_or(RegularTileShape::Box(*ctx.bounding_box));
        let max_clearance =
            (1.2 * f64::from(matrix.max_value(clearance_class_index, layer.unwrap_or(0)))) as i32;
        let offset_bounds = offset_regular(bounds, f64::from(max_clearance));

        let mut sorted_items: BTreeSet<EntrySortedByClearance> = BTreeSet::new();
        for entry in self.tree.overlaps(&offset_bounds) {
            if self.ignore_object(entry, layer, ignore_net_nos, items, &NoRooms, ctx) {
                continue;
            }
            let TreeObject::Item(id) = entry.object else {
                continue;
            };
            let item = items.item(id).expect("ignore_object rejects unknown items");
            let current_clearance = match layer {
                Some(layer) => matrix.get_value(
                    clearance_class_index,
                    item.header().clearance_class(),
                    layer,
                    // Match the nominal clearance used by the compensated maze tree.
                    // Adding a margin here rejects paths the locator legally placed.
                    false,
                ),
                None => 0,
            };
            *entry_counter += 1;
            sorted_items.insert(EntrySortedByClearance {
                clearance: current_clearance,
                entry_id: *entry_counter,
                entry,
            });
        }

        let mut current_half_clearance = 0;
        let mut current_offset_shape = shape.clone();
        let mut result = Vec::new();
        for sorted in sorted_items {
            let tmp_half_clearance = sorted.clearance / 2;
            if tmp_half_clearance != current_half_clearance {
                current_half_clearance = tmp_half_clearance;
                current_offset_shape = shape.enlarge(f64::from(current_half_clearance));
            }
            let tmp_shape = self.tree_shape_of(sorted.entry, items, &NoRooms, ctx);
            let tmp_offset_shape = tmp_shape.enlarge(f64::from(current_half_clearance));
            if current_offset_shape.intersects(&tmp_offset_shape) {
                result.push(sorted.entry);
            }
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    pub fn overlapping_tree_entries_with_clearance_auto(
        &self,
        shape: &TileShape,
        layer: Option<usize>,
        ignore_net_nos: &[i32],
        clearance_class_index: usize,
        items: &impl ItemLookup,
        ctx: &ItemCtx<'_>,
        entry_counter: &mut u64,
    ) -> Vec<TreeEntry<TreeObject>> {
        if self.is_clearance_compensation_used() {
            self.overlapping_tree_entries(shape, layer, ignore_net_nos, items, ctx)
        } else {
            self.overlapping_tree_entries_with_clearance(
                shape,
                layer,
                ignore_net_nos,
                clearance_class_index,
                items,
                ctx,
                entry_counter,
            )
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn overlapping_objects_with_clearance(
        &self,
        shape: &TileShape,
        layer: Option<usize>,
        ignore_net_nos: &[i32],
        clearance_class_index: usize,
        items: &impl ItemLookup,
        ctx: &ItemCtx<'_>,
        entry_counter: &mut u64,
    ) -> BTreeSet<TreeObject> {
        self.overlapping_tree_entries_with_clearance_auto(
            shape,
            layer,
            ignore_net_nos,
            clearance_class_index,
            items,
            ctx,
            entry_counter,
        )
        .into_iter()
        .map(|entry| entry.object)
        .collect()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn overlapping_items_with_clearance(
        &self,
        shape: &TileShape,
        layer: Option<usize>,
        ignore_net_nos: &[i32],
        clearance_class_index: usize,
        items: &impl ItemLookup,
        ctx: &ItemCtx<'_>,
        entry_counter: &mut u64,
    ) -> Vec<ItemId> {
        self.overlapping_objects_with_clearance(
            shape,
            layer,
            ignore_net_nos,
            clearance_class_index,
            items,
            ctx,
            entry_counter,
        )
        .into_iter()
        .filter_map(|object| match object {
            TreeObject::Item(id) => Some(id),
            TreeObject::Room(_) => None,
        })
        .collect()
    }

    pub fn validate_entries(&self, item: &Item) -> bool {
        let Some(entries) = item.get_search_tree_entries(self.id) else {
            return true;
        };
        entries
            .iter()
            .enumerate()
            .all(|(i, entry)| entry.is_none_or(|leaf| self.tree.leaf_entry(leaf).shape_index == i))
    }
}

fn offset_regular(bounds: RegularTileShape, dist: f64) -> RegularTileShape {
    match bounds {
        RegularTileShape::Box(b) => RegularTileShape::Box(b.offset(dist)),
        RegularTileShape::Octagon(o) => RegularTileShape::Octagon(o.offset(dist)),
    }
}

impl ShapeSearchTree {
    fn insert_index(
        &mut self,
        id: ItemId,
        shapes: &[Option<TileShape>],
        index: usize,
    ) -> Option<LeafId> {
        let shape = shapes.get(index)?.as_ref()?;
        let bounds = self.tree.bounding_shape(shape)?;
        Some(self.tree.insert_leaf(TreeObject::Item(id), index, bounds))
    }

    fn rekey(&mut self, leaf: Option<LeafId>, id: ItemId, shape_index: usize) {
        if let Some(leaf) = leaf {
            self.tree
                .set_leaf_entry(leaf, TreeObject::Item(id), shape_index);
        }
    }

    pub fn change_entries(
        &mut self,
        trace: &mut PolylineTrace,
        new_polyline: &Polyline,
        keep_at_start_count: usize,
        keep_at_end_count: usize,
        rules: &BoardRules,
    ) {
        let id = trace.hdr.id();
        let compensated_half_width = self.compensated_half_width(trace, rules);
        let changed_shapes = self.offset_shapes(
            new_polyline,
            compensated_half_width,
            keep_at_start_count,
            new_polyline.lines().len() - 1 - keep_at_end_count,
        );
        let old_shape_count = trace
            .hdr
            .get_precalculated_tree_shapes(self.id)
            .map_or(0, <[Option<TileShape>]>::len);
        let new_shape_count = changed_shapes.len() + keep_at_start_count + keep_at_end_count;
        let old_entries: Vec<Option<LeafId>> = trace
            .hdr
            .get_tree_entries(self.id)
            .map(<[Option<LeafId>]>::to_vec)
            .unwrap_or_default();
        let old_shapes: Vec<Option<TileShape>> = trace
            .hdr
            .get_precalculated_tree_shapes(self.id)
            .map(<[Option<TileShape>]>::to_vec)
            .unwrap_or_default();

        let mut new_leaves: Vec<Option<LeafId>> = vec![None; new_shape_count];
        let mut new_shapes: Vec<Option<TileShape>> = vec![None; new_shape_count];

        for i in 0..keep_at_start_count {
            new_leaves[i] = old_entries[i];
            new_shapes[i].clone_from(&old_shapes[i]);
        }
        for old_entry in old_entries
            .iter()
            .take(old_shape_count.saturating_sub(keep_at_end_count))
            .skip(keep_at_start_count)
        {
            self.tree.remove_leaf_opt(*old_entry);
        }
        let old_tail_base = old_shape_count as i64 - keep_at_end_count as i64;
        for i in 0..keep_at_end_count {
            let new_index = new_shape_count - keep_at_end_count + i;
            let old_index_signed = old_tail_base + i as i64;
            assert!(
                old_index_signed >= 0,
                "ShapeSearchTree.changeEntries: keepAtEndCount ({keep_at_end_count}) exceeds \
                 oldShapeCount ({old_shape_count}); Java throws ArrayIndexOutOfBoundsException \
                 on oldEntries[{old_index_signed}] (ShapeSearchTree.java:148)"
            );
            let old_index = old_index_signed as usize;
            new_leaves[new_index] = old_entries[old_index];
            self.rekey(new_leaves[new_index], id, new_index);
            new_shapes[new_index].clone_from(&old_shapes[old_index]);
        }
        for (offset, shape) in changed_shapes.into_iter().enumerate() {
            new_shapes[keep_at_start_count + offset] = Some(shape);
        }
        trace
            .hdr
            .set_precalculated_tree_shapes(self.id, new_shapes.clone());

        for (i, leaf) in new_leaves
            .iter_mut()
            .enumerate()
            .take(new_shape_count.saturating_sub(keep_at_end_count))
            .skip(keep_at_start_count)
        {
            *leaf = self.insert_index(id, &new_shapes, i);
        }
        trace.hdr.set_tree_entries(self.id, new_leaves);
    }

    pub fn merge_entries_in_front(
        &mut self,
        from_trace: &mut PolylineTrace,
        to_trace: &mut PolylineTrace,
        joined_polyline: &Polyline,
        from_entry_no: usize,
        to_entry_no: usize,
        rules: &BoardRules,
    ) {
        let to_id = to_trace.hdr.id();
        let change_order = from_trace.first_corner() == to_trace.first_corner();
        let from_shape_count_minus_1 = from_trace.tile_shape_count() - 1;
        let remove_no = if change_order {
            0
        } else {
            from_shape_count_minus_1
        };
        let from_entries: Vec<Option<LeafId>> = from_trace
            .hdr
            .get_tree_entries(self.id)
            .map(<[Option<LeafId>]>::to_vec)
            .unwrap_or_default();
        let to_entries: Vec<Option<LeafId>> = to_trace
            .hdr
            .get_tree_entries(self.id)
            .map(<[Option<LeafId>]>::to_vec)
            .unwrap_or_default();
        let from_shapes: Vec<Option<TileShape>> = from_trace
            .hdr
            .get_precalculated_tree_shapes(self.id)
            .map(<[Option<TileShape>]>::to_vec)
            .unwrap_or_default();
        let to_shapes: Vec<Option<TileShape>> = to_trace
            .hdr
            .get_precalculated_tree_shapes(self.id)
            .map(<[Option<TileShape>]>::to_vec)
            .unwrap_or_default();
        self.tree.remove_leaf_opt(from_entries[remove_no]);
        self.tree.remove_leaf_opt(to_entries[0]);

        let link_shapes = self.offset_shapes(
            joined_polyline,
            self.compensated_half_width(to_trace, rules),
            from_entry_no,
            to_entry_no,
        );
        let new_shape_count = from_entries.len() + link_shapes.len() + to_entries.len() - 2;
        let old_to_shape_count = to_entries.len();
        let mut new_leaves: Vec<Option<LeafId>> = vec![None; new_shape_count];
        let mut new_shapes: Vec<Option<TileShape>> = vec![None; new_shape_count];

        for i in 0..from_shape_count_minus_1 {
            let from_no = if change_order {
                from_shape_count_minus_1 - i
            } else {
                i
            };
            new_shapes[i].clone_from(&from_shapes[from_no]);
            new_leaves[i] = from_entries[from_no];
            self.rekey(new_leaves[i], to_id, i);
        }
        for i in 1..old_to_shape_count {
            let current_ind = from_shape_count_minus_1 + link_shapes.len() + i - 1;
            new_shapes[current_ind].clone_from(&to_shapes[i]);
            new_leaves[current_ind] = to_entries[i];
            self.rekey(new_leaves[current_ind], to_id, current_ind);
        }
        for (i, shape) in link_shapes.iter().enumerate() {
            new_shapes[from_shape_count_minus_1 + i] = Some(shape.clone());
        }
        to_trace
            .hdr
            .set_precalculated_tree_shapes(self.id, new_shapes.clone());
        for i in 0..link_shapes.len() {
            let current_ind = from_shape_count_minus_1 + i;
            new_leaves[current_ind] = self.insert_index(to_id, &new_shapes, current_ind);
        }
        to_trace.hdr.set_tree_entries(self.id, new_leaves);
    }

    pub fn merge_entries_at_end(
        &mut self,
        from_trace: &mut PolylineTrace,
        to_trace: &mut PolylineTrace,
        joined_polyline: &Polyline,
        from_entry_no: usize,
        to_entry_no: usize,
        rules: &BoardRules,
    ) {
        let to_id = to_trace.hdr.id();
        let change_order = from_trace.last_corner() == to_trace.last_corner();
        let from_entries: Vec<Option<LeafId>> = from_trace
            .hdr
            .get_tree_entries(self.id)
            .map(<[Option<LeafId>]>::to_vec)
            .unwrap_or_default();
        let to_entries: Vec<Option<LeafId>> = to_trace
            .hdr
            .get_tree_entries(self.id)
            .map(<[Option<LeafId>]>::to_vec)
            .unwrap_or_default();
        let from_shapes: Vec<Option<TileShape>> = from_trace
            .hdr
            .get_precalculated_tree_shapes(self.id)
            .map(<[Option<TileShape>]>::to_vec)
            .unwrap_or_default();
        let to_shapes: Vec<Option<TileShape>> = to_trace
            .hdr
            .get_precalculated_tree_shapes(self.id)
            .map(<[Option<TileShape>]>::to_vec)
            .unwrap_or_default();

        let to_shape_count_minus_1 = to_trace.tile_shape_count() - 1;
        self.tree
            .remove_leaf_opt(to_entries[to_shape_count_minus_1]);
        let remove_no = if change_order {
            from_trace.tile_shape_count() - 1
        } else {
            0
        };
        self.tree.remove_leaf_opt(from_entries[remove_no]);

        let link_shapes = self.offset_shapes(
            joined_polyline,
            self.compensated_half_width(to_trace, rules),
            from_entry_no,
            to_entry_no,
        );
        let new_shape_count = from_entries.len() + link_shapes.len() + to_entries.len() - 2;
        let mut new_leaves: Vec<Option<LeafId>> = vec![None; new_shape_count];
        let mut new_shapes: Vec<Option<TileShape>> = vec![None; new_shape_count];

        for i in 0..to_shape_count_minus_1 {
            new_shapes[i].clone_from(&to_shapes[i]);
            new_leaves[i] = to_entries[i];
        }
        for i in 1..from_entries.len() {
            let current_ind = to_shape_count_minus_1 + link_shapes.len() + i - 1;
            let from_no = if change_order {
                from_entries.len() - i - 1
            } else {
                i
            };
            new_shapes[current_ind].clone_from(&from_shapes[from_no]);
            new_leaves[current_ind] = from_entries[from_no];
            self.rekey(new_leaves[current_ind], to_id, current_ind);
        }
        for (i, shape) in link_shapes.iter().enumerate() {
            new_shapes[to_shape_count_minus_1 + i] = Some(shape.clone());
        }
        to_trace
            .hdr
            .set_precalculated_tree_shapes(self.id, new_shapes.clone());
        for i in 0..link_shapes.len() {
            let current_ind = to_shape_count_minus_1 + i;
            new_leaves[current_ind] = self.insert_index(to_id, &new_shapes, current_ind);
        }
        to_trace.hdr.set_tree_entries(self.id, new_leaves);
    }

    pub fn reuse_entries_after_cutout(
        &mut self,
        from_trace: &mut PolylineTrace,
        start_piece: &mut PolylineTrace,
        end_piece: &mut PolylineTrace,
        ctx: &ItemCtx<'_>,
    ) {
        let start_id = start_piece.hdr.id();
        let end_id = end_piece.hdr.id();
        let start_len = start_piece.polyline().lines().len() - 2;
        let end_len = end_piece.polyline().lines().len() - 2;
        let mut from_entries: Vec<Option<LeafId>> = from_trace
            .hdr
            .get_tree_entries(self.id)
            .map(<[Option<LeafId>]>::to_vec)
            .unwrap_or_default();
        let mut start_leaves: Vec<Option<LeafId>> = vec![None; start_len];
        let mut end_leaves: Vec<Option<LeafId>> = vec![None; end_len];

        for i in 0..start_len - 1 {
            start_leaves[i] = from_entries[i];
            self.rekey(start_leaves[i], start_id, i);
            from_entries[i] = None;
        }
        let start_shapes = self.tree_shapes_for(start_piece, ctx);
        start_leaves[start_len - 1] = self.insert_index(start_id, &start_shapes, start_len - 1);

        let end_shapes = self.tree_shapes_for(end_piece, ctx);
        end_leaves[0] = self.insert_index(end_id, &end_shapes, 0);
        for (i, leaf) in end_leaves.iter_mut().enumerate().skip(1) {
            let from_index = from_entries.len() - end_len + i;
            *leaf = from_entries[from_index];
            if let Some(leaf) = *leaf {
                self.tree.set_leaf_entry(leaf, TreeObject::Item(end_id), i);
            }
            from_entries[from_index] = None;
        }

        from_trace.hdr.set_tree_entries(self.id, from_entries);
        start_piece.hdr.set_tree_entries(self.id, start_leaves);
        end_piece.hdr.set_tree_entries(self.id, end_leaves);
    }

    fn tree_shapes_for(
        &self,
        trace: &mut PolylineTrace,
        ctx: &ItemCtx<'_>,
    ) -> Vec<Option<TileShape>> {
        if trace.hdr.get_precalculated_tree_shapes(self.id).is_none() {
            let shapes = self.calculate_trace_tree_shapes(trace, ctx);
            trace.hdr.set_precalculated_tree_shapes(self.id, shapes);
        }
        trace
            .hdr
            .get_precalculated_tree_shapes(self.id)
            .expect("just filled")
            .to_vec()
    }

    pub fn change_item_shape(&mut self, item: &mut Item, shape_index: usize, new_shape: TileShape) {
        let id = item.id();
        let old_entries: Vec<Option<LeafId>> = item
            .get_search_tree_entries(self.id)
            .map(<[Option<LeafId>]>::to_vec)
            .unwrap_or_default();
        let old_shapes: Vec<Option<TileShape>> = item
            .header()
            .get_precalculated_tree_shapes(self.id)
            .map(<[Option<TileShape>]>::to_vec)
            .unwrap_or_default();
        let mut new_leaves: Vec<Option<LeafId>> = vec![None; old_entries.len()];
        let mut new_shapes: Vec<Option<TileShape>> = vec![None; old_entries.len()];
        self.tree.remove_leaf_opt(old_entries[shape_index]);
        for i in 0..new_shapes.len() {
            if i == shape_index {
                new_shapes[i] = Some(new_shape.clone());
            } else {
                new_shapes[i].clone_from(&old_shapes[i]);
                new_leaves[i] = old_entries[i];
            }
        }
        item.set_precalculated_tree_shapes(self.id, new_shapes.clone());
        new_leaves[shape_index] = self.insert_index(id, &new_shapes, shape_index);
        item.set_tree_entries(self.id, new_leaves);
    }

    pub fn reduce_trace_shape_at_tie_pin(
        &mut self,
        tie_pin: &crate::items::Pin,
        trace: &mut PolylineTrace,
        ctx: &ItemCtx<'_>,
    ) {
        let Some(pin_shape) = tie_pin
            .get_tree_shape_on_layer(self.id, trace.get_layer(), ctx)
            .cloned()
        else {
            return;
        };
        let pin_center = tie_pin.get_center(ctx);
        let (trace_shape_no, compare_corner) = if trace.first_corner() == Some(pin_center.clone()) {
            (0, trace.polyline().corner_approx(1))
        } else if trace.last_corner() == Some(pin_center) {
            let no = trace.corner_count() - 2;
            (no, trace.polyline().corner_approx(no))
        } else {
            return;
        };
        let Some(trace_shape) = trace
            .hdr
            .get_precalculated_tree_shapes(self.id)
            .and_then(|shapes| shapes.get(trace_shape_no).and_then(Option::as_ref).cloned())
        else {
            return;
        };
        if trace_shape.intersection(&pin_shape).dimension() < 2 {
            return;
        }
        let Some(shape_pieces) = trace_shape.cutout(&pin_shape) else {
            return;
        };
        let mut new_trace_shape = TileShape::Simplex(copper_geometry::Simplex::EMPTY);
        let mut found = false;
        for piece in shape_pieces {
            if piece.dimension() == 2 {
                let contains_corner =
                    compare_corner.is_some_and(|corner| piece.contains_float(&corner));
                if !found || contains_corner {
                    new_trace_shape = piece;
                    found = true;
                }
            }
        }
        let mut item = Item::Trace(trace.clone());
        self.change_item_shape(&mut item, trace_shape_no, new_trace_shape);
        if let Item::Trace(updated) = item {
            *trace = updated;
        }
    }
}
