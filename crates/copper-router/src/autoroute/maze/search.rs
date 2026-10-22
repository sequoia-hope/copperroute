use std::collections::BTreeSet;

use copper_board::datastructures::StopCheck;
use copper_board::structure::AngleRestriction;
use copper_board::{Board, Item, ItemId, TreeId};
use copper_geometry::{FloatLine, FloatPoint, Point, SplitMix64};

use crate::arena::DoorId;
use crate::autoroute::expansion::{ExpandableRef, RoomRef};
use crate::autoroute::item_info;
use crate::autoroute::maze::queue::p7t14b_maze_ledger;
use crate::autoroute::maze::{
    AutorouteControl, AutorouteEngine, DestinationDistance, MazeAdjustment, MazeExpansionEngine,
    MazeListElement, MazeQueue,
};

pub const ALREADY_RIPPED_COSTS: i32 = 1;

#[derive(Debug)]
pub struct MazeSearchEngine<'a> {
    pub engine: &'a mut AutorouteEngine,

    pub ctrl: &'a AutorouteControl,

    pub queue: MazeQueue,

    pub destination_distance: DestinationDistance,

    coarse_field: Option<super::coarse_field::CoarseField>,

    pub search_tree: TreeId,

    pub random_generator: SplitMix64,

    destination_door: Option<ExpandableRef>,

    section_no_of_destination_door: i32,
}

impl<'a> MazeSearchEngine<'a> {
    pub fn new(
        engine: &'a mut AutorouteEngine,
        ctrl: &'a AutorouteControl,
    ) -> MazeSearchEngine<'a> {
        let mut random_generator = SplitMix64::new(0);
        random_generator.set_seed(i64::from(ctrl.ripup_costs));
        MazeSearchEngine {
            search_tree: engine.tree,
            engine,
            destination_distance: DestinationDistance::new(
                &ctrl.trace_costs,
                &ctrl.layer_active,
                ctrl.min_normal_via_cost,
                ctrl.min_cheap_via_cost,
            ),
            coarse_field: None,
            ctrl,
            queue: MazeQueue::new(),
            random_generator,
            destination_door: None,
            section_no_of_destination_door: 0,
        }
    }

    pub fn get_instance(
        start_items: &BTreeSet<ItemId>,
        destination_items: &BTreeSet<ItemId>,
        engine: &'a mut AutorouteEngine,
        board: &mut Board,
        ctrl: &'a AutorouteControl,
        stop: StopCheck<'_>,
    ) -> Option<MazeSearchEngine<'a>> {
        let mut new_instance = MazeSearchEngine::new(engine, ctrl);
        if new_instance.init(board, start_items, destination_items, stop) {
            Some(new_instance)
        } else {
            None
        }
    }

    /// An admissible estimate of the cost from `point` to the destination.
    pub fn heuristic(&self, point: &FloatPoint, layer: usize) -> f64 {
        let box_distance = self.destination_distance.calculate_from_point(point, layer);
        match self
            .coarse_field
            .as_ref()
            .and_then(|f| f.lower_bound(point, layer))
        {
            Some(coarse) => box_distance.max(coarse),
            None => box_distance,
        }
    }

    pub fn destination_door(&self) -> Option<ExpandableRef> {
        self.destination_door
    }

    pub fn section_no_of_destination_door(&self) -> i32 {
        self.section_no_of_destination_door
    }

    pub fn push(&mut self, element: MazeListElement, board: &Board) -> bool {
        self.queue.push(element, self.ctrl, self.engine, board)
    }

    pub fn init(
        &mut self,
        board: &mut Board,
        start_items: &BTreeSet<ItemId>,
        destination_items: &BTreeSet<ItemId>,
        stop: StopCheck<'_>,
    ) -> bool {
        MazeSearchEngine::reduce_trace_shapes_at_tie_pins(
            board,
            start_items,
            self.ctrl.net_number,
            self.search_tree,
        );
        MazeSearchEngine::reduce_trace_shapes_at_tie_pins(
            board,
            destination_items,
            self.ctrl.net_number,
            self.search_tree,
        );

        let mut destination_ok = false;
        let mut destination_shapes = Vec::new();
        for current_item in destination_items.iter().rev().copied() {
            if self.engine.is_stop_requested(stop) {
                return false;
            }
            item_info::set_start_info(board, current_item, false);
            let mut i = 0;
            while i < board.item_tree_shape_count(current_item, self.search_tree) {
                if let Some(current_tree_shape) =
                    board.item_tree_shape(current_item, self.search_tree, i)
                {
                    let layer = board.item_shape_layer(current_item, i).unwrap_or_else(|| {
                        panic!(
                            "MazeSearchEngine.init: item {current_item:?} has a tree shape but no \
                             layer — Java would have NPE'd at MazeSearchEngine.java:983"
                        )
                    });
                    self.destination_distance
                        .join(&current_tree_shape.bounding_box(), layer);
                    destination_shapes.push((current_tree_shape.bounding_box(), layer));
                }
                i += 1;
            }
            destination_ok = true;
        }

        if !destination_ok && self.ctrl.is_fanout {
            let board_bounding_box = board.bounding_box;
            self.destination_distance.join(&board_bounding_box, 0);
            self.destination_distance
                .join(&board_bounding_box, self.ctrl.layer_count - 1);
            destination_ok = true;
        }

        if !destination_ok {
            return false;
        }
        if super::coarse_field::enabled() && !self.ctrl.is_fanout {
            self.coarse_field = super::coarse_field::CoarseField::build(
                board,
                self.search_tree,
                self.ctrl,
                &destination_shapes,
            );
        }

        let mut start_rooms = Vec::new();
        for current_item in start_items.iter().rev().copied() {
            if self.engine.is_stop_requested(stop) {
                return false;
            }
            item_info::set_start_info(board, current_item, true);
            if board
                .items
                .get(&current_item)
                .is_none_or(|item| item.as_connectable().is_none())
            {
                continue;
            }
            let mut i = 0;
            while i < board.item_tree_shape_count(current_item, self.search_tree) {
                let contained_shape = {
                    let ctx = board.ctx();
                    board
                        .items
                        .get(&current_item)
                        .and_then(Item::as_connectable)
                        .and_then(|connectable| {
                            connectable.as_dyn().get_trace_connection_shape(
                                self.search_tree,
                                i,
                                &ctx,
                            )
                        })
                };
                let layer = board.item_shape_layer(current_item, i).unwrap_or_else(|| {
                    panic!(
                        "MazeSearchEngine.init: item {current_item:?} has no layer for shape {i} \
                         — Java would have NPE'd at MazeSearchEngine.java:1019"
                    )
                });
                let new_start_room =
                    self.engine
                        .add_incomplete_expansion_room(None, layer, contained_shape);
                start_rooms.push(new_start_room);
                i += 1;
            }
        }

        let mut completed_start_rooms = Vec::new();
        if self.engine.maintain_database {
            completed_start_rooms.extend(
                self.engine
                    .rooms_with_target_items(start_items)
                    .into_iter()
                    .rev(),
            );
        }

        for current_room in start_rooms {
            if self.engine.is_stop_requested(stop) {
                return false;
            }
            let current_completed_rooms = self
                .engine
                .complete_expansion_room_or_committed(board, current_room);
            completed_start_rooms.extend(current_completed_rooms);
        }

        let mut start_ok = false;
        for current_room in completed_start_rooms {
            let room_ref = RoomRef::Complete(current_room);
            let target_doors = self.engine.rooms.room_target_doors(room_ref).to_vec();
            for current_door in target_doors {
                if self.engine.is_stop_requested(stop) {
                    return false;
                }
                let door = self
                    .engine
                    .rooms
                    .target_door(current_door)
                    .expect("MazeSearchEngine.init: a target door its room still lists");
                let item = door.item;
                let tree_entry_no = door.tree_entry_no;
                if self
                    .engine
                    .rooms
                    .target_door(current_door)
                    .expect("just read")
                    .is_destination_door(board)
                {
                    continue;
                }
                let connection_shape = {
                    let ctx = board.ctx();
                    board
                        .items
                        .get(&item)
                        .and_then(Item::as_connectable)
                        .and_then(|connectable| {
                            connectable.as_dyn().get_trace_connection_shape(
                                self.search_tree,
                                tree_entry_no,
                                &ctx,
                            )
                        })
                        .unwrap_or_else(|| {
                            panic!(
                                "MazeSearchEngine.init: the target door's item {item:?} is not \
                                 connectable — Java's cast at MazeSearchEngine.java:1059 would \
                                 have thrown"
                            )
                        })
                };
                let room_shape = self
                    .engine
                    .rooms
                    .room_shape(room_ref)
                    .cloned()
                    .unwrap_or_else(|| {
                        panic!(
                            "MazeSearchEngine.init: a completed room with no shape — Java would \
                             have NPE'd at MazeSearchEngine.java:1061"
                        )
                    });
                let connection_shape = connection_shape.intersection(&room_shape);
                let current_center = connection_shape.centre_of_gravity();
                let shape_entry = FloatLine::new(current_center, current_center);
                let layer = self
                    .engine
                    .rooms
                    .complete_room(current_room)
                    .expect("just read")
                    .get_layer();
                let sorting_value = self.heuristic(&current_center, layer);
                if let Some(field) = self
                    .coarse_field
                    .as_ref()
                    .filter(|_| self.ctrl.layer_active[layer])
                {
                    super::coarse_field::record_start(
                        self.destination_distance
                            .calculate_from_point(&current_center, layer),
                        field.lower_bound(&current_center, layer),
                        self.ctrl.net_number,
                        &current_center,
                        layer,
                    );
                }
                let new_list_element = MazeListElement {
                    door: ExpandableRef::TargetDoor(current_door),
                    section_no_of_door: 0,
                    backtrack_door: None,
                    section_no_of_backtrack_door: 0,
                    expansion_value: 0.0,
                    sorting_value,
                    next_room: Some(room_ref),
                    shape_entry,
                    room_ripped: false,
                    adjustment: MazeAdjustment::None,
                    already_checked: false,
                    ripup_cost: 0,
                };
                if self.push(new_list_element, board) {
                    start_ok = true;
                }
            }
        }
        start_ok
    }

    pub fn find_connection(
        &mut self,
        board: &mut Board,
        stop: StopCheck<'_>,
    ) -> Option<MazeResult> {
        while self.occupy_next_element(board, stop) {}
        let destination_door = self.destination_door?;
        Some(MazeResult {
            destination_door,
            section_no_of_door: self.section_no_of_destination_door,
        })
    }

    pub fn occupy_next_element(&mut self, board: &mut Board, stop: StopCheck<'_>) -> bool {
        if self.destination_door.is_some() {
            return false;
        }
        let mut list_element: Option<MazeListElement> = None;
        while !self.queue.is_empty() {
            if self.engine.is_stop_requested(stop) {
                return false;
            }
            let popped = self
                .queue
                .pop_first()
                .expect("MazeQueue::pop_first on a non-empty queue");
            let is_occupied = self
                .engine
                .maze_search_element(popped.door, popped.section_no_of_door)
                .unwrap_or_else(|| {
                    panic!(
                        "MazeSearchEngine.occupyNextElement: no maze search element for section \
                         {} of {:?} — Java throws at MazeSearchEngine.java:332",
                        popped.section_no_of_door, popped.door
                    )
                })
                .is_occupied;
            if p7t14b_maze_ledger() {
                eprintln!(
                    "POPQ occ={is_occupied} sec={} sort={:.6} exp={:.6} adj={:?}",
                    popped.section_no_of_door,
                    popped.sorting_value,
                    popped.expansion_value,
                    popped.adjustment
                );
            }
            if !is_occupied {
                crate::visualization::capture_maze_step(
                    board,
                    self.engine,
                    &popped,
                    self.queue.len(),
                );
                list_element = Some(popped);
                break;
            }
        }
        if p7t14b_maze_ledger() {
            match &list_element {
                None => eprintln!("OCC found=false"),
                Some(element) => {
                    let room = match element.next_room {
                        None => "null".to_string(),
                        Some(RoomRef::Obstacle(id)) => {
                            self.engine.rooms.obstacle_room(id).map_or_else(
                                || "obst?".to_string(),
                                |room| {
                                    format!(
                                        "obst{}:{}",
                                        room.get_item().0,
                                        room.get_index_in_item()
                                    )
                                },
                            )
                        }
                        Some(_) => "free".to_string(),
                    };
                    eprintln!(
                        "OCC found=true sec={} exp={:.6} sort={:.6} adj={:?} chk={} rip={} \
                         cost={} room={room}",
                        element.section_no_of_door,
                        element.expansion_value,
                        element.sorting_value,
                        element.adjustment,
                        element.already_checked,
                        element.room_ripped,
                        element.ripup_cost
                    );
                }
            }
        }
        let Some(list_element) = list_element else {
            return false;
        };

        {
            let section = self
                .engine
                .maze_search_element_mut(list_element.door, list_element.section_no_of_door)
                .expect("just resolved above");
            section.backtrack_door = list_element.backtrack_door;
            section.section_no_of_backtrack_door = list_element.section_no_of_backtrack_door;
            section.room_ripped = list_element.room_ripped;
            section.ripup_cost = list_element.ripup_cost;
            section.adjustment = list_element.adjustment;
        }

        if matches!(list_element.door, ExpandableRef::Page(_)) {
            MazeExpansionEngine::expand_to_drills_of_page(self, board, &list_element, stop);
            return true;
        }

        if let ExpandableRef::TargetDoor(current_door) = list_element.door {
            let is_destination = self
                .engine
                .rooms
                .target_door(current_door)
                .expect("just resolved above")
                .is_destination_door(board);
            if is_destination {
                self.destination_door = Some(list_element.door);
                self.section_no_of_destination_door = list_element.section_no_of_door;
                return false;
            }
        }

        if self.ctrl.is_fanout
            && matches!(list_element.door, ExpandableRef::Drill(_))
            && matches!(list_element.backtrack_door, Some(ExpandableRef::Drill(_)))
        {
            self.destination_door = Some(list_element.door);
            self.section_no_of_destination_door = list_element.section_no_of_door;
            return false;
        }

        if self.ctrl.vias_allowed
            && matches!(list_element.door, ExpandableRef::Drill(_))
            && !matches!(list_element.backtrack_door, Some(ExpandableRef::Drill(_)))
        {
            MazeExpansionEngine::expand_to_other_layers(self, board, &list_element);
        }

        if list_element.next_room.is_some() && !self.expand_to_room_doors(board, &list_element) {
            return true;
        }

        self.engine
            .maze_search_element_mut(list_element.door, list_element.section_no_of_door)
            .expect("just resolved above")
            .is_occupied = true;
        true
    }

    pub fn door_is_small(&self, board: &Board, door: DoorId, trace_width: f64) -> bool {
        let Some(current_door) = self.engine.rooms.door(door) else {
            return false;
        };
        let both_free_space = matches!(current_door.first_room, RoomRef::Complete(_))
            && matches!(current_door.second_room, RoomRef::Complete(_));
        if current_door.dimension != 1 && !both_free_space {
            return false;
        }
        let Some(door_shape) = self.engine.rooms.door_shape(door) else {
            return true;
        };
        if door_shape.is_empty() {
            return true;
        }
        let door_length = match board.rules.trace_angle_restriction {
            AngleRestriction::NinetyDegree => door_shape.bounding_box().max_width(),
            AngleRestriction::FortyFiveDegree => door_shape
                .bounding_octagon()
                .unwrap_or_else(|| {
                    panic!(
                        "MazeSearchEngine.doorIsSmall: a non-empty door shape with no bounding \
                             octagon — Java would have NPE'd at MazeSearchEngine.java:780"
                    )
                })
                .max_width(),
            AngleRestriction::None => {
                let door_line_segment = door_shape.diagonal_corner_segment().unwrap_or_else(|| {
                    panic!(
                        "MazeSearchEngine.doorIsSmall: a non-empty door shape with no diagonal \
                         corner segment — Java would have NPE'd at MazeSearchEngine.java:783"
                    )
                });
                door_line_segment.b.distance(&door_line_segment.a)
            }
        };
        door_length < trace_width
    }

    pub fn reduce_trace_shapes_at_tie_pins(
        board: &mut Board,
        item_list: &BTreeSet<ItemId>,
        own_net_no: i32,
        autoroute_tree: TreeId,
    ) {
        for current_item in item_list.iter().rev().copied() {
            let is_tie_pin = board
                .items
                .get(&current_item)
                .is_some_and(|item| matches!(item, Item::Pin(_)) && item.net_count() > 1);
            if !is_tie_pin {
                continue;
            }
            let pin_contacts = board.normal_contacts(current_item);
            for current_contact in pin_contacts.into_iter().rev() {
                let is_foreign_trace = board.items.get(&current_contact).is_some_and(|item| {
                    matches!(item, Item::Trace(_)) && !item.contains_net(own_net_no)
                });
                if !is_foreign_trace {
                    continue;
                }
                board.item_tree_shape_count(current_item, autoroute_tree);
                board.item_tree_shape_count(current_contact, autoroute_tree);
                let Some(Item::Pin(tie_pin)) = board.items.get(&current_item).cloned() else {
                    continue;
                };
                let Some(Item::Trace(mut trace)) = board.items.remove(&current_contact) else {
                    continue;
                };
                let mut trees = std::mem::take(&mut board.trees);
                {
                    let ctx = board.ctx();
                    trees
                        .trees_mut()
                        .find(|tree| tree.id() == autoroute_tree)
                        .unwrap_or_else(|| {
                            panic!(
                                "MazeSearchEngine.reduceTraceShapesAtTiePins: no search tree with \
                                 id {autoroute_tree:?}"
                            )
                        })
                        .reduce_trace_shape_at_tie_pin(&tie_pin, &mut trace, &ctx);
                }
                board.trees = trees;
                board.items.insert(current_contact, Item::Trace(trace));
                crate::autoroute::instrument::note_mutation(
                    crate::autoroute::instrument::Mutation::TiePinReduction,
                );
            }
        }
    }
}

pub fn segment_projection(from_segment: &FloatLine, to_segment: &FloatLine) -> Option<FloatLine> {
    let check_segment = from_segment.adjust_direction(to_segment);
    let first_projection = to_segment.segment_projection(&check_segment);
    let second_projection = to_segment.segment_projection_2(&check_segment);
    match (first_projection, second_projection) {
        (Some(first_projection), Some(second_projection)) => {
            let result_a =
                if first_projection.a == to_segment.a || second_projection.a == to_segment.a {
                    to_segment.a
                } else if first_projection.a.distance_square(&to_segment.a)
                    <= second_projection.a.distance_square(&to_segment.a)
                {
                    first_projection.a
                } else {
                    second_projection.a
                };
            let result_b =
                if first_projection.b == to_segment.b || second_projection.b == to_segment.b {
                    to_segment.b
                } else if first_projection.b.distance_square(&to_segment.b)
                    <= second_projection.b.distance_square(&to_segment.b)
                {
                    first_projection.b
                } else {
                    second_projection.b
                };
            Some(FloatLine::new(result_a, result_b))
        }
        (Some(first_projection), None) => Some(first_projection),
        (None, second_projection) => second_projection,
    }
}

pub fn to_impacted_points(shape_entry: Option<&FloatLine>) -> Option<[Point; 2]> {
    let shape_entry = shape_entry?;
    Some([
        Point::from(shape_entry.a.round()),
        Point::from(shape_entry.b.round()),
    ])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MazeResult {
    pub destination_door: ExpandableRef,
    pub section_no_of_door: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShoveResult {
    pub opposite_door: DoorId,
    pub side_doors: Vec<DoorId>,
    pub from_door_passing_point: FloatPoint,
    pub opposite_door_passing_point: FloatPoint,
}

impl ShoveResult {
    pub fn new(
        opposite_door: DoorId,
        side_doors: Vec<DoorId>,
        from_door_passing_point: FloatPoint,
        opposite_door_passing_point: FloatPoint,
    ) -> ShoveResult {
        ShoveResult {
            opposite_door,
            side_doors,
            from_door_passing_point,
            opposite_door_passing_point,
        }
    }
}
