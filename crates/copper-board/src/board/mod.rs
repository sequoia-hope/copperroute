pub mod changed_area;
pub mod clearance;
pub mod clearance_override;
pub mod communication;
pub mod connectivity;
mod contact_cache;
pub mod item_store;
pub mod normalize;
pub mod query;
pub mod shape_trace_entries;
pub mod snapshot;
pub mod trace_normalize;

use item_store::ItemStore;
use std::borrow::Cow;

use copper_geometry::{Area, FloatPoint, IntBox, Point, Polyline, PolylineShapeRef, TileShape, Vector};

pub use changed_area::ChangedArea;
pub use clearance_override::{
    BOARD_EDGE_CLEARANCE_CLASS_NAME, DEFAULT_COPPER_TO_EDGE_CLEARANCE_UM,
    HOLE_EDGE_CLEARANCE_CLASS_NAME,
};
pub use communication::{Communication, WriteResolution};
pub use connectivity::StopConnectionOption;
pub use normalize::MAX_NORMALIZE_ITERATIONS;
pub use shape_trace_entries::ShapeTraceEntries;
pub use trace_normalize::MAX_NORMALIZATION_DEPTH;

use crate::ids::{ItemId, TreeId};
use crate::items::{
    ComponentObstacleArea, ComponentOutline, ConductionArea, Item, ItemCtx, ItemHeader,
    ObstacleArea, ObstacleAreaData, Pin, PolylineTrace, Via, ViaObstacleArea,
};
use crate::library::BoardLibrary;
use crate::rules::BoardRules;
use crate::searchtree::SearchTreeManager;
use crate::structure::{BoardOutline, Components, FixedState, LayerStructure};

/// Builds an [`ItemCtx`] from a board's own fields.
///
/// A macro rather than a `fn ctx(&self)` on purpose: a method borrows the *whole* board, so it
/// could not be combined with `self.items.get_mut(...)` or `&mut self.trees` in the same
/// expression. Expanded inline, the four field borrows are disjoint from `items` and `trees` and
/// the borrow checker accepts them.
macro_rules! item_ctx {
    ($board:expr) => {
        $crate::items::ItemCtx {
            library: &$board.library,
            components: &$board.components,
            rules: &$board.rules,
            bounding_box: &$board.bounding_box,
            max_tree_shape_width: $board.max_tree_shape_width,
        }
    };
}
pub(crate) use item_ctx;

#[derive(Debug, Clone, PartialEq)]
pub struct Board {
    pub items: item_store::ItemStore<Item>,
    pub components: Components,
    pub rules: BoardRules,
    pub library: BoardLibrary,
    pub communication: Communication,
    pub bounding_box: IntBox,
    pub trees: SearchTreeManager,
    pub changed_area: Option<ChangedArea>,
    pub shove_failing_obstacle: Option<ItemId>,
    pub shove_failing_layer: i32,

    pub normalize_suppressed_net_nos: std::collections::BTreeSet<i32>,

    revision: u64,
    contact_cache: contact_cache::ContactCache,
    max_trace_half_width: i32,
    min_trace_half_width: i32,
    max_tree_shape_width: f64,

    undo_journal: Option<crate::board::snapshot::UndoJournal>,
}

impl Board {
    pub fn set_flip_style_rotate_first(&mut self, value: bool) {
        if self.components.get_flip_style_rotate_first() == value {
            return;
        }
        self.components.set_flip_style_rotate_first(value);
        for item in self.items.values_mut() {
            item.clear_derived_data();
        }
    }

    // -- construction ---------------------------------------------------------------------------

    pub fn new(
        outline_shapes: Vec<PolylineShapeRef>,
        outline_clearance_class: usize,
        bounding_box: IntBox,
        rules: BoardRules,
        library: BoardLibrary,
        components: Components,
        communication: Communication,
    ) -> Board {
        let mut max_tree_shape_width = crate::items::DEFAULT_MAX_TREE_SHAPE_WIDTH;
        if communication.host_cad_exists() {
            max_tree_shape_width = max_tree_shape_width
                .min(500.0 * communication.get_resolution(crate::structure::Unit::Mil));
        }
        let mut board = Board {
            items: ItemStore::new(),
            components,
            rules,
            library,
            communication,
            bounding_box,
            trees: SearchTreeManager::new(),
            changed_area: None,
            shove_failing_obstacle: None,
            shove_failing_layer: -1,
            normalize_suppressed_net_nos: std::collections::BTreeSet::new(),
            revision: 0,
            contact_cache: contact_cache::ContactCache::default(),
            max_trace_half_width: 1000,
            min_trace_half_width: 10000,
            max_tree_shape_width,
            undo_journal: None,
        };
        board.insert_outline(outline_shapes, outline_clearance_class);
        board
    }

    pub fn layer_structure(&self) -> &LayerStructure {
        self.rules.layer_structure()
    }

    pub fn get_layer_count(&self) -> usize {
        self.layer_structure().count()
    }

    pub fn ctx(&self) -> ItemCtx<'_> {
        item_ctx!(self)
    }

    pub fn default_tree_id(&self) -> TreeId {
        self.trees.get_default_tree().id()
    }

    // -- revision -------------------------------------------------------------------------------

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn increment_revision(&mut self) {
        self.revision += 1;
    }

    // -- the insert/remove protocol (BoardItemRepository) ----------------------------------------

    #[track_caller]
    pub fn new_item_id(&mut self) -> ItemId {
        let id = self.communication.id_gen.new_id();
        if p7t8b_ids_ledger() && id.0 >= p7t8b_ids_from() {
            let caller = std::panic::Location::caller();
            eprintln!("ID {} {}:{}", id.0, caller.file(), caller.line());
            if p7t8b_ids_backtrace() {
                eprintln!(
                    "IDBT {}\n{}",
                    id.0,
                    std::backtrace::Backtrace::force_capture()
                );
            }
        }
        id
    }

    pub fn insert_item(&mut self, mut item: Item) -> ItemId {
        self.invalidate_cached_contacts();
        if item.clearance_class() >= self.rules.clearance_matrix.get_class_count() {
            item.set_clearance_class(0, &self.rules);
        }
        let id = item.id();
        debug_assert!(
            id.0 > 0,
            "Board::insert_item: item ids come from Board::new_item_id and start at 1 \
             (ItemIdGenerator.java:37-55)"
        );
        self.items.insert(id, item);
        self.journal_insert(id);
        let ctx = item_ctx!(self);
        let inserted = self
            .items
            .get_mut(&id)
            .expect("Board::insert_item: just inserted");
        self.trees.insert(inserted, &ctx);
        self.revision += 1;
        id
    }

    pub fn remove_item(&mut self, id: ItemId) -> bool {
        self.invalidate_cached_contacts();
        let Some(item) = self.items.get(&id) else {
            return false;
        };
        if item.is_deletion_forbidden(&self.rules) {
            return false;
        }
        let item = self
            .items
            .get_mut(&id)
            .expect("Board::remove_item: present, just checked");
        self.trees.remove(item);
        let removed = self.items.remove(&id);
        self.journal_remove(id, removed.as_ref());
        self.revision += 1;
        true
    }

    pub fn remove_items(&mut self, ids: impl IntoIterator<Item = ItemId>) -> bool {
        let mut result = true;
        for id in ids {
            let Some(item) = self.items.get(&id) else {
                continue;
            };
            if item.is_deletion_forbidden(&self.rules) || item.is_user_fixed() {
                result = false;
            } else {
                self.remove_item(id);
            }
        }
        result
    }

    // -- the typed inserters (BasicBoard) --------------------------------------------------------

    pub fn insert_outline(
        &mut self,
        outline_shapes: Vec<PolylineShapeRef>,
        clearance_class: usize,
    ) -> ItemId {
        let id = self.new_item_id();
        let outline = BoardOutline::new(
            ItemHeader::new(id, Vec::new(), clearance_class, 0, FixedState::SystemFixed),
            outline_shapes,
        );
        self.insert_item(Item::BoardOutline(outline))
    }

    pub fn insert_trace_without_cleaning(
        &mut self,
        polyline: Polyline,
        layer: usize,
        half_width: i32,
        net_nos: Vec<i32>,
        clearance_class: usize,
        fixed_state: FixedState,
    ) -> Option<ItemId> {
        if polyline.corner_count() < 2 {
            return None;
        }
        let id = self.new_item_id();
        let layer_count = self.get_layer_count();
        let new_trace = PolylineTrace::new(
            ItemHeader::new(id, net_nos, clearance_class, 0, fixed_state),
            polyline,
            layer,
            half_width,
            Some(layer_count),
        );
        if new_trace.first_corner() == new_trace.last_corner()
            && (fixed_state as u8) < (FixedState::UserFixed as u8)
        {
            return None;
        }
        let nets_normal = new_trace.hdr.nets_normal();
        self.insert_item(Item::Trace(new_trace));
        if nets_normal {
            self.max_trace_half_width = self.max_trace_half_width.max(half_width);
            self.min_trace_half_width = self.min_trace_half_width.min(half_width);
        }
        Some(id)
    }

    pub fn insert_trace(
        &mut self,
        polyline: Polyline,
        layer: usize,
        half_width: i32,
        net_nos: Vec<i32>,
        clearance_class: usize,
        fixed_state: FixedState,
    ) -> Option<ItemId> {
        let id = self.insert_trace_without_cleaning(
            polyline,
            layer,
            half_width,
            net_nos,
            clearance_class,
            fixed_state,
        )?;
        let clip_shape = self.changed_area.as_ref().map(|area| area.get_area(layer));
        let _ = self.normalize_trace(id, clip_shape.as_ref());
        Some(id)
    }

    pub fn insert_trace_at_points(
        &mut self,
        points: &[Point],
        layer: usize,
        half_width: i32,
        net_nos: Vec<i32>,
        clearance_class: usize,
        fixed_state: FixedState,
    ) -> Option<ItemId> {
        let polyline = Polyline::from_points(points);
        self.insert_trace(
            polyline,
            layer,
            half_width,
            net_nos,
            clearance_class,
            fixed_state,
        )
    }

    pub fn insert_via(
        &mut self,
        padstack: crate::ids::PadstackId,
        center: Point,
        net_nos: Vec<i32>,
        clearance_class: usize,
        fixed_state: FixedState,
        attach_allowed: bool,
    ) -> Result<ItemId, crate::BoardError> {
        self.insert_via_checked(
            padstack,
            center,
            net_nos,
            clearance_class,
            fixed_state,
            attach_allowed,
            &|| false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_via_checked(
        &mut self,
        padstack: crate::ids::PadstackId,
        center: Point,
        net_nos: Vec<i32>,
        clearance_class: usize,
        fixed_state: FixedState,
        attach_allowed: bool,
        stop: crate::datastructures::StopCheck<'_>,
    ) -> Result<ItemId, crate::BoardError> {
        let id = self.new_item_id();
        let via = Via::new(
            ItemHeader::new(id, net_nos.clone(), clearance_class, 0, fixed_state),
            padstack,
            center.clone(),
            attach_allowed,
        );
        self.insert_item(Item::Via(via));
        let (from_layer, to_layer) = self.padstack_layer_range(padstack);
        for layer in from_layer..to_layer {
            for net_number in &net_nos {
                self.split_traces_checked(&center, layer as usize, *net_number, stop)?;
            }
        }
        Ok(id)
    }

    pub fn insert_escape_via(
        &mut self,
        padstack: crate::ids::PadstackId,
        center: Point,
        net_nos: Vec<i32>,
        clearance_class: usize,
        fixed_state: FixedState,
        smd_layer: usize,
    ) -> Result<ItemId, crate::BoardError> {
        self.insert_escape_via_checked(
            padstack,
            center,
            net_nos,
            clearance_class,
            fixed_state,
            smd_layer,
            &|| false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_escape_via_checked(
        &mut self,
        padstack: crate::ids::PadstackId,
        center: Point,
        net_nos: Vec<i32>,
        clearance_class: usize,
        fixed_state: FixedState,
        smd_layer: usize,
        stop: crate::datastructures::StopCheck<'_>,
    ) -> Result<ItemId, crate::BoardError> {
        let id = self.new_item_id();
        let mut via = Via::new(
            ItemHeader::new(id, net_nos.clone(), clearance_class, 0, fixed_state),
            padstack,
            center.clone(),
            true,
        );
        via.is_escape_via = true;
        via.escape_via_smd_layer = smd_layer as i32;
        self.insert_item(Item::Via(via));
        let (from_layer, to_layer) = self.padstack_layer_range(padstack);
        for layer in from_layer..=to_layer {
            for net_number in &net_nos {
                self.split_traces_checked(&center, layer as usize, *net_number, stop)?;
            }
        }
        Ok(id)
    }

    fn padstack_layer_range(&self, padstack: crate::ids::PadstackId) -> (i32, i32) {
        let padstack = self
            .library
            .padstacks
            .get(padstack)
            .expect("Board::insertVia: the padstack of an inserted via is in the library");
        (padstack.from_layer(), padstack.to_layer())
    }

    pub fn insert_pin(
        &mut self,
        component_id: i32,
        pin_index: i32,
        net_nos: Vec<i32>,
        clearance_class: usize,
        fixed_state: FixedState,
    ) -> ItemId {
        let id = self.new_item_id();
        let pin = Pin::new(
            ItemHeader::new(id, net_nos, clearance_class, component_id, fixed_state),
            pin_index,
        );
        self.insert_item(Item::Pin(pin))
    }

    pub fn insert_obstacle(
        &mut self,
        area: Area,
        layer: usize,
        clearance_class: usize,
        fixed_state: FixedState,
    ) -> ItemId {
        self.insert_obstacle_of_component(
            area,
            layer,
            Vector::ZERO,
            0.0,
            false,
            clearance_class,
            0,
            None,
            fixed_state,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_obstacle_of_component(
        &mut self,
        area: Area,
        layer: usize,
        translation: Vector,
        rotation_in_degree: f64,
        side_changed: bool,
        clearance_class: usize,
        component_id: i32,
        name: Option<String>,
        fixed_state: FixedState,
    ) -> ItemId {
        let id = self.new_item_id();
        let obstacle = ObstacleArea::new(
            ItemHeader::new(id, Vec::new(), clearance_class, component_id, fixed_state),
            ObstacleAreaData::new(
                area,
                layer,
                translation,
                rotation_in_degree,
                side_changed,
                name,
            ),
        );
        self.insert_item(Item::ObstacleArea(obstacle))
    }

    pub fn insert_via_obstacle(
        &mut self,
        area: Area,
        layer: usize,
        clearance_class: usize,
        fixed_state: FixedState,
    ) -> ItemId {
        self.insert_via_obstacle_of_component(
            area,
            layer,
            Vector::ZERO,
            0.0,
            false,
            clearance_class,
            0,
            None,
            fixed_state,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_via_obstacle_of_component(
        &mut self,
        area: Area,
        layer: usize,
        translation: Vector,
        rotation_in_degree: f64,
        side_changed: bool,
        clearance_class: usize,
        component_id: i32,
        name: Option<String>,
        fixed_state: FixedState,
    ) -> ItemId {
        let id = self.new_item_id();
        let obstacle = ViaObstacleArea::new(
            ItemHeader::new(id, Vec::new(), clearance_class, component_id, fixed_state),
            ObstacleAreaData::new(
                area,
                layer,
                translation,
                rotation_in_degree,
                side_changed,
                name,
            ),
        );
        self.insert_item(Item::ViaObstacleArea(obstacle))
    }

    pub fn insert_component_obstacle(
        &mut self,
        area: Area,
        layer: usize,
        clearance_class: usize,
        fixed_state: FixedState,
    ) -> ItemId {
        self.insert_component_obstacle_of_component(
            area,
            layer,
            Vector::ZERO,
            0.0,
            false,
            clearance_class,
            0,
            None,
            fixed_state,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_component_obstacle_of_component(
        &mut self,
        area: Area,
        layer: usize,
        translation: Vector,
        rotation_in_degree: f64,
        side_changed: bool,
        clearance_class: usize,
        component_id: i32,
        name: Option<String>,
        fixed_state: FixedState,
    ) -> ItemId {
        let id = self.new_item_id();
        let obstacle = ComponentObstacleArea::new(
            ItemHeader::new(id, Vec::new(), clearance_class, component_id, fixed_state),
            ObstacleAreaData::new(
                area,
                layer,
                translation,
                rotation_in_degree,
                side_changed,
                name,
            ),
        );
        self.insert_item(Item::ComponentObstacleArea(obstacle))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_component_outline(
        &mut self,
        area: Area,
        is_front: bool,
        translation: Vector,
        rotation_in_degree: f64,
        component_id: i32,
        is_courtyard: bool,
        is_fabrication: bool,
        is_closed: bool,
        fixed_state: FixedState,
    ) -> Option<ItemId> {
        if !area.is_bounded() {
            return None;
        }
        let id = self.new_item_id();
        let outline = ComponentOutline::new(
            ItemHeader::new(id, Vec::new(), 0, component_id, fixed_state),
            area,
            is_front,
            translation,
            rotation_in_degree,
            is_courtyard,
            is_fabrication,
            is_closed,
        );
        Some(self.insert_item(Item::ComponentOutline(outline)))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_conduction_area(
        &mut self,
        area: Area,
        layer: usize,
        net_nos: Vec<i32>,
        clearance_class: usize,
        is_obstacle: bool,
        fixed_state: FixedState,
    ) -> ItemId {
        let id = self.new_item_id();
        let conduction = ConductionArea::new(
            ItemHeader::new(id, net_nos, clearance_class, 0, fixed_state),
            ObstacleAreaData::new(area, layer, Vector::ZERO, 0.0, false, None),
            is_obstacle,
        );
        self.insert_item(Item::ConductionArea(conduction))
    }

    pub fn make_conductive(&mut self, id: ItemId, net_number: i32) -> Option<ItemId> {
        let Some(Item::ObstacleArea(area)) = self.items.get(&id) else {
            return None;
        };
        let clearance_class = area.hdr.clearance_class();
        let component_id = area.hdr.get_component_id();
        let fixed_state = area.hdr.get_fixed_state();
        let data = ObstacleAreaData::new(
            area.get_relative_area().clone(),
            area.get_layer(),
            area.get_translation().clone(),
            area.get_rotation_in_degree(),
            area.get_side_changed(),
            area.name().map(str::to_string),
        );
        let new_id = self.new_item_id();
        let new_item = ConductionArea::new(
            ItemHeader::new(
                new_id,
                vec![net_number],
                clearance_class,
                component_id,
                fixed_state,
            ),
            data,
            true,
        );
        self.remove_item(id);
        Some(self.insert_item(Item::ConductionArea(new_item)))
    }

    // -- item-list queries (BoardItemRepository / BoardConnectivityQueries) -----------------------

    pub fn get_item(&self, id: ItemId) -> Option<&Item> {
        self.items.get(&id)
    }

    pub fn get_item_mut(&mut self, id: ItemId) -> Option<&mut Item> {
        self.invalidate_cached_contacts();
        self.items.get_mut(&id)
    }

    pub fn get_items(&self) -> impl DoubleEndedIterator<Item = &Item> {
        self.items.values().rev()
    }

    pub fn items_in_board_order(&self) -> Vec<ItemId> {
        self.items.keys().rev().collect()
    }

    pub fn get_outline(&self) -> Option<ItemId> {
        self.items
            .iter()
            .rev()
            .find(|(_, item)| matches!(item, Item::BoardOutline(_)))
            .map(|(id, _)| id)
    }

    pub fn get_conduction_areas(&self) -> Vec<ItemId> {
        self.ids_where(|item| matches!(item, Item::ConductionArea(_)))
    }

    pub fn get_pins(&self) -> Vec<ItemId> {
        self.ids_where(|item| matches!(item, Item::Pin(_)))
    }

    pub fn get_smd_pins(&self) -> Vec<ItemId> {
        let ctx = self.ctx();
        self.ids_where(|item| {
            matches!(item, Item::Pin(_)) && item.first_layer(&ctx) == item.last_layer(&ctx)
        })
    }

    pub fn get_vias(&self) -> Vec<ItemId> {
        self.ids_where(|item| matches!(item, Item::Via(_)))
    }

    pub fn get_traces(&self) -> Vec<ItemId> {
        self.ids_where(Item::is_trace)
    }

    pub fn cumulative_trace_length(&self) -> f64 {
        self.items
            .values()
            .rev()
            .filter_map(|item| match item {
                Item::Trace(trace) => Some(trace.get_length()),
                _ => None,
            })
            .fold(0.0, |result, length| result + length)
    }

    pub fn get_non_45_degree_trace_count(&self) -> usize {
        self.items
            .values()
            .rev()
            .filter(|item| match item {
                Item::Trace(trace) => !trace.polyline().is_multiple_of_45_degree(),
                _ => false,
            })
            .count()
    }

    pub fn delete_all_tracks_and_vias(&mut self) {
        for id in self.items_in_board_order() {
            let Some(item) = self.items.get(&id) else {
                continue;
            };
            if !matches!(item, Item::Trace(_) | Item::Via(_)) {
                continue;
            }
            let item = self
                .items
                .get_mut(&id)
                .expect("Board::delete_all_tracks_and_vias: present, just checked");
            self.trees.remove(item);
            let removed = self.items.remove(&id);
            self.journal_remove(id, removed.as_ref());
        }
    }

    pub fn unfill_conduction_areas(&mut self) {
        self.rules.set_ignore_conduction(true);
        for item in self.items.values_mut() {
            if let Item::ConductionArea(area) = item {
                area.set_is_filled(false);
                area.set_is_obstacle(false);
            }
        }
        self.reinsert_tree_items();
    }

    pub fn change_conduction_is_obstacle(&mut self, value: bool) {
        let mut something_changed = false;
        for item in self.items.values_mut() {
            if let Item::ConductionArea(area) = item {
                let is_signal = self.rules.layer_structure().layers[area.get_layer()].is_signal;
                if is_signal && area.get_is_obstacle() != value {
                    area.set_is_obstacle(value);
                    something_changed = true;
                }
            }
        }
        self.rules.set_ignore_conduction(!value);
        if something_changed {
            self.reinsert_tree_items();
        }
    }

    pub fn reinsert_tree_items(&mut self) {
        let mut items = std::mem::take(&mut self.items);
        let ctx = item_ctx!(self);
        let mut refs: Vec<&mut Item> = items.ordered_values_mut();
        refs.reverse();
        self.trees.reinsert_tree_shapes(&mut refs, &ctx);
        drop(refs);
        self.items = items;
    }

    pub fn set_clearance_compensation_used(&mut self, value: bool) {
        let mut items = std::mem::take(&mut self.items);
        let ctx = item_ctx!(self);
        let mut refs: Vec<&mut Item> = items.ordered_values_mut();
        refs.reverse();
        self.trees
            .set_clearance_compensation_used(value, &mut refs, &ctx);
        drop(refs);
        self.items = items;
    }

    pub fn generate_keepout_outside(&mut self, id: ItemId, value: bool) -> bool {
        let Some(Item::BoardOutline(outline)) = self.items.get(&id) else {
            return false;
        };
        if outline.keepout_outside_outline_generated() == value {
            return false;
        }
        let mut item = self
            .items
            .remove(&id)
            .expect("Board::generate_keepout_outside: present, just checked");
        self.trees.remove(&mut item);
        if let Item::BoardOutline(outline) = &mut item {
            outline.generate_keepout_outside(value);
        }
        let ctx = item_ctx!(self);
        self.trees.insert(&mut item, &ctx);
        self.items.insert(id, item);
        true
    }

    pub fn change_clearance_class_index(&mut self, id: ItemId, index: usize) -> bool {
        if !self.items.contains_key(&id) {
            return false;
        }
        {
            let rules = &self.rules;
            let item = self
                .items
                .get_mut(&id)
                .expect("Board::change_clearance_class_index: present, just checked");
            item.set_clearance_class(index, rules);
            item.clear_derived_data();
        }
        if self.trees.is_clearance_compensation_used() {
            let mut item = self
                .items
                .remove(&id)
                .expect("Board::change_clearance_class_index: present, just checked");
            self.trees.remove(&mut item);
            let ctx = item_ctx!(self);
            self.trees.insert(&mut item, &ctx);
            self.items.insert(id, item);
        }
        true
    }

    /// Turns the item about `pole` and re-enters it into the search trees. Unlike `move_item_by`,
    /// no traces are added to keep contacts: the caller moves what touches it.
    pub fn rotate_item_approx(&mut self, id: ItemId, angle_in_degree: f64, pole: &FloatPoint) -> bool {
        self.invalidate_cached_contacts();
        if !self.items.contains_key(&id) {
            return false;
        }
        self.save_for_undo(id);
        let mut item = self
            .items
            .remove(&id)
            .expect("Board::rotate_item_approx: present, just checked");
        self.trees.remove(&mut item);
        let ctx = item_ctx!(self);
        item.rotate_approx(angle_in_degree, pole, &ctx);
        self.trees.insert(&mut item, &ctx);
        self.items.insert(id, item);
        self.revision += 1;
        true
    }

    pub fn move_item_by(&mut self, id: ItemId, vector: &Vector) -> Result<bool, crate::BoardError> {
        self.invalidate_cached_contacts();
        let Some(item) = self.items.get(&id) else {
            return Ok(false);
        };
        let is_drill_item = item.is_drill_item();
        let old_center = is_drill_item.then(|| self.drill_center(id).expect("a drill item"));
        let mut by_layer: std::collections::BTreeMap<usize, (i32, usize)> =
            std::collections::BTreeMap::new();
        if is_drill_item {
            for contact_id in self.normal_contacts(id).into_iter().rev() {
                if let Some(Item::Trace(trace)) = self.items.get(&contact_id) {
                    by_layer
                        .entry(trace.get_layer())
                        .or_insert((trace.get_half_width(), trace.hdr.clearance_class()));
                }
            }
        }
        let contact_trace_info: Vec<(usize, i32, usize)> = by_layer
            .into_iter()
            .rev()
            .map(|(layer, (half_width, clearance_class))| (layer, half_width, clearance_class))
            .collect();

        self.save_for_undo(id);
        let mut item = self
            .items
            .remove(&id)
            .expect("Board::move_item_by: present, just checked");
        self.trees.remove(&mut item);
        let translated = item.translate_by(vector);
        let ctx = item_ctx!(self);
        self.trees.insert(&mut item, &ctx);
        let net_nos = item.net_nos().to_vec();
        self.items.insert(id, item);
        translated.map_err(crate::BoardError::Normalization)?;

        if let Some(old_center) = old_center {
            let new_center = self.drill_center(id).expect("still a drill item");
            let mut connect_points = vec![old_center.clone()];
            if let (Point::Int(from), Point::Int(to)) = (&old_center, &new_center) {
                let add_corner = match self.rules.trace_angle_restriction {
                    crate::structure::AngleRestriction::NinetyDegree => {
                        Some(from.ninety_degree_corner(to, true))
                    }
                    crate::structure::AngleRestriction::FortyFiveDegree => {
                        Some(from.fortyfive_degree_corner(to, true))
                    }
                    crate::structure::AngleRestriction::None => None,
                };
                if let Some(Some(corner)) = add_corner {
                    connect_points.push(Point::Int(corner));
                }
            }
            connect_points.push(new_center);
            for (layer, half_width, clearance_class) in contact_trace_info {
                self.insert_trace_at_points(
                    &connect_points,
                    layer,
                    half_width,
                    net_nos.clone(),
                    clearance_class,
                    FixedState::Unfixed,
                );
            }
        }
        Ok(true)
    }

    pub fn trace_has_default_entries(&self, first: ItemId, second: ItemId) -> bool {
        let default_tree = self.default_tree_id();
        [first, second].into_iter().all(|id| {
            self.items
                .get(&id)
                .is_some_and(|item| item.get_search_tree_entries(default_tree).is_some())
        })
    }

    pub fn replace_trace_geometry(&mut self, id: ItemId, new_polyline: Polyline) -> bool {
        self.invalidate_cached_contacts();
        if !matches!(self.items.get(&id), Some(Item::Trace(_))) {
            return false;
        }
        let mut item = self
            .items
            .remove(&id)
            .expect("Board::replace_trace_geometry: present, just checked");
        self.trees.remove(&mut item);
        item.clear_tree_entries();
        if let Item::Trace(trace) = &mut item {
            trace.set_polyline(new_polyline);
        }
        item.clear_derived_data();
        let ctx = item_ctx!(self);
        self.trees.insert(&mut item, &ctx);
        self.items.insert(id, item);
        true
    }

    pub fn merge_trace_entries_in_front(
        &mut self,
        from_trace: ItemId,
        to_trace: ItemId,
        joined_polyline: &Polyline,
        from_entry_no: usize,
        to_entry_no: usize,
    ) -> bool {
        self.invalidate_cached_contacts();
        self.with_two_traces(from_trace, to_trace, |trees, rules, from, to| {
            trees.merge_entries_in_front(
                from,
                to,
                joined_polyline,
                from_entry_no,
                to_entry_no,
                rules,
            );
        })
    }

    pub fn merge_trace_entries_at_end(
        &mut self,
        from_trace: ItemId,
        to_trace: ItemId,
        joined_polyline: &Polyline,
        from_entry_no: usize,
        to_entry_no: usize,
    ) -> bool {
        self.invalidate_cached_contacts();
        self.with_two_traces(from_trace, to_trace, |trees, rules, from, to| {
            trees.merge_entries_at_end(
                from,
                to,
                joined_polyline,
                from_entry_no,
                to_entry_no,
                rules,
            );
        })
    }

    pub fn change_trace_entries(
        &mut self,
        id: ItemId,
        new_polyline: &Polyline,
        keep_at_start_count: usize,
        keep_at_end_count: usize,
    ) -> bool {
        self.invalidate_cached_contacts();
        let Some(Item::Trace(_)) = self.items.get(&id) else {
            return false;
        };
        let mut item = self.items.remove(&id).expect("present, just checked");
        if let Item::Trace(trace) = &mut item {
            self.trees.change_entries(
                trace,
                new_polyline,
                keep_at_start_count,
                keep_at_end_count,
                &self.rules,
            );
        }
        self.items.insert(id, item);
        true
    }

    fn with_two_traces(
        &mut self,
        from_id: ItemId,
        to_id: ItemId,
        f: impl FnOnce(&mut SearchTreeManager, &BoardRules, &mut PolylineTrace, &mut PolylineTrace),
    ) -> bool {
        if from_id == to_id
            || !matches!(self.items.get(&from_id), Some(Item::Trace(_)))
            || !matches!(self.items.get(&to_id), Some(Item::Trace(_)))
        {
            return false;
        }
        let mut from_item = self.items.remove(&from_id).expect("present, just checked");
        let mut to_item = self.items.remove(&to_id).expect("present, just checked");
        if let (Item::Trace(from), Item::Trace(to)) = (&mut from_item, &mut to_item) {
            f(&mut self.trees, &self.rules, from, to);
        }
        self.items.insert(from_id, from_item);
        self.items.insert(to_id, to_item);
        true
    }

    fn fill_tree_shapes(&mut self, id: ItemId, tree: TreeId) -> Option<usize> {
        let item = self.items.get(&id)?;
        if let Some(shapes) = item.header().get_precalculated_tree_shapes(tree) {
            return Some(shapes.len());
        }
        let search_tree = self.trees.trees().find(|t| t.id() == tree)?;
        let ctx = item_ctx!(self);
        let shapes = search_tree.calculate_tree_shapes(item, &ctx);
        let len = shapes.len();
        self.items
            .get_mut(&id)
            .expect("Board::fill_tree_shapes: present, just read")
            .set_precalculated_tree_shapes(tree, shapes);
        Some(len)
    }

    pub fn item_tree_shape_count(&mut self, id: ItemId, tree: TreeId) -> usize {
        self.fill_tree_shapes(id, tree).unwrap_or(0)
    }

    pub fn item_shape_layer(&self, id: ItemId, index: usize) -> Option<usize> {
        let ctx = item_ctx!(self);
        Some(self.items.get(&id)?.shape_layer(index, &ctx))
    }

    pub fn item_tree_shape(&mut self, id: ItemId, tree: TreeId, index: usize) -> Option<TileShape> {
        let len = self.fill_tree_shapes(id, tree)?;
        if index >= len {
            self.items.get_mut(&id)?.clear_derived_data();
            if index >= self.fill_tree_shapes(id, tree)? {
                return None;
            }
        }
        self.items.get(&id)?.get_tree_shape(tree, index).cloned()
    }

    pub fn item_tile_shape(&mut self, id: ItemId, index: usize) -> Option<TileShape> {
        {
            let ctx = item_ctx!(self);
            let item = self.items.get(&id)?;
            match item {
                Item::ObstacleArea(i) => return i.get_tile_shape(index, &ctx),
                Item::ConductionArea(i) => return i.get_tile_shape(index, &ctx),
                Item::ViaObstacleArea(i) => return i.get_tile_shape(index, &ctx),
                Item::ComponentObstacleArea(i) => return i.get_tile_shape(index, &ctx),
                _ => {}
            }
        }
        let default_tree = self.default_tree_id();
        self.item_tree_shape(id, default_tree, index)
    }

    pub fn item_tree_shape_ref(
        &self,
        id: ItemId,
        tree: TreeId,
        index: usize,
    ) -> Option<Cow<'_, TileShape>> {
        let item = self.items.get(&id)?;
        let search_tree = self.trees.trees().find(|t| t.id() == tree)?;
        search_tree.get_tree_shape(item, index, &self.ctx())
    }

    pub fn item_tile_shape_ref(&self, id: ItemId, index: usize) -> Option<Cow<'_, TileShape>> {
        let ctx = self.ctx();
        let item = self.items.get(&id)?;
        match item {
            Item::ObstacleArea(i) => return i.get_tile_shape(index, &ctx).map(Cow::Owned),
            Item::ConductionArea(i) => return i.get_tile_shape(index, &ctx).map(Cow::Owned),
            Item::ViaObstacleArea(i) => return i.get_tile_shape(index, &ctx).map(Cow::Owned),
            Item::ComponentObstacleArea(i) => return i.get_tile_shape(index, &ctx).map(Cow::Owned),
            _ => {}
        }
        self.item_tree_shape_ref(id, self.default_tree_id(), index)
    }

    pub fn drill_item_tile_shape_on_layer_ref(
        &self,
        id: ItemId,
        layer: usize,
    ) -> Option<Cow<'_, TileShape>> {
        let ctx = self.ctx();
        let item = self.items.get(&id)?;
        if !item.is_drill_item() {
            return None;
        }
        let from_layer = item.first_layer(&ctx);
        if layer < from_layer || layer > item.last_layer(&ctx) {
            return None;
        }
        self.item_tile_shape_ref(id, layer - from_layer)
    }

    pub fn drill_item_tile_shape_on_layer(
        &mut self,
        id: ItemId,
        layer: usize,
    ) -> Option<TileShape> {
        let ctx = self.ctx();
        let item = self.items.get(&id)?;
        if !item.is_drill_item() {
            return None;
        }
        let from_layer = item.first_layer(&ctx);
        let to_layer = item.last_layer(&ctx);
        if layer < from_layer || layer > to_layer {
            return None;
        }
        self.item_tile_shape(id, layer - from_layer)
    }

    pub fn validate_item(&mut self, id: ItemId) -> bool {
        let ctx = self.ctx();
        let Some(item) = self.items.get(&id) else {
            return true;
        };
        let mut result = self.trees.validate_entries(item);
        let shape_count = item.tile_shape_count(&ctx);
        for i in 0..shape_count {
            match self.item_tile_shape(id, i) {
                Some(shape) if !shape.is_empty() => {}
                _ => result = false,
            }
        }
        if let Some(Item::Trace(trace)) = self.items.get(&id)
            && trace.first_corner() == trace.last_corner()
        {
            result = false;
        }
        result
    }

    // -- half widths, clearance, geometry --------------------------------------------------------

    pub fn get_max_trace_half_width(&self) -> i32 {
        self.max_trace_half_width
    }

    pub fn get_min_trace_half_width(&self) -> i32 {
        self.min_trace_half_width
    }

    pub fn clearance_value(&self, class1: usize, class2: usize, layer: usize) -> i32 {
        self.rules
            .clearance_matrix
            .get_value(class1, class2, layer, true)
    }

    pub fn get_trace_half_width(&self, net_number: i32, layer: usize) -> i32 {
        self.rules.get_trace_half_width(net_number, layer)
    }

    pub fn get_bounding_box(&self) -> IntBox {
        self.bounding_box
    }

    pub fn get_bounding_box_of_items(&self, ids: impl IntoIterator<Item = ItemId>) -> IntBox {
        let ctx = self.ctx();
        let mut result = IntBox::EMPTY;
        for id in ids {
            if let Some(item) = self.items.get(&id) {
                result = result.union(&item.bounding_box(&ctx));
            }
        }
        result
    }

    pub fn contains(&self, point: &Point) -> bool {
        point.is_contained_in(&self.bounding_box)
    }

    pub fn drill_center(&self, id: ItemId) -> Option<Point> {
        let ctx = self.ctx();
        match self.items.get(&id)? {
            Item::Via(via) => Some(via.get_center()),
            Item::Pin(pin) => Some(pin.get_center(&ctx)),
            _ => None,
        }
    }

    pub fn component_obstacle_area_is_front(&self, id: ItemId) -> bool {
        let Some(Item::ComponentObstacleArea(area)) = self.items.get(&id) else {
            return true;
        };
        let component_id = area.hdr.get_component_id();
        if component_id < 1 || component_id as usize > self.components.count() {
            return true;
        }
        self.components.get(component_id).placed_on_front()
    }

    pub fn item_component_name(&self, id: ItemId) -> Option<&str> {
        let item = self.items.get(&id)?;
        let component_id = item.component_id();
        if component_id <= 0 {
            return None;
        }
        Some(&self.components.get(component_id).name)
    }

    pub fn net_terminal_items(&self, net_number: i32) -> Vec<ItemId> {
        self.ids_where(|item| {
            item.as_connectable().is_some() && item.contains_net(net_number) && !item.is_routable()
        })
    }

    pub fn net_pins(&self, net_number: i32) -> Vec<ItemId> {
        self.ids_where(|item| matches!(item, Item::Pin(_)) && item.contains_net(net_number))
    }

    pub fn net_items(&self, net_number: i32) -> Vec<ItemId> {
        self.ids_where(|item| item.contains_net(net_number))
    }

    pub fn net_trace_length(&self, net_number: i32) -> f64 {
        self.get_connectable_items(net_number)
            .into_iter()
            .filter_map(|id| match self.items.get(&id) {
                Some(Item::Trace(trace)) => Some(trace.get_length()),
                _ => None,
            })
            .fold(0.0, |result, length| result + length)
    }

    pub fn net_via_count(&self, net_number: i32) -> usize {
        self.get_connectable_items(net_number)
            .into_iter()
            .filter(|id| matches!(self.items.get(id), Some(Item::Via(_))))
            .count()
    }

    pub fn has_ignored_nets(&self, id: ItemId) -> bool {
        let Some(item) = self.items.get(&id) else {
            return false;
        };
        item.net_nos().iter().any(|net_number| {
            let net = self.rules.nets.get(*net_number).expect(
                "Item.hasIgnoredNets: nets.get(netNumber) is null — Java throws a \
                 NullPointerException here too (Item.java:1244)",
            );
            self.rules
                .net_classes
                .get(net.get_net_class())
                .is_ignored_by_autorouter
        })
    }

    pub fn all_nets(&self, id: ItemId) -> Vec<i32> {
        let Some(item) = self.items.get(&id) else {
            return Vec::new();
        };
        item.net_nos()
            .iter()
            .filter(|net_number| self.rules.nets.get(**net_number).is_some())
            .copied()
            .collect()
    }

    pub fn all_net_names(&self, id: ItemId) -> String {
        let names: Vec<String> = self
            .all_nets(id)
            .into_iter()
            .filter_map(|net_number| self.rules.nets.get(net_number))
            .map(ToString::to_string)
            .collect();
        if names.is_empty() {
            return "no nets".to_string();
        }
        names.join(",")
    }

    // -- the changed area (RoutingBoardOperations, non-shove half) --------------------------------

    pub fn start_marking_changed_area(&mut self) {
        if self.changed_area.is_none() {
            self.changed_area = Some(ChangedArea::new(self.get_layer_count()));
        }
    }

    pub fn join_changed_area(&mut self, point: &copper_geometry::FloatPoint, layer: usize) {
        if let Some(changed_area) = &mut self.changed_area {
            changed_area.join(point, layer);
        }
    }

    pub fn mark_changed_area(&mut self, shape: &TileShape, layer: usize) {
        if let Some(changed_area) = &mut self.changed_area {
            changed_area.join_shape(shape, layer);
        }
    }

    pub fn mark_all_changed_area(&mut self) {
        self.start_marking_changed_area();
        let box_ = self.bounding_box;
        let corners = [
            copper_geometry::FloatPoint::new(f64::from(box_.ll.x), f64::from(box_.ll.y)),
            copper_geometry::FloatPoint::new(f64::from(box_.ur.x), f64::from(box_.ll.y)),
            copper_geometry::FloatPoint::new(f64::from(box_.ur.x), f64::from(box_.ur.y)),
            copper_geometry::FloatPoint::new(f64::from(box_.ll.x), f64::from(box_.ur.y)),
        ];
        for layer in 0..self.get_layer_count() {
            for corner in &corners {
                self.join_changed_area(corner, layer);
            }
        }
    }

    pub fn set_changed_area_layer_count(&mut self, layer_count: usize) {
        self.changed_area = Some(ChangedArea::new(layer_count));
    }

    pub fn remove_items_marking_changed_area(
        &mut self,
        ids: impl IntoIterator<Item = ItemId>,
    ) -> (bool, std::collections::BTreeSet<i32>) {
        let mut result = true;
        let mut changed_nets = std::collections::BTreeSet::new();
        self.start_marking_changed_area();
        for id in ids {
            let Some(item) = self.items.get(&id) else {
                continue;
            };
            if item.is_deletion_forbidden(&self.rules) || item.is_user_fixed() {
                result = false;
                continue;
            }
            let ctx = self.ctx();
            let shape_layers: Vec<usize> = (0..item.tile_shape_count(&ctx))
                .map(|i| item.shape_layer(i, &ctx))
                .collect();
            let net_nos = item.net_nos().to_vec();
            let shapes: Vec<(TileShape, usize)> = shape_layers
                .into_iter()
                .enumerate()
                .filter_map(|(i, layer)| self.item_tile_shape(id, i).map(|shape| (shape, layer)))
                .collect();
            for (shape, layer) in shapes {
                self.mark_changed_area(&shape, layer);
            }
            self.remove_item(id);
            changed_nets.extend(net_nos);
        }
        (result, changed_nets)
    }

    // -- the shove-failure fields (RoutingBoard) ---------------------------------------------------

    pub fn get_shove_failing_obstacle(&self) -> Option<ItemId> {
        self.shove_failing_obstacle
    }

    pub fn set_shove_failing_obstacle(&mut self, id: Option<ItemId>) {
        self.shove_failing_obstacle = id;
    }

    pub fn get_shove_failing_layer(&self) -> i32 {
        self.shove_failing_layer
    }

    pub fn set_shove_failing_layer(&mut self, layer: i32) {
        self.shove_failing_layer = layer;
    }

    pub fn clear_shove_failing_obstacle(&mut self) {
        self.shove_failing_obstacle = None;
        self.shove_failing_layer = -1;
    }

    pub fn clear_all_item_temporary_autoroute_data(&mut self) {
        for item in self.items.values_mut() {
            item.clear_autoroute_info();
        }
    }

    // -- private helpers ---------------------------------------------------------------------------

    /// The ids of every item matching `predicate`, in `board.itemList` order (descending id).
    fn ids_where(&self, predicate: impl Fn(&Item) -> bool) -> Vec<ItemId> {
        self.items
            .iter()
            .rev()
            .filter(|(_, item)| predicate(item))
            .map(|(id, _)| id)
            .collect()
    }
}

fn p7t8b_ids_ledger() -> bool {
    static ON: std::sync::LazyLock<bool> =
        std::sync::LazyLock::new(|| std::env::var_os("P7T8B_IDS").is_some());
    *ON
}

/// `P7T8B_IDS_FROM` — the lowest item id the `ID` ledger prints, so a run can be narrowed to one
/// connection's allocations without carrying a quarter of a million lines of board load.
fn p7t8b_ids_from() -> u32 {
    static FROM: std::sync::LazyLock<u32> = std::sync::LazyLock::new(|| {
        std::env::var("P7T8B_IDS_FROM")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    });
    *FROM
}

fn p7t8b_ids_backtrace() -> bool {
    static ON: std::sync::LazyLock<bool> =
        std::sync::LazyLock::new(|| std::env::var_os("P7T8B_IDS_BT").is_some());
    *ON
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Board>();
    }
}
