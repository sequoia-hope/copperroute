use std::sync::OnceLock;

use copper_geometry::{
    Direction, FloatPoint, IntBox, IntPoint, Line, Point, Polyline, Shape, ShapeOps, TileShape,
    Vector,
};

use crate::ids::{ItemId, PadstackId, TreeId};
use crate::items::header::ItemHeader;
use crate::items::{Connectable, copied_header};
use crate::library::{BoardLibrary, Package, Padstack};
use crate::rules::{BoardRules, Nets};
use crate::structure::{Component, Components};

#[derive(Debug, Clone, Copy)]
pub struct ItemCtx<'a> {
    pub library: &'a BoardLibrary,
    pub components: &'a Components,
    pub rules: &'a BoardRules,
    pub bounding_box: &'a IntBox,
    pub max_tree_shape_width: f64,
}

pub const DEFAULT_MAX_TREE_SHAPE_WIDTH: f64 = 50000.0;

impl<'a> ItemCtx<'a> {
    fn component(&self, component_id: i32) -> Option<&'a Component> {
        if component_id < 1 || component_id as usize > self.components.count() {
            return None;
        }
        Some(self.components.get(component_id))
    }
}

#[derive(Debug, Clone, Default)]
pub struct DrillItemData {
    center: OnceLock<Point>,
    min_width: OnceLock<f64>,
    first_layer: OnceLock<usize>,
    last_layer: OnceLock<usize>,
}

impl PartialEq for DrillItemData {
    fn eq(&self, other: &DrillItemData) -> bool {
        self.center.get() == other.center.get()
    }
}

impl DrillItemData {
    pub fn new(center: Option<Point>) -> DrillItemData {
        let data = DrillItemData::default();
        if let Some(center) = center {
            data.center
                .set(center)
                .expect("a fresh OnceLock is always empty");
        }
        data
    }

    pub fn raw_center(&self) -> Option<&Point> {
        self.center.get()
    }

    fn init_center(&self, calculate: impl FnOnce() -> Point) -> &Point {
        self.center.get_or_init(calculate)
    }

    fn clear_center(&mut self) {
        self.center.take();
    }

    fn map_center(&mut self, transform: impl FnOnce(&Point) -> Point) {
        if let Some(center) = self.center.get_mut() {
            let moved = transform(center);
            *center = moved;
        }
    }

    fn clear_derived_data(&mut self) {
        self.min_width.take();
        self.first_layer.take();
        self.last_layer.take();
    }
}

trait DrillItemBase {
    fn drill(&self) -> &DrillItemData;

    fn padstack_of<'a>(&self, ctx: &ItemCtx<'a>) -> Option<&'a Padstack>;

    fn shape_of(&self, index: usize, ctx: &ItemCtx<'_>) -> Option<Shape>;

    fn placed_on_front(&self, ctx: &ItemCtx<'_>) -> bool;
}

fn layer_index(value: i32, what: &str) -> usize {
    usize::try_from(value).unwrap_or_else(|_| {
        panic!(
            "{what}: negative layer index {value} — the padstack has no shape on any layer \
             (Padstack.java:137-152); Java returns the negative number and then throws \
             downstream"
        )
    })
}

fn first_layer_of<D: DrillItemBase>(item: &D, ctx: &ItemCtx<'_>) -> usize {
    *item.drill().first_layer.get_or_init(|| {
        let padstack = item
            .padstack_of(ctx)
            .expect("DrillItem.firstLayer: Java NPEs on a null padstack (DrillItem.java:164-166)");
        let value = if item.placed_on_front(ctx) || padstack.placed_absolute {
            padstack.from_layer()
        } else {
            padstack.board_layer_count() as i32 - padstack.to_layer() - 1
        };
        layer_index(value, "DrillItem.firstLayer")
    })
}

fn last_layer_of<D: DrillItemBase>(item: &D, ctx: &ItemCtx<'_>) -> usize {
    *item.drill().last_layer.get_or_init(|| {
        let padstack = item
            .padstack_of(ctx)
            .expect("DrillItem.lastLayer: Java NPEs on a null padstack (DrillItem.java:177-179)");
        let value = if item.placed_on_front(ctx) || padstack.placed_absolute {
            padstack.to_layer()
        } else {
            padstack.board_layer_count() as i32 - padstack.from_layer() - 1
        };
        layer_index(value, "DrillItem.lastLayer")
    })
}

fn tile_shape_count_of<D: DrillItemBase>(item: &D, ctx: &ItemCtx<'_>) -> usize {
    let padstack = item
        .padstack_of(ctx)
        .expect("DrillItem.tileShapeCount: Java NPEs on a null padstack (DrillItem.java:204)");
    layer_index(
        padstack.to_layer() - padstack.from_layer() + 1,
        "DrillItem.tileShapeCount",
    )
}

fn bounding_box_of<D: DrillItemBase>(item: &D, ctx: &ItemCtx<'_>) -> IntBox {
    let mut result = IntBox::EMPTY;
    for i in 0..tile_shape_count_of(item, ctx) {
        if let Some(shape) = item.shape_of(i, ctx) {
            result = result.union(&shape.bounding_box());
        }
    }
    result
}

fn smallest_radius_of<D: DrillItemBase>(item: &D, center: &Point, ctx: &ItemCtx<'_>) -> f64 {
    let mut result = f64::MAX;
    let c = center.to_float();
    for i in 0..tile_shape_count_of(item, ctx) {
        if let Some(shape) = item.shape_of(i, ctx) {
            result = (result).min(shape.border_distance(&c));
        }
    }
    result
}

fn min_width_of<D: DrillItemBase>(item: &D, ctx: &ItemCtx<'_>) -> f64 {
    *item.drill().min_width.get_or_init(|| {
        let mut min_width = f64::from(i32::MAX);
        let begin_layer = first_layer_of(item, ctx);
        let end_layer = last_layer_of(item, ctx);
        for current_layer in begin_layer..=end_layer {
            if !ctx.rules.layer_structure().layers[current_layer].is_signal {
                continue;
            }
            if let Some(shape) = shape_on_layer_of(item, current_layer, ctx) {
                let bounding_box = shape.bounding_box();
                min_width = (min_width).min(f64::from(bounding_box.width()));
                min_width = (min_width).min(f64::from(bounding_box.height()));
            }
        }
        min_width
    })
}

fn shape_on_layer_of<D: DrillItemBase>(item: &D, layer: usize, ctx: &ItemCtx<'_>) -> Option<Shape> {
    let from_layer = first_layer_of(item, ctx);
    let to_layer = last_layer_of(item, ctx);
    if layer < from_layer || layer > to_layer {
        return None;
    }
    item.shape_of(layer - from_layer, ctx)
}

fn shape_layer_of<D: DrillItemBase>(item: &D, index: usize, ctx: &ItemCtx<'_>) -> usize {
    let from_layer = first_layer_of(item, ctx);
    let to_layer = last_layer_of(item, ctx);
    from_layer + index.min(to_layer - from_layer)
}

fn trace_connection_shape_of(center: &Point) -> TileShape {
    TileShape::Box(TileShape::get_instance_from_point(center))
}

#[derive(Debug, Clone)]
pub struct Via {
    pub hdr: ItemHeader,
    pub drill: DrillItemData,
    padstack: PadstackId,
    pub attach_allowed: bool,
    pub is_escape_via: bool,
    pub escape_via_smd_layer: i32,
    shapes: OnceLock<Vec<Option<Shape>>>,
}

impl PartialEq for Via {
    fn eq(&self, other: &Via) -> bool {
        self.hdr == other.hdr
            && self.drill == other.drill
            && self.padstack == other.padstack
            && self.attach_allowed == other.attach_allowed
            && self.is_escape_via == other.is_escape_via
            && self.escape_via_smd_layer == other.escape_via_smd_layer
    }
}

impl DrillItemBase for Via {
    fn drill(&self) -> &DrillItemData {
        &self.drill
    }

    fn padstack_of<'a>(&self, ctx: &ItemCtx<'a>) -> Option<&'a Padstack> {
        ctx.library.get_padstack(self.padstack)
    }

    fn shape_of(&self, index: usize, ctx: &ItemCtx<'_>) -> Option<Shape> {
        self.get_shape(index, ctx)
    }

    fn placed_on_front(&self, _ctx: &ItemCtx<'_>) -> bool {
        true
    }
}

impl Via {
    pub fn new(hdr: ItemHeader, padstack: PadstackId, center: Point, attach_allowed: bool) -> Via {
        Via {
            hdr,
            drill: DrillItemData::new(Some(center)),
            padstack,
            attach_allowed,
            is_escape_via: false,
            escape_via_smd_layer: -1,
            shapes: OnceLock::new(),
        }
    }

    pub fn copy(&self, new_id: ItemId) -> Via {
        let mut copy = Via::new(
            copied_header(&self.hdr, new_id),
            self.padstack,
            self.get_center(),
            self.attach_allowed,
        );
        copy.is_escape_via = self.is_escape_via;
        copy.escape_via_smd_layer = self.escape_via_smd_layer;
        copy
    }

    pub fn get_center(&self) -> Point {
        self.drill
            .raw_center()
            .expect("a Via is always constructed with a centre (Via.java:65)")
            .clone()
    }

    pub fn get_padstack_id(&self) -> PadstackId {
        self.padstack
    }

    pub fn get_padstack<'a>(&self, ctx: &ItemCtx<'a>) -> Option<&'a Padstack> {
        ctx.library.get_padstack(self.padstack)
    }

    pub fn set_padstack(&mut self, padstack: PadstackId) {
        self.padstack = padstack;
    }

    pub fn get_shape(&self, index: usize, ctx: &ItemCtx<'_>) -> Option<Shape> {
        let padstack = self.get_padstack(ctx)?;
        let shapes = self.shapes.get_or_init(|| {
            let count = layer_index(
                padstack.to_layer() - padstack.from_layer() + 1,
                "Via.getShape",
            );
            let translate_vector = self.get_center().difference_by(&Point::ZERO);
            let first_layer = first_layer_of(self, ctx) as i32;
            (0..count)
                .map(|i| {
                    padstack
                        .get_shape(i as i32 + first_layer)
                        .map(|shape| shape.translate_by(&translate_vector))
                })
                .collect()
        });
        shapes.get(index).cloned().flatten()
    }

    pub fn get_shape_on_layer(&self, layer: usize, ctx: &ItemCtx<'_>) -> Option<Shape> {
        shape_on_layer_of(self, layer, ctx)
    }

    pub fn get_tile_shape_on_layer(
        &self,
        tree: TreeId,
        layer: usize,
        ctx: &ItemCtx<'_>,
    ) -> Option<&TileShape> {
        self.get_tree_shape_on_layer(tree, layer, ctx)
    }

    pub fn get_tree_shape_on_layer(
        &self,
        tree: TreeId,
        layer: usize,
        ctx: &ItemCtx<'_>,
    ) -> Option<&TileShape> {
        let from_layer = self.first_layer(ctx);
        if layer < from_layer || layer > self.last_layer(ctx) {
            return None;
        }
        self.hdr
            .get_precalculated_tree_shapes(tree)
            .and_then(|shapes| shapes.get(layer - from_layer))
            .and_then(Option::as_ref)
    }

    pub fn first_layer(&self, ctx: &ItemCtx<'_>) -> usize {
        first_layer_of(self, ctx)
    }

    pub fn last_layer(&self, ctx: &ItemCtx<'_>) -> usize {
        last_layer_of(self, ctx)
    }

    pub fn is_on_layer(&self, layer: usize, ctx: &ItemCtx<'_>) -> bool {
        layer >= self.first_layer(ctx) && layer <= self.last_layer(ctx)
    }

    pub fn shape_layer(&self, index: usize, ctx: &ItemCtx<'_>) -> usize {
        shape_layer_of(self, index, ctx)
    }

    pub fn is_placed_on_front(&self, _ctx: &ItemCtx<'_>) -> bool {
        true
    }

    pub fn bounding_box(&self, ctx: &ItemCtx<'_>) -> IntBox {
        bounding_box_of(self, ctx)
    }

    pub fn tile_shape_count(&self, ctx: &ItemCtx<'_>) -> usize {
        tile_shape_count_of(self, ctx)
    }

    pub fn smallest_radius(&self, ctx: &ItemCtx<'_>) -> f64 {
        smallest_radius_of(self, &self.get_center(), ctx)
    }

    pub fn min_width(&self, ctx: &ItemCtx<'_>) -> f64 {
        min_width_of(self, ctx)
    }

    pub fn translate_by(&mut self, vector: &Vector) {
        self.drill.map_center(|c| c.translate_by(vector));
        self.clear_derived_data();
    }

    pub fn turn_90_degree(&mut self, factor: i32, pole: &IntPoint) {
        self.drill
            .map_center(|c| c.turn_90_degree(factor, &Point::Int(*pole)));
        self.clear_derived_data();
    }

    pub fn rotate_approx(&mut self, angle_in_degree: f64, pole: &FloatPoint) {
        self.drill.map_center(|c| {
            Point::Int(
                c.to_float()
                    .rotate(angle_in_degree.to_radians(), pole)
                    .round(),
            )
        });
        self.clear_derived_data();
    }

    pub fn change_placement_side(&mut self, pole: &IntPoint, ctx: &ItemCtx<'_>) {
        let Some(new_padstack) = ctx.library.get_mirrored_via_padstack(self.padstack) else {
            return;
        };
        self.padstack = new_padstack;
        self.drill
            .map_center(|c| c.mirror_vertical(&Point::Int(*pole)));
        self.clear_derived_data();
    }

    pub fn clear_derived_data(&mut self) {
        self.hdr.clear_derived_data();
        self.drill.clear_derived_data();
        self.shapes.take();
        self.clear_autoroute_drill_info();
    }

    pub fn clear_autoroute_drill_info(&self) {}

    pub fn clear_autoroute_info(&mut self) {
        self.hdr.clear_autoroute_info();
        self.clear_autoroute_drill_info();
    }
}

impl Connectable for Via {
    fn header(&self) -> &ItemHeader {
        &self.hdr
    }

    fn get_trace_connection_shape(
        &self,
        _tree: TreeId,
        _index: usize,
        _ctx: &ItemCtx<'_>,
    ) -> Option<TileShape> {
        Some(trace_connection_shape_of(&self.get_center()))
    }
}

#[derive(Debug, Clone)]
pub struct Pin {
    pub hdr: ItemHeader,
    pub drill: DrillItemData,
    pub allow_solder_mask_bridges: bool,
    pub solder_mask_expansion: std::collections::BTreeMap<usize, i32>,
    pub effective_solder_mask_expansion: std::collections::BTreeMap<usize, i32>,
    pub source_footprint: Option<String>,
    pub source_pad_number: Option<String>,
    pin_index: i32,
    changed_to: Option<ItemId>,
    shapes: OnceLock<Vec<Option<Shape>>>,
}

impl PartialEq for Pin {
    fn eq(&self, other: &Pin) -> bool {
        self.hdr == other.hdr
            && self.drill == other.drill
            && self.pin_index == other.pin_index
            && self.changed_to == other.changed_to
            && self.solder_mask_expansion == other.solder_mask_expansion
            && self.effective_solder_mask_expansion == other.effective_solder_mask_expansion
            && self.allow_solder_mask_bridges == other.allow_solder_mask_bridges
            && self.source_footprint == other.source_footprint
            && self.source_pad_number == other.source_pad_number
    }
}

impl DrillItemBase for Pin {
    fn drill(&self) -> &DrillItemData {
        &self.drill
    }

    fn padstack_of<'a>(&self, ctx: &ItemCtx<'a>) -> Option<&'a Padstack> {
        self.get_padstack(ctx)
    }

    fn shape_of(&self, index: usize, ctx: &ItemCtx<'_>) -> Option<Shape> {
        self.get_shape(index, ctx)
    }

    fn placed_on_front(&self, ctx: &ItemCtx<'_>) -> bool {
        self.is_placed_on_front(ctx)
    }
}

impl Pin {
    pub fn new(hdr: ItemHeader, pin_index: i32) -> Pin {
        Pin {
            hdr,
            drill: DrillItemData::new(None),
            pin_index,
            solder_mask_expansion: Default::default(),
            effective_solder_mask_expansion: Default::default(),
            allow_solder_mask_bridges: false,
            source_footprint: None,
            source_pad_number: None,
            changed_to: None,
            shapes: OnceLock::new(),
        }
    }

    pub fn copy(&self, new_id: ItemId) -> Pin {
        let mut result = Pin::new(copied_header(&self.hdr, new_id), self.pin_index);
        result.solder_mask_expansion = self.solder_mask_expansion.clone();
        result.effective_solder_mask_expansion = self.effective_solder_mask_expansion.clone();
        result.allow_solder_mask_bridges = self.allow_solder_mask_bridges;
        result.source_footprint = self.source_footprint.clone();
        result.source_pad_number = self.source_pad_number.clone();
        result
    }

    pub fn get_pin_index(&self) -> i32 {
        self.pin_index
    }

    fn component<'a>(&self, ctx: &ItemCtx<'a>) -> Option<&'a Component> {
        ctx.component(self.hdr.get_component_id())
    }

    fn package<'a>(&self, ctx: &ItemCtx<'a>) -> Option<&'a Package> {
        let component = self.component(ctx)?;
        Some(ctx.library.get_package(component.get_package()))
    }

    pub fn is_placed_on_front(&self, ctx: &ItemCtx<'_>) -> bool {
        self.component(ctx).is_none_or(Component::placed_on_front)
    }

    pub fn get_padstack<'a>(&self, ctx: &ItemCtx<'a>) -> Option<&'a Padstack> {
        let package = self.package(ctx)?;
        let package_pin = package
            .get_pin(self.pin_index)
            .expect("Pin.getPadstack: Java NPEs on a pin index outside the package (Pin.java:129)");
        ctx.library.padstacks.get(package_pin.padstack_no)
    }

    pub fn name<'a>(&self, ctx: &ItemCtx<'a>) -> Option<&'a str> {
        let package = self.package(ctx)?;
        let package_pin = package
            .get_pin(self.pin_index)
            .expect("Pin.name: Java NPEs on a pin index outside the package (Pin.java:156)");
        Some(&package_pin.name)
    }

    pub fn relative_location(&self, ctx: &ItemCtx<'_>) -> Vector {
        let component = self
            .component(ctx)
            .expect("Pin.relativeLocation: Java NPEs on a missing component (Pin.java:66-67)");
        let package = ctx.library.get_package(component.get_package());
        let package_pin = package.get_pin(self.pin_index).expect(
            "Pin.relativeLocation: Java NPEs on a pin index outside the package (Pin.java:68-69)",
        );
        let mut rel_location = package_pin.relative_location.clone();
        let component_rotation = component.get_rotation_in_degree();
        if !component.placed_on_front() && !ctx.components.get_flip_style_rotate_first() {
            rel_location = package_pin.relative_location.mirror_at_y_axis();
        }
        if component_rotation % 90.0 == 0.0 {
            let factor = component_rotation as i32 / 90;
            if factor != 0 {
                rel_location = rel_location.turn_90_degree(factor);
            }
        } else {
            let location_approx = rel_location
                .to_float()
                .rotate(component_rotation.to_radians(), &FloatPoint::ZERO);
            rel_location = Point::Int(location_approx.round()).difference_by(&Point::ZERO);
        }
        if !component.placed_on_front() && ctx.components.get_flip_style_rotate_first() {
            rel_location = rel_location.mirror_at_y_axis();
        }
        rel_location
    }

    pub fn get_center(&self, ctx: &ItemCtx<'_>) -> Point {
        self.drill
            .init_center(|| {
                let component = self
                    .component(ctx)
                    .expect("Pin.getCenter: Java NPEs on a missing component (Pin.java:97-98)");
                let location = component
                    .get_location()
                    .expect("Pin.getCenter: Java NPEs on an unplaced component (Pin.java:98)");
                let mut pin_center = location.translate_by(&self.relative_location(ctx));

                let padstack = self
                    .get_padstack(ctx)
                    .expect("Pin.getCenter: Java NPEs on a null padstack (Pin.java:102-104)");
                let count = layer_index(
                    padstack.to_layer() - padstack.from_layer() + 1,
                    "Pin.getCenter",
                );
                match (0..count).find_map(|i| self.get_shape(i, ctx)) {
                    None => {}
                    Some(shape) if !shape.contains_inside(&pin_center) => {
                        pin_center = Point::Int(shape.centre_of_gravity().round());
                    }
                    Some(_) => {}
                }
                pin_center
            })
            .clone()
    }

    pub fn get_padstack_layer(&self, index: i32, ctx: &ItemCtx<'_>) -> i32 {
        let padstack = self
            .get_padstack(ctx)
            .expect("Pin.getPadstackLayer: Java NPEs on a null padstack (Pin.java:249)");
        let component = self
            .component(ctx)
            .expect("Pin.getPadstackLayer: Java NPEs on a missing component (Pin.java:250-252)");
        let first_layer = first_layer_of(self, ctx) as i32;
        if component.placed_on_front() || padstack.placed_absolute {
            index + first_layer
        } else {
            padstack.board_layer_count() as i32 - index - first_layer - 1
        }
    }

    pub fn get_shape(&self, index: usize, ctx: &ItemCtx<'_>) -> Option<Shape> {
        if self.shapes.get().is_none() {
            let shapes = self.calculate_shapes(ctx)?;
            let _ = self.shapes.set(shapes);
        }
        self.shapes
            .get()
            .expect("just set")
            .get(index)
            .cloned()
            .flatten()
    }

    fn calculate_shapes(&self, ctx: &ItemCtx<'_>) -> Option<Vec<Option<Shape>>> {
        {
            let padstack = self
                .get_padstack(ctx)
                .expect("Pin.getShape: Java NPEs on a null padstack (Pin.java:166,170)");
            let count = layer_index(
                padstack.to_layer() - padstack.from_layer() + 1,
                "Pin.getShape",
            );
            let component = self.component(ctx)?;
            let package = ctx.library.get_package(component.get_package());
            let package_pin = package.get_pin(self.pin_index)?;

            let component_rotation = component.get_rotation_in_degree();
            let mirror_on_y_axis =
                !component.placed_on_front() && !ctx.components.get_flip_style_rotate_first();
            let rel_location = if mirror_on_y_axis {
                package_pin.relative_location.mirror_at_y_axis()
            } else {
                package_pin.relative_location.clone()
            };
            let component_translation = component
                .get_location()
                .expect("Pin.getShape: Java NPEs on an unplaced component (Pin.java:197)")
                .difference_by(&Point::ZERO);

            let mut shapes: Vec<Option<Shape>> = Vec::with_capacity(count);
            for shape_index in 0..count {
                let padstack_layer = self.get_padstack_layer(shape_index as i32, ctx);
                let Some(base) = padstack.get_shape(padstack_layer) else {
                    shapes.push(None);
                    continue;
                };
                let pin_rotation = package_pin.rotation_in_degree;
                let mut current_shape = if pin_rotation % 90.0 == 0.0 {
                    let factor = pin_rotation as i32 / 90;
                    if factor == 0 {
                        base.clone()
                    } else {
                        base.turn_90_degree(factor, &IntPoint::ZERO)
                    }
                } else {
                    base.rotate_approx(pin_rotation.to_radians(), &FloatPoint::ZERO)
                };
                if mirror_on_y_axis {
                    current_shape = current_shape.mirror_vertical(&IntPoint::ZERO);
                }
                let mut translated_shape = current_shape.translate_by(&rel_location);
                if component_rotation % 90.0 == 0.0 {
                    let factor = component_rotation as i32 / 90;
                    if factor != 0 {
                        translated_shape = translated_shape.turn_90_degree(factor, &IntPoint::ZERO);
                    }
                } else {
                    translated_shape = translated_shape
                        .rotate_approx(component_rotation.to_radians(), &FloatPoint::ZERO);
                }
                if !component.placed_on_front() && ctx.components.get_flip_style_rotate_first() {
                    translated_shape = translated_shape.mirror_vertical(&IntPoint::ZERO);
                }
                shapes.push(Some(translated_shape.translate_by(&component_translation)));
            }
            Some(shapes)
        }
    }

    pub fn get_shape_on_layer(&self, layer: usize, ctx: &ItemCtx<'_>) -> Option<Shape> {
        shape_on_layer_of(self, layer, ctx)
    }

    pub fn get_tile_shape_on_layer(
        &self,
        tree: TreeId,
        layer: usize,
        ctx: &ItemCtx<'_>,
    ) -> Option<&TileShape> {
        self.get_tree_shape_on_layer(tree, layer, ctx)
    }

    pub fn get_tree_shape_on_layer(
        &self,
        tree: TreeId,
        layer: usize,
        ctx: &ItemCtx<'_>,
    ) -> Option<&TileShape> {
        let from_layer = self.first_layer(ctx);
        if layer < from_layer || layer > self.last_layer(ctx) {
            return None;
        }
        self.hdr
            .get_precalculated_tree_shapes(tree)
            .and_then(|shapes| shapes.get(layer - from_layer))
            .and_then(Option::as_ref)
    }

    pub fn first_layer(&self, ctx: &ItemCtx<'_>) -> usize {
        first_layer_of(self, ctx)
    }

    pub fn last_layer(&self, ctx: &ItemCtx<'_>) -> usize {
        last_layer_of(self, ctx)
    }

    pub fn is_on_layer(&self, layer: usize, ctx: &ItemCtx<'_>) -> bool {
        layer >= self.first_layer(ctx) && layer <= self.last_layer(ctx)
    }

    pub fn shape_layer(&self, index: usize, ctx: &ItemCtx<'_>) -> usize {
        shape_layer_of(self, index, ctx)
    }

    pub fn bounding_box(&self, ctx: &ItemCtx<'_>) -> IntBox {
        bounding_box_of(self, ctx)
    }

    pub fn tile_shape_count(&self, ctx: &ItemCtx<'_>) -> usize {
        tile_shape_count_of(self, ctx)
    }

    pub fn smallest_radius(&self, ctx: &ItemCtx<'_>) -> f64 {
        smallest_radius_of(self, &self.get_center(ctx), ctx)
    }

    pub fn min_width(&self, ctx: &ItemCtx<'_>) -> f64 {
        min_width_of(self, ctx)
    }

    pub fn get_min_width(&self, layer: usize, ctx: &ItemCtx<'_>) -> f64 {
        self.padstack_bounding_box(layer, ctx)
            .map_or(0.0, |b| b.min_width())
    }

    pub fn get_max_width(&self, layer: usize, ctx: &ItemCtx<'_>) -> f64 {
        self.padstack_bounding_box(layer, ctx)
            .map_or(0.0, |b| b.max_width())
    }

    fn padstack_bounding_box(&self, layer: usize, ctx: &ItemCtx<'_>) -> Option<IntBox> {
        let padstack_layer =
            self.get_padstack_layer(layer as i32 - first_layer_of(self, ctx) as i32, ctx);
        let padstack = self
            .get_padstack(ctx)
            .expect("Pin.getMinWidth: Java NPEs on a null padstack (Pin.java:494)");
        padstack
            .get_shape(padstack_layer)
            .map(ShapeOps::bounding_box)
    }

    pub fn get_trace_neckdown_halfwidth(&self, layer: usize, ctx: &ItemCtx<'_>) -> i32 {
        let result = (0.5 * self.get_min_width(layer, ctx) - 1.0).max(1.0);
        result as i32
    }

    pub fn drill_allowed(&self, ctx: &ItemCtx<'_>) -> bool {
        self.first_layer(ctx) == self.last_layer(ctx)
    }

    // two `.expect(...)`s that used to fire on the missing component are simply no longer reached
    pub fn get_trace_exit_restrictions(
        &self,
        layer: usize,
        ctx: &ItemCtx<'_>,
    ) -> Vec<TraceExitRestriction> {
        let mut result = Vec::new();
        let Some(component) = self.component(ctx) else {
            return result;
        };
        let first_layer = first_layer_of(self, ctx) as i32;
        let padstack_layer = self.get_padstack_layer(layer as i32 - first_layer, ctx);
        let mut pad_xy_factor = 1.5;

        let package = ctx.library.get_package(component.get_package());
        if package.pin_count() <= 3 {
            pad_xy_factor *= 2.0;
        }

        let padstack = self
            .get_padstack(ctx)
            .expect("Pin.getTraceExitRestrictions: Java NPEs on a null padstack (Pin.java:280)");
        let padstack_exit_directions =
            padstack.get_trace_exit_directions(padstack_layer, pad_xy_factor);
        if padstack_exit_directions.is_empty() {
            return result;
        }
        let shape_index = layer as i32 - first_layer;
        let current_shape = usize::try_from(shape_index)
            .ok()
            .and_then(|i| self.get_shape(i, ctx));
        let Some(Shape::Tile(pad_shape)) = current_shape else {
            return result;
        };
        let component_rotation = component.get_rotation_in_degree();
        let pin_center = self.get_center(ctx);
        let center_approx = pin_center.to_float();

        for padstack_exit_direction in padstack_exit_directions {
            let package = ctx.library.get_package(component.get_package());
            let Some(package_pin) = package.get_pin(self.pin_index) else {
                continue;
            };
            let current_rotation_in_degree = component_rotation + package_pin.rotation_in_degree;
            let current_exit_direction = if current_rotation_in_degree % 45.0 == 0.0 {
                let fortyfive_degree_factor = current_rotation_in_degree as i32 / 45;
                padstack_exit_direction.turn_45_degree(fortyfive_degree_factor)
            } else {
                let current_angle_in_radian = current_rotation_in_degree.to_radians()
                    + padstack_exit_direction.angle_approx();
                Direction::from_angle_approx(current_angle_in_radian)
            };
            let Some(intersecting_border_line_no) =
                pad_shape.intersecting_border_line_no(&pin_center, &current_exit_direction)
            else {
                continue;
            };
            let Point::Int(int_center) = pin_center else {
                continue;
            };
            let Some(current_exit_line) =
                Line::from_direction_any(int_center, &current_exit_direction)
            else {
                continue;
            };
            let border_line = pad_shape
                .border_line(intersecting_border_line_no)
                .expect("intersectingBorderLineNo returned a valid border line index");
            let nearest_border_point = current_exit_line.intersection_approx(&border_line);
            result.push(TraceExitRestriction {
                direction: current_exit_direction,
                min_length: center_approx.distance(&nearest_border_point),
            });
        }
        result
    }

    pub fn has_trace_exit_restrictions(&self, ctx: &ItemCtx<'_>) -> bool {
        (self.first_layer(ctx)..=self.last_layer(ctx))
            .any(|layer| !self.get_trace_exit_restrictions(layer, ctx).is_empty())
    }

    pub fn calc_nearest_exit_restriction_direction(
        &self,
        trace_polyline: &Polyline,
        trace_half_width: i32,
        layer: usize,
        ctx: &ItemCtx<'_>,
    ) -> Option<Direction> {
        let trace_exit_restrictions = self.get_trace_exit_restrictions(layer, ctx);
        if trace_exit_restrictions.is_empty() {
            return None;
        }
        let offset_pin_shape = self.offset_pad_shape(layer, trace_half_width, ctx)?;
        let entries = offset_pin_shape.entrance_points(trace_polyline);
        let latest_entry_tuple = *entries.last()?;
        let trace_entry_location_approx = trace_polyline.lines()[latest_entry_tuple[0]]
            .intersection_approx(
                &offset_pin_shape
                    .border_line(latest_entry_tuple[1])
                    .expect("entrancePoints returns valid border line indices"),
            );

        let mut min_exit_corner_distance = f64::MAX;
        let mut nearest_exit_corner: Option<FloatPoint> = None;
        let mut pin_exit_direction = None;
        const TOLERANCE: f64 = 1.0;
        let pin_center = self.get_center(ctx);
        for current_exit_restriction in &trace_exit_restrictions {
            let Some(current_exit_corner) = self.exit_corner(
                &offset_pin_shape,
                &pin_center,
                &current_exit_restriction.direction,
            ) else {
                continue;
            };
            let current_exit_corner_distance =
                current_exit_corner.distance_square(&trace_entry_location_approx);
            let mut new_nearest_corner_found = false;
            if current_exit_corner_distance + TOLERANCE < min_exit_corner_distance {
                new_nearest_corner_found = true;
            } else if current_exit_corner_distance < min_exit_corner_distance + TOLERANCE {
                for i in 1..trace_polyline.corner_count() {
                    let Some(current_trace_corner) = trace_polyline.corner_approx(i) else {
                        break;
                    };
                    let current_trace_corner_distance =
                        current_trace_corner.distance_square(&current_exit_corner);
                    let old_trace_corner_distance = current_trace_corner.distance_square(
                        nearest_exit_corner
                            .as_ref()
                            .expect("Java NPEs here; unreachable on the first iteration"),
                    );
                    if current_trace_corner_distance + TOLERANCE < old_trace_corner_distance {
                        new_nearest_corner_found = true;
                        break;
                    } else if current_trace_corner_distance > old_trace_corner_distance + TOLERANCE
                    {
                        break;
                    }
                }
            }
            if new_nearest_corner_found {
                min_exit_corner_distance = current_exit_corner_distance;
                pin_exit_direction = Some(current_exit_restriction.direction.clone());
                nearest_exit_corner = Some(current_exit_corner);
            }
        }
        pin_exit_direction
    }

    pub fn nearest_trace_exit_corner(
        &self,
        from_point: &FloatPoint,
        trace_half_width: i32,
        layer: usize,
        ctx: &ItemCtx<'_>,
    ) -> Option<FloatPoint> {
        let trace_exit_restrictions = self.get_trace_exit_restrictions(layer, ctx);
        if trace_exit_restrictions.is_empty() {
            return None;
        }
        let pin_center = self.get_center(ctx);
        let offset_pin_shape = self.offset_pad_shape(layer, trace_half_width, ctx)?;
        let mut min_exit_corner_distance = f64::MAX;
        let mut nearest_exit_corner = None;
        for current_exit_restriction in &trace_exit_restrictions {
            let Some(current_exit_corner) = self.exit_corner(
                &offset_pin_shape,
                &pin_center,
                &current_exit_restriction.direction,
            ) else {
                continue;
            };
            let current_exit_corner_distance = current_exit_corner.distance_square(from_point);
            if current_exit_corner_distance < min_exit_corner_distance {
                min_exit_corner_distance = current_exit_corner_distance;
                nearest_exit_corner = Some(current_exit_corner);
            }
        }
        nearest_exit_corner
    }

    fn offset_pad_shape(
        &self,
        layer: usize,
        trace_half_width: i32,
        ctx: &ItemCtx<'_>,
    ) -> Option<TileShape> {
        let shape_index = usize::try_from(layer as i32 - first_layer_of(self, ctx) as i32).ok()?;
        let Some(Shape::Tile(pin_shape)) = self.get_shape(shape_index, ctx) else {
            return None;
        };
        let edge_to_turn_dist = ctx.rules.get_pin_edge_to_turn_dist();
        if edge_to_turn_dist < 0.0 {
            return None;
        }
        Some(pin_shape.offset(edge_to_turn_dist + f64::from(trace_half_width)))
    }

    fn exit_corner(
        &self,
        offset_pin_shape: &TileShape,
        pin_center: &Point,
        direction: &Direction,
    ) -> Option<FloatPoint> {
        let border_line_no = offset_pin_shape.intersecting_border_line_no(pin_center, direction)?;
        let Point::Int(int_center) = pin_center else {
            return None;
        };
        let pin_exit_ray = Line::from_direction_any(*int_center, direction)?;
        let border_line = offset_pin_shape.border_line(border_line_no)?;
        Some(pin_exit_ray.intersection_approx(&border_line))
    }

    pub fn swap(&mut self, other: &mut Pin, nets: &Nets) -> bool {
        if self.hdr.net_count() > 1 || other.hdr.net_count() > 1 {
            return false;
        }
        let this_net_no = net_no_of_pin(self);
        let other_net_no = net_no_of_pin(other);
        self.hdr.assign_net_no(other_net_no, nets);
        other.hdr.assign_net_no(this_net_no, nets);
        let this_changed_to = self.get_changed_to();
        let other_changed_to = other.get_changed_to();
        self.changed_to = (other_changed_to != self.hdr.id()).then_some(other_changed_to);
        other.changed_to = (this_changed_to != other.hdr.id()).then_some(this_changed_to);
        true
    }

    pub fn get_changed_to(&self) -> ItemId {
        self.changed_to.unwrap_or_else(|| self.hdr.id())
    }

    pub fn turn_90_degree(&mut self, _factor: i32, _pole: &IntPoint) {
        self.drill.clear_center();
        self.clear_derived_data();
    }

    pub fn rotate_approx(&mut self, _angle_in_degree: f64, _pole: &FloatPoint) {
        self.drill.clear_center();
        self.clear_derived_data();
    }

    pub fn change_placement_side(&mut self, _pole: &IntPoint) {
        self.drill.clear_center();
        self.clear_derived_data();
    }

    /// The centre derives from the component's location, which the caller has already moved:
    /// shifting a cached centre as well would move the pin twice whenever the move itself had
    /// just computed it.
    pub fn translate_by(&mut self, _vector: &Vector) {
        self.drill.clear_center();
        self.clear_derived_data();
    }

    pub fn clear_derived_data(&mut self) {
        self.hdr.clear_derived_data();
        self.drill.clear_derived_data();
        self.shapes.take();
    }
}

fn net_no_of_pin(pin: &Pin) -> i32 {
    if pin.hdr.net_count() > 0 {
        pin.hdr.get_net_number(0)
    } else {
        0
    }
}

impl Connectable for Pin {
    fn header(&self) -> &ItemHeader {
        &self.hdr
    }

    fn get_trace_connection_shape(
        &self,
        _tree: TreeId,
        _index: usize,
        ctx: &ItemCtx<'_>,
    ) -> Option<TileShape> {
        Some(trace_connection_shape_of(&self.get_center(ctx)))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TraceExitRestriction {
    pub direction: Direction,
    pub min_length: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::Item;

    #[test]
    fn item_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DrillItemData>();
        assert_send_sync::<Via>();
        assert_send_sync::<Pin>();
        assert_send_sync::<Item>();
    }
}
