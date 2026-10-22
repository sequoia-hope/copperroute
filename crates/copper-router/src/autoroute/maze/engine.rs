use std::collections::{BTreeMap, BTreeSet};
use std::panic::AssertUnwindSafe;

use copper_board::ids::TreeObject;
use copper_board::structure::Unit;
use copper_board::{
    Board, Item, ItemId, RoomId, ShapeSearchTree, StopCheck, StopConnectionOption, TimeLimit,
    TreeId,
};
use copper_geometry::{Simplex, TileShape};
use copper_settings::{ExpansionCostFactor, RouterSettings};

use crate::Arena;
use crate::arena::{DoorId, DrillId, IncompleteRoomId, PageId};
use crate::autoroute::attempt::{AutorouteAttemptResult, AutorouteAttemptState};
use crate::autoroute::drill::DrillPageArray;
use crate::autoroute::expansion::sorted_neighbours::SortedRoomNeighbours;
use crate::autoroute::expansion::{
    ExpandableRef, ExpansionRoomStore, IncompleteFreeSpaceExpansionRoom, RoomRef,
};
use crate::autoroute::item_info;
use crate::autoroute::maze::{
    AutorouteControl, MazeResult, MazeSearchElement, MazeSearchEngine, ViaPricing,
};
use crate::autoroute::path::{Connection, FoundConnectionInserter, FoundConnectionLocator};
use crate::autoroute::tree_ext::{AutorouteSearchTreeExt, p7t14b_cs_ledger};
use crate::board_ext::RoutingBoardExt;
use crate::error::RouterError;
use crate::pipeline::connection_budget::ConnectionBudget;
use crate::pipeline::{BatchAutorouter, RouterBudget};

#[derive(Debug)]
pub struct AutorouteEngine {
    pub rooms: ExpansionRoomStore,

    pub tree: TreeId,

    pub maintain_database: bool,

    pub max_drill_page_width: i32,

    drill_page_array: DrillPageArray,

    complete_expansion_rooms: Vec<RoomId>,

    net_number: i32,

    time_limit: Option<TimeLimit>,

    pub connections: Arena<Connection>,
}

impl AutorouteEngine {
    pub fn new(
        board: &mut Board,
        trace_clearance_class_index: usize,
        maintain_database: bool,
    ) -> AutorouteEngine {
        let tree = {
            let mut items = std::mem::take(&mut board.items);
            let mut trees = std::mem::take(&mut board.trees);
            let id = {
                let ctx = board.ctx();
                let mut refs: Vec<&mut copper_board::Item> = items.ordered_values_mut();
                refs.reverse();
                trees
                    .get_autoroute_tree(trace_clearance_class_index, &mut refs, &ctx)
                    .id()
            };
            board.items = items;
            board.trees = trees;
            id
        };

        let default_via_diameter = board
            .rules
            .get_default_via_diameter(&board.library.padstacks);
        let max_drill_page_width = ((5.0 * default_via_diameter) as i32).max(10_000);

        let mut rooms = ExpansionRoomStore::new();
        let drill_page_array = DrillPageArray::new(board, max_drill_page_width, &mut rooms);

        AutorouteEngine {
            rooms,
            complete_expansion_rooms: Vec::new(),
            connections: Arena::new(),
            tree,
            maintain_database,
            max_drill_page_width,
            drill_page_array,
            net_number: -1,
            time_limit: None,
        }
    }

    pub fn init_connection(
        &mut self,
        board: &mut Board,
        net_number: i32,
        time_limit: Option<TimeLimit>,
    ) {
        if self.maintain_database && net_number != self.net_number {
            let rooms_to_remove: Vec<RoomId> = self
                .complete_expansion_rooms
                .iter()
                .copied()
                .filter(|room| {
                    self.rooms
                        .complete_room(*room)
                        .is_some_and(|r| r.is_net_dependent())
                })
                .collect();
            for room in rooms_to_remove {
                self.remove_complete_expansion_room(board, room);
            }

            let items: Vec<ItemId> = board
                .items_in_board_order()
                .into_iter()
                .filter(|id| {
                    board
                        .get_item(*id)
                        .is_some_and(|item| item.contains_net(net_number))
                })
                .collect();
            for item in items {
                board.additional_update_after_change(self, item);
            }
        }
        self.net_number = net_number;
        self.time_limit = time_limit;
    }

    pub fn time_limit(&self) -> Option<TimeLimit> {
        self.time_limit
    }

    pub fn complete_expansion_rooms(&self) -> &[RoomId] {
        &self.complete_expansion_rooms
    }

    pub fn get_net_number(&self) -> i32 {
        self.net_number
    }

    pub fn is_stop_requested(&self, stop: StopCheck<'_>) -> bool {
        if let Some(time_limit) = &self.time_limit
            && time_limit.is_exceeded()
        {
            return true;
        }
        stop()
    }

    pub fn clear(&mut self, board: &mut Board) {
        {
            let tree = tree_mut(board, self.tree);
            self.rooms.clear(tree);
        }
        self.complete_expansion_rooms.clear();
        board.clear_all_item_temporary_autoroute_data();
    }

    pub fn add_incomplete_expansion_room(
        &mut self,
        shape: Option<TileShape>,
        layer: usize,
        contained_shape: Option<TileShape>,
    ) -> IncompleteRoomId {
        self.rooms
            .new_incomplete_room(shape, layer, contained_shape)
    }

    pub fn get_first_incomplete_expansion_room(&self) -> Option<IncompleteRoomId> {
        self.rooms
            .incomplete_rooms
            .iter()
            .next()
            .map(|(index, _)| IncompleteRoomId(index))
    }

    pub fn remove_incomplete_expansion_room(&mut self, room: IncompleteRoomId) {
        self.rooms.remove_incomplete_expansion_room(room);
    }

    pub fn remove_all_doors(&mut self, room: RoomRef) {
        self.rooms.remove_all_doors(room);
    }

    pub fn invalidate_drill_pages(&mut self, shape: &TileShape) {
        self.drill_page_array
            .invalidate(shape, &mut self.rooms.drills);
    }

    pub fn drill_pages(&self) -> &DrillPageArray {
        &self.drill_page_array
    }

    pub fn drill_pages_mut(&mut self) -> &mut DrillPageArray {
        &mut self.drill_page_array
    }

    pub fn drill_page_drills(
        &mut self,
        board: &mut Board,
        page: PageId,
        attach_smd: bool,
        stop: StopCheck<'_>,
    ) -> std::sync::Arc<Vec<DrillId>> {
        let (i, j) = self.drill_page_array.split(page);
        let mut pages = self.drill_page_array.take_pages();
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
            pages[j][i].get_drills(self, board, attach_smd, stop)
        }));
        self.drill_page_array.restore_pages(pages);
        match outcome {
            Ok(drills) => drills,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    pub fn generate_room_id_no(&mut self) -> i32 {
        self.rooms.next_room_id_no()
    }

    pub fn expandable_id_no(&self, object: ExpandableRef) -> i32 {
        match object {
            ExpandableRef::Door(door) => self
                .rooms
                .door_id_no(door)
                .expect("ExpansionDoor.getId: a live door with two live rooms"),
            ExpandableRef::TargetDoor(door) => self
                .rooms
                .target_door_id_no(door)
                .expect("TargetItemExpansionDoor.getId: a live target door"),
            ExpandableRef::Drill(drill) => self
                .rooms
                .drills
                .get(drill.0)
                .expect("ExpansionDrill.getId: a live drill")
                .get_id(),
            ExpandableRef::Page(page) => self.drill_page_array.page(page).get_id(),
        }
    }

    pub fn expandable_shape(&self, object: ExpandableRef) -> Option<TileShape> {
        match object {
            ExpandableRef::Door(door) => self.rooms.door_shape(door),
            ExpandableRef::TargetDoor(door) => {
                Some(self.rooms.target_door(door)?.get_shape().clone())
            }
            ExpandableRef::Drill(drill) => {
                Some(self.rooms.drills.get(drill.0)?.get_shape().clone())
            }
            ExpandableRef::Page(page) => {
                Some(TileShape::Box(self.drill_page_array.page(page).shape))
            }
        }
    }

    pub fn expandable_dimension(&self, object: ExpandableRef) -> i32 {
        match object {
            ExpandableRef::Door(door) => self.rooms.door(door).map_or(0, |door| door.dimension),
            ExpandableRef::TargetDoor(_) | ExpandableRef::Drill(_) | ExpandableRef::Page(_) => 2,
        }
    }

    pub fn maze_search_element(
        &self,
        object: ExpandableRef,
        section: i32,
    ) -> Option<&MazeSearchElement> {
        let index = usize::try_from(section).ok()?;
        match object {
            ExpandableRef::Door(door) => self.rooms.door(door)?.get_maze_search_element(index),
            ExpandableRef::TargetDoor(door) => {
                Some(self.rooms.target_door(door)?.get_maze_search_element(index))
            }
            ExpandableRef::Drill(drill) => {
                let drill = self.rooms.drills.get(drill.0)?;
                (index < drill.maze_search_element_count())
                    .then(|| drill.get_maze_search_element(index))
            }
            ExpandableRef::Page(page) => {
                let page = self.drill_page_array.page(page);
                (index < page.maze_search_element_count())
                    .then(|| page.get_maze_search_element(index))
            }
        }
    }

    pub fn expandable_other_room(&self, object: ExpandableRef, room: RoomRef) -> Option<RoomRef> {
        match object {
            ExpandableRef::Door(door) => self.rooms.door(door)?.other_complete_room(room),
            ExpandableRef::TargetDoor(door) => self.rooms.target_door(door)?.other_room(room),
            ExpandableRef::Drill(drill) => self.rooms.drills.get(drill.0)?.other_room(room),
            ExpandableRef::Page(page) => self.drill_page_array.page(page).other_room(room),
        }
    }

    pub fn maze_search_element_count(&self, object: ExpandableRef) -> Option<usize> {
        match object {
            ExpandableRef::Door(door) => self.rooms.door(door)?.maze_search_element_count(),
            ExpandableRef::TargetDoor(door) => {
                Some(self.rooms.target_door(door)?.maze_search_element_count())
            }
            ExpandableRef::Drill(drill) => {
                Some(self.rooms.drills.get(drill.0)?.maze_search_element_count())
            }
            ExpandableRef::Page(page) => {
                Some(self.drill_page_array.page(page).maze_search_element_count())
            }
        }
    }

    pub fn maze_search_element_mut(
        &mut self,
        object: ExpandableRef,
        section: i32,
    ) -> Option<&mut MazeSearchElement> {
        let index = usize::try_from(section).ok()?;
        match object {
            ExpandableRef::Door(door) => self
                .rooms
                .door_mut(door)?
                .get_maze_search_element_mut(index),
            ExpandableRef::TargetDoor(door) => Some(
                self.rooms
                    .target_door_mut(door)?
                    .get_maze_search_element_mut(index),
            ),
            ExpandableRef::Drill(drill) => {
                let drill = self.rooms.drills.get_mut(drill.0)?;
                if index >= drill.maze_search_element_count() {
                    return None;
                }
                Some(drill.get_maze_search_element_mut(index))
            }
            ExpandableRef::Page(page) => {
                let page = self.drill_page_array.page_mut(page);
                if index >= page.maze_search_element_count() {
                    return None;
                }
                Some(page.get_maze_search_element_mut(index))
            }
        }
    }

    pub fn remove_complete_expansion_room(&mut self, board: &mut Board, room: RoomId) -> bool {
        let room_ref = RoomRef::Complete(room);
        let Some(room_shape) = self.rooms.room_shape(room_ref).cloned() else {
            return false;
        };
        let room_layer = match self.rooms.complete_room(room) {
            Some(r) => r.get_layer(),
            None => return false,
        };

        let room_doors: Vec<DoorId> = self.rooms.room_doors(room_ref).to_vec();
        for current_door in room_doors {
            let Some(current_neighbour) = self
                .rooms
                .door(current_door)
                .and_then(|d| d.other_room(room_ref))
            else {
                continue;
            };
            self.rooms
                .remove_door(current_neighbour, ExpandableRef::Door(current_door));
            let Some(neighbour_shape) = self.rooms.room_shape(current_neighbour).cloned() else {
                continue;
            };
            let intersection = room_shape.intersection(&neighbour_shape);
            if intersection.dimension() != 1 {
                continue;
            }
            let Some(touching_sides) = room_shape.touching_sides(&neighbour_shape) else {
                continue;
            };
            let border_line = neighbour_shape
                .border_line(touching_sides[1])
                .unwrap_or_else(|| {
                    panic!(
                        "AutorouteEngine.removeCompleteExpansionRoom: the neighbour has no border \
                         line {} — Java NPEs at AutorouteEngine.java:394",
                        touching_sides[1]
                    )
                })
                .opposite();
            let new_incomplete_room_shape =
                TileShape::Simplex(Simplex::from_lines(vec![border_line]));
            let new_incomplete_room = self.add_incomplete_expansion_room(
                Some(new_incomplete_room_shape),
                room_layer,
                Some(intersection),
            );
            let new_door = self.rooms.new_door(
                current_neighbour,
                RoomRef::Incomplete(new_incomplete_room),
                1,
            );
            self.rooms.add_door(current_neighbour, new_door);
            self.rooms
                .add_door(RoomRef::Incomplete(new_incomplete_room), new_door);
        }

        self.remove_all_doors(room_ref);
        let removed = {
            let tree = tree_mut(board, self.tree);
            self.rooms.remove_complete_room(tree, room)
        };
        self.complete_expansion_rooms.retain(|r| *r != room);
        self.invalidate_drill_pages(&room_shape);
        removed
    }

    pub fn complete_expansion_room(
        &mut self,
        board: &mut Board,
        room: IncompleteRoomId,
    ) -> Result<Vec<RoomId>, RouterError> {
        let mut result: Vec<RoomId> = Vec::new();
        match std::panic::catch_unwind(AssertUnwindSafe(|| {
            self.complete_expansion_room_inner(board, room, &mut result)
        })) {
            Ok(()) => Ok(result),
            Err(payload) => {
                let message = panic_message(&payload);
                if result.is_empty() {
                    Err(RouterError::Panicked(message))
                } else {
                    Err(RouterError::PanickedWithRooms {
                        message,
                        rooms: result,
                    })
                }
            }
        }
    }

    pub fn complete_expansion_room_or_committed(
        &mut self,
        board: &mut Board,
        room: IncompleteRoomId,
    ) -> Vec<RoomId> {
        match self.complete_expansion_room(board, room) {
            Ok(rooms) => rooms,
            Err(RouterError::PanickedWithRooms { rooms, .. }) => rooms,
            Err(_) => Vec::new(),
        }
    }

    fn complete_expansion_room_inner(
        &mut self,
        board: &mut Board,
        room: IncompleteRoomId,
        result: &mut Vec<RoomId>,
    ) {
        let room_ref = RoomRef::Incomplete(room);

        let mut from_door_shape: Option<TileShape> = None;
        let mut ignore_object: Option<TreeObject> = None;
        for current_door in self.rooms.room_doors(room_ref).to_vec() {
            let Some(door) = self.rooms.door(current_door) else {
                continue;
            };
            let other_room = door.other_room(room_ref);
            let dimension = door.get_dimension();
            if let Some(RoomRef::Complete(free_room)) = other_room
                && dimension == 2
            {
                from_door_shape = self.rooms.door_shape(current_door);
                ignore_object = Some(TreeObject::Room(free_room));
                break;
            }
        }

        let completed_shapes = self.complete_shape(board, room, ignore_object, &from_door_shape);
        if p7t14b_cs_ledger() {
            let incomplete = self.rooms.incomplete_room(room);
            let mut line = format!(
                "CSHAPE in={} contained={} fromDoor={} ignore={} out={}",
                p7t14b_shape(incomplete.and_then(|room| room.get_shape())),
                p7t14b_shape(incomplete.and_then(|room| room.get_contained_shape())),
                p7t14b_shape(from_door_shape.as_ref()),
                match ignore_object {
                    None => "null",
                    Some(TreeObject::Room(_)) => "CompleteFreeSpaceExpansionRoom",
                    Some(TreeObject::Item(_)) => "Item",
                },
                completed_shapes.len()
            );
            for candidate in &completed_shapes {
                line.push(' ');
                line.push_str(&p7t14b_shape(candidate.get_shape()));
            }
            eprintln!("{line}");
        }

        self.remove_incomplete_expansion_room(room);

        let mut is_first_completed_room = true;
        for current_incomplete_room in completed_shapes {
            let dimension = current_incomplete_room
                .get_shape()
                .unwrap_or_else(|| {
                    panic!(
                        "AutorouteEngine.completeExpansionRoom: a completed shape with no shape — \
                         Java NPEs at AutorouteEngine.java:472"
                    )
                })
                .dimension();
            if dimension != 2 {
                continue;
            }
            if is_first_completed_room {
                is_first_completed_room = false;
                if let Some(completed_room) = self.add_complete_room(board, current_incomplete_room)
                {
                    result.push(completed_room);
                }
            } else {
                let recalculated = {
                    let tmp = self
                        .rooms
                        .incomplete_rooms
                        .insert(current_incomplete_room.clone());
                    let out = self.complete_shape(
                        board,
                        IncompleteRoomId(tmp),
                        ignore_object,
                        &from_door_shape,
                    );
                    self.rooms.incomplete_rooms.remove(tmp);
                    out
                };
                for tmp_room in recalculated {
                    if let Some(completed_room) = self.add_complete_room(board, tmp_room) {
                        result.push(completed_room);
                    }
                }
            }
        }
    }

    fn complete_shape(
        &self,
        board: &Board,
        room: IncompleteRoomId,
        ignore_object: Option<TreeObject>,
        from_door_shape: &Option<TileShape>,
    ) -> Vec<IncompleteFreeSpaceExpansionRoom> {
        let incomplete = self.rooms.incomplete_room(room).unwrap_or_else(|| {
            panic!(
                "AutorouteEngine.completeExpansionRoom: incomplete room {room:?} is not in the \
                 arena — Java holds a live reference and would dereference it at \
                 AutorouteEngine.java:425"
            )
        });
        let ctx = board.ctx();
        tree_of(board, self.tree).complete_shape(
            incomplete,
            self.net_number,
            ignore_object,
            from_door_shape.as_ref(),
            &board.items,
            &self.rooms,
            &ctx,
        )
    }

    fn add_complete_room(
        &mut self,
        board: &mut Board,
        room: IncompleteFreeSpaceExpansionRoom,
    ) -> Option<RoomId> {
        let temporary = self.rooms.incomplete_rooms.insert(room);
        let completed_room =
            self.calculate_doors(board, RoomRef::Incomplete(IncompleteRoomId(temporary)));
        self.rooms.incomplete_rooms.remove(temporary);

        let completed_room = completed_room?;
        let abandon = |engine: &mut Self| {
            engine.rooms.detach_all_doors(completed_room);
            if let Some(id_no) = engine.rooms.room_id_no(completed_room) {
                engine.rooms.release_room_id_no(id_no);
            }
            None
        };
        let RoomRef::Complete(completed_room_id) = completed_room else {
            return abandon(self);
        };
        let dimension = self
            .rooms
            .complete_room(completed_room_id)
            .and_then(|r| r.get_shape())
            .map(TileShape::dimension);
        let Some(dimension) = dimension else {
            return abandon(self);
        };
        if dimension != 2 {
            return abandon(self);
        }
        let completed_room = completed_room_id;

        self.complete_expansion_rooms.push(completed_room);
        {
            let tree = tree_mut(board, self.tree);
            self.rooms.insert_complete_room(tree, completed_room);
        }
        if p7t14b_cs_ledger() {
            let room = self.rooms.complete_room(completed_room);
            let layer = room.map_or(usize::MAX, |room| room.get_layer());
            let bounds = room.and_then(|room| room.get_shape()).map_or_else(
                || "null".to_string(),
                |shape| {
                    let b = shape.bounding_box();
                    format!("[({},{})..({},{})]", b.ll.x, b.ll.y, b.ur.x, b.ur.y)
                },
            );
            let doors = self
                .rooms
                .room_doors(RoomRef::Complete(completed_room))
                .len();
            eprintln!("CROOM8 layer={layer} bb={bounds} doors={doors}");
        }
        Some(completed_room)
    }

    fn calculate_doors(&mut self, board: &mut Board, room: RoomRef) -> Option<RoomRef> {
        SortedRoomNeighbours::complete(room, self.net_number, board, &mut self.rooms, self.tree)
    }

    pub fn complete_neighbour_rooms(&mut self, board: &mut Board, room: RoomRef) {
        let mut index = 0usize;
        loop {
            let doors = self.rooms.room_doors(room).to_vec();
            let Some(current_door) = doors.get(index).copied() else {
                return;
            };
            index += 1;

            let Some(neighbour_room) = self
                .rooms
                .door(current_door)
                .and_then(|d| d.other_room(room))
            else {
                continue;
            };
            match neighbour_room {
                RoomRef::Incomplete(free_room) => {
                    let _ = self.complete_expansion_room(board, free_room);
                    index = 0;
                }
                RoomRef::Obstacle(obstacle_neighbour_room) => {
                    let all_doors_calculated = self
                        .rooms
                        .obstacle_room(obstacle_neighbour_room)
                        .is_some_and(|r| r.all_doors_calculated());
                    if !all_doors_calculated {
                        self.calculate_doors(board, neighbour_room);
                        if let Some(r) = self.rooms.obstacle_room_mut(obstacle_neighbour_room) {
                            r.set_doors_calculated(true);
                        }
                    }
                }
                RoomRef::Complete(_) => {}
            }
        }
    }

    pub fn rooms_with_target_items(&self, items: &BTreeSet<ItemId>) -> BTreeSet<RoomId> {
        let mut result = BTreeSet::new();
        for current_room in &self.complete_expansion_rooms {
            let Some(room) = self.rooms.complete_room(*current_room) else {
                continue;
            };
            for current_target_door in room.get_target_doors() {
                let Some(door) = self.rooms.target_door(*current_target_door) else {
                    continue;
                };
                if items.contains(&door.item) {
                    result.insert(*current_room);
                }
            }
        }
        result
    }

    pub fn validate(&self, board: &Board) -> bool {
        let mut result = true;
        for current_room in &self.complete_expansion_rooms {
            let Some(room) = self.rooms.complete_room(*current_room) else {
                continue;
            };
            if !room.validate(self, board, *current_room) {
                result = false;
            }
        }
        result
    }

    pub fn reset_all_doors(&mut self, board: &mut Board) {
        let complete = self.complete_expansion_rooms.clone();
        for room in complete {
            self.rooms.reset_doors(RoomRef::Complete(room));
        }

        for item in board.items_in_board_order() {
            if board
                .get_item(item)
                .and_then(|i| i.get_autoroute_info_pur())
                .is_none()
            {
                continue;
            }
            let rooms = &mut self.rooms;
            item_info::reset_doors(board, item, |_, obstacle_room| {
                rooms.reset_doors(RoomRef::Obstacle(obstacle_room));
            });
            if let Some(info) = board
                .get_item_mut(item)
                .and_then(|i| i.get_autoroute_info_pur_mut())
            {
                info.precalculated_connection = None;
            }
        }

        self.drill_page_array.reset(&mut self.rooms.drills);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn autoroute_connection(
        &mut self,
        board: &mut Board,
        start: &BTreeSet<ItemId>,
        dest: &BTreeSet<ItemId>,
        ctrl: &AutorouteControl,
        ripped: &mut BTreeSet<ItemId>,
        ripup_costs: Option<&mut BTreeMap<ItemId, i32>>,
        stop: StopCheck<'_>,
    ) -> AutorouteAttemptResult {
        self.autoroute_connection_impl(board, start, dest, ctrl, ripped, ripup_costs, stop, false)
    }

    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn autoroute_connection_with_forced_locator_failure(
        &mut self,
        board: &mut Board,
        start: &BTreeSet<ItemId>,
        dest: &BTreeSet<ItemId>,
        ctrl: &AutorouteControl,
        ripped: &mut BTreeSet<ItemId>,
        ripup_costs: Option<&mut BTreeMap<ItemId, i32>>,
        stop: StopCheck<'_>,
        force: bool,
    ) -> AutorouteAttemptResult {
        self.autoroute_connection_impl(board, start, dest, ctrl, ripped, ripup_costs, stop, force)
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn autoroute_connection_impl(
        &mut self,
        board: &mut Board,
        start: &BTreeSet<ItemId>,
        dest: &BTreeSet<ItemId>,
        ctrl: &AutorouteControl,
        ripped: &mut BTreeSet<ItemId>,
        ripup_costs: Option<&mut BTreeMap<ItemId, i32>>,
        stop: StopCheck<'_>,
        panic_in_locator: bool,
    ) -> AutorouteAttemptResult {
        let start_names = connection_item_names(board, start);
        let dest_names = connection_item_names(board, dest);

        let maze_outcome: Option<Option<MazeResult>> = {
            let engine: &mut AutorouteEngine = self;
            let board: &mut Board = board;
            std::panic::catch_unwind(AssertUnwindSafe(move || {
                let mut maze_search_algo =
                    MazeSearchEngine::get_instance(start, dest, engine, board, ctrl, stop)?;
                Some(
                    std::panic::catch_unwind(AssertUnwindSafe(|| {
                        maze_search_algo.find_connection(board, stop)
                    }))
                    .unwrap_or(None),
                )
            }))
            .unwrap_or(None)
        };

        let Some(search_result) = maze_outcome else {
            return AutorouteAttemptResult::with_details(
                AutorouteAttemptState::Failed,
                format!(
                    "Failed to route connection between {}, because the maze search algorithm \
                     could not be created.",
                    describe_connection_from_names(&start_names, &dest_names)
                ),
            );
        };

        let autoroute_result = match search_result.as_ref() {
            None => None,
            Some(search_result) => {
                let engine: &mut AutorouteEngine = self;
                let angle_restriction = board.rules.trace_angle_restriction;
                let board: &mut Board = board;
                let ripped: &mut BTreeSet<ItemId> = ripped;
                std::panic::catch_unwind(AssertUnwindSafe(move || {
                    assert!(
                        !panic_in_locator,
                        "AutorouteEngine.autoroute_connection: the injected \
                         FoundConnectionLocator.get_instance failure of plan-6 ruling 7's fourth \
                         recovery boundary (AutorouteEngine.java:190)"
                    );
                    FoundConnectionLocator::get_instance(
                        Some(search_result),
                        ctrl,
                        engine,
                        board,
                        angle_restriction,
                        ripped,
                        ripup_costs,
                    )
                }))
                .unwrap_or(None)
            }
        };

        if self.maintain_database {
            self.reset_all_doors(board);
        } else {
            self.clear(board);
        }

        if search_result.is_none() {
            return AutorouteAttemptResult::with_details(
                AutorouteAttemptState::Failed,
                format!(
                    "Failed to route connection between {}, because no connection was found \
                     between their nets.",
                    describe_connection_from_names(&start_names, &dest_names)
                ),
            );
        }

        let Some(autoroute_result) = autoroute_result else {
            return AutorouteAttemptResult::with_details(
                AutorouteAttemptState::Failed,
                format!(
                    "Failed to route connection between {}.",
                    describe_connection_from_names(&start_names, &dest_names)
                ),
            );
        };

        if !ctrl.layer_active[autoroute_result.start_layer]
            || !ctrl.layer_active[autoroute_result.target_layer]
        {
            return AutorouteAttemptResult::with_details(
                AutorouteAttemptState::Failed,
                format!(
                    "Failed to route connection between {}, because some of their layers are \
                     disabled.",
                    describe_connection_from_names(&start_names, &dest_names)
                ),
            );
        }

        let mut ripped_connections: BTreeSet<ItemId> = BTreeSet::new();
        let mut changed_nets: BTreeSet<i32> = BTreeSet::new();
        let stop_connection_option = if ctrl.remove_unconnected_vias {
            StopConnectionOption::None
        } else {
            StopConnectionOption::FanoutVia
        };

        for current_ripped_item in ripped.iter().rev() {
            ripped_connections
                .extend(board.connection_items(*current_ripped_item, stop_connection_option));
            let Some(item) = board.get_item(*current_ripped_item) else {
                continue;
            };
            if super::region_history::enabled() {
                let ctx = board.ctx();
                super::region_history::record(
                    &item.bounding_box(&ctx),
                    item.first_layer(&ctx),
                    item.last_layer(&ctx),
                );
            }
            for i in 0..item.net_count() {
                changed_nets.insert(item.get_net_number(i));
            }
        }
        for net_no in &changed_nets {
            super::ripup_history::record(*net_no);
        }

        board.remove_items(ripped_connections.iter().rev().copied());
        if !ripped_connections.is_empty() {
            crate::autoroute::instrument::note_mutation(
                crate::autoroute::instrument::Mutation::RipupRemoveItems,
            );
        }

        for current_net_number in &changed_nets {
            crate::autoroute::instrument::note_mutation(
                crate::autoroute::instrument::Mutation::RemoveTraceTails,
            );
            if let Err(error) =
                board.remove_trace_tails(*current_net_number, stop_connection_option)
            {
                panic!(
                    "AutorouteEngine.autoroute_connection:263: removeTraceTails failed ({error}) \
                     — Java's throw here is caught only by \
                     AutorouteConnectionRouter.route:155-158"
                );
            }
        }

        let insert_found_connection_algo = FoundConnectionInserter::get_instance(
            Some(&autoroute_result),
            board,
            ctrl,
            Some(self),
            &|| false,
        );
        match insert_found_connection_algo {
            Err(error) => panic!(
                "AutorouteEngine.autoroute_connection:265: FoundConnectionInserter.getInstance \
                 failed ({error}) — Java's throw here is caught only by \
                 AutorouteConnectionRouter.route:155-158"
            ),
            Ok(None) => AutorouteAttemptResult::with_details(
                AutorouteAttemptState::Failed,
                format!(
                    "Failed to route connection between {}, because the new connection could not \
                     be inserted.",
                    describe_connection_from_names(&start_names, &dest_names)
                ),
            ),
            Ok(Some(_)) => {
                crate::visualization::capture_route_committed(board, self);
                AutorouteAttemptResult::new(AutorouteAttemptState::Routed)
            }
        }
    }
}

pub(crate) fn tree_of(board: &Board, tree_id: TreeId) -> &ShapeSearchTree {
    board
        .trees
        .trees()
        .find(|tree| tree.id() == tree_id)
        .unwrap_or_else(|| panic!("AutorouteEngine: no search tree with id {tree_id:?}"))
}

pub(crate) fn tree_mut(board: &mut Board, tree_id: TreeId) -> &mut ShapeSearchTree {
    board
        .trees
        .trees_mut()
        .find(|tree| tree.id() == tree_id)
        .unwrap_or_else(|| panic!("AutorouteEngine: no search tree with id {tree_id:?}"))
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "AutorouteEngine.complete_expansion_room: a panic with no message".to_string()
    }
}

pub fn describe_connection(
    board: &Board,
    start_set: &BTreeSet<ItemId>,
    dest_set: &BTreeSet<ItemId>,
) -> String {
    describe_connection_from_names(
        &connection_item_names(board, start_set),
        &connection_item_names(board, dest_set),
    )
}

pub(crate) fn connection_item_names(board: &Board, set: &BTreeSet<ItemId>) -> Vec<String> {
    set.iter()
        .rev()
        .filter_map(|id| board.get_item(*id))
        .map(ToString::to_string)
        .collect()
}

pub(crate) fn describe_connection_from_names(start: &[String], dest: &[String]) -> String {
    format!("{} and {}", start.join(", "), dest.join(", "))
}

#[allow(clippy::too_many_arguments)]
pub fn route_connection(
    board: &mut Board,
    engine: &mut Option<AutorouteEngine>,
    item: ItemId,
    net_no: i32,
    settings: &RouterSettings,
    trace_costs: &[ExpansionCostFactor],
    ripped: &mut BTreeSet<ItemId>,
    ripup_costs: &mut BTreeMap<ItemId, i32>,
    ripup_pass_no: i32,
    start_ripup_costs: i32,
    remove_unconnected_vias: bool,
    retain_autoroute_database: bool,
    stop: StopCheck<'_>,
) -> AutorouteAttemptResult {
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        match route_connection_steps_1_to_5(
            board,
            engine,
            item,
            net_no,
            settings,
            trace_costs,
            ViaPricing::ByPadstackRadius,
            ripped,
            ripup_costs,
            ripup_pass_no,
            start_ripup_costs,
            remove_unconnected_vias,
            retain_autoroute_database,
            false,
            stop,
        ) {
            Steps1To5::Early(result) | Steps1To5::Ran { result, .. } => result,
        }
    }))
    .unwrap_or_else(|_| AutorouteAttemptResult::new(AutorouteAttemptState::Failed))
}

struct RouteContext {
    autoroute_control: AutorouteControl,
    current_via_costs: i32,
    route_start_set: BTreeSet<ItemId>,
    route_dest_set: BTreeSet<ItemId>,
    max_item_id_before_route: ItemId,
    strict_drc_board_snapshot: Option<Board>,
}

enum Steps1To5 {
    Early(AutorouteAttemptResult),
    Ran {
        result: AutorouteAttemptResult,
        context: Box<RouteContext>,
    },
}

fn connection_time_limit(ripup_pass_no: i32) -> TimeLimit {
    let max_milliseconds =
        (100_000.0 * f64::powf(2.0, f64::from(ripup_pass_no - 1))).min(f64::from(i32::MAX));
    TimeLimit::new(max_milliseconds as i32)
}

#[allow(clippy::too_many_arguments)]
fn route_connection_steps_1_to_5(
    board: &mut Board,
    engine: &mut Option<AutorouteEngine>,
    item: ItemId,
    net_no: i32,
    settings: &RouterSettings,
    trace_costs: &[ExpansionCostFactor],
    via_pricing: ViaPricing,
    ripped: &mut BTreeSet<ItemId>,
    ripup_costs: &mut BTreeMap<ItemId, i32>,
    ripup_pass_no: i32,
    start_ripup_costs: i32,
    remove_unconnected_vias: bool,
    retain_autoroute_database: bool,
    take_strict_drc_snapshot: bool,
    stop: StopCheck<'_>,
) -> Steps1To5 {
    let route_net = board.rules.nets.get(net_no);
    let contains_plane = route_net.is_some_and(copper_board::rules::Net::contains_plane);
    let current_via_costs = if contains_plane {
        settings.get_plane_via_costs()
    } else {
        settings.get_via_costs()
    };

    let mut autoroute_control = AutorouteControl::priced(
        board,
        net_no,
        settings,
        current_via_costs,
        trace_costs,
        via_pricing,
    );
    autoroute_control.ripup_allowed = true;
    autoroute_control.ripup_costs = start_ripup_costs * ripup_pass_no;
    autoroute_control.remove_unconnected_vias = remove_unconnected_vias;

    let unconnected_set = board.unconnected_set(item, net_no);
    if unconnected_set.is_empty() {
        return Steps1To5::Early(AutorouteAttemptResult::new(
            AutorouteAttemptState::NoUnconnectedNets,
        ));
    }

    let connected_set = board.connected_set(item, net_no, false);
    let (route_start_set, route_dest_set) = if contains_plane {
        for current_item in connected_set.iter().rev() {
            if matches!(board.get_item(*current_item), Some(Item::ConductionArea(_))) {
                return Steps1To5::Early(AutorouteAttemptResult::new(
                    AutorouteAttemptState::ConnectedToPlane,
                ));
            }
        }
        (connected_set, unconnected_set)
    } else {
        (unconnected_set, connected_set)
    };

    let time_limit = connection_time_limit(ripup_pass_no);

    *engine = Some(board.init_autoroute(
        engine.take(),
        net_no,
        autoroute_control.trace_clearance_class_index,
        Some(time_limit),
        retain_autoroute_database,
    ));

    let max_item_id_before_route = board.communication.id_gen.max_generated_id();
    let strict_drc_board_snapshot = if take_strict_drc_snapshot && settings.is_strict_drc() {
        Some(board.clone())
    } else {
        None
    };

    let autoroute_engine = engine
        .as_mut()
        .expect("initAutoroute always answers an engine");

    let result = autoroute_engine.autoroute_connection(
        board,
        &route_start_set,
        &route_dest_set,
        &autoroute_control,
        ripped,
        Some(ripup_costs),
        stop,
    );

    Steps1To5::Ran {
        result,
        context: Box::new(RouteContext {
            autoroute_control,
            current_via_costs,
            route_start_set,
            route_dest_set,
            max_item_id_before_route,
            strict_drc_board_snapshot,
        }),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn route_connection_full(
    board: &mut Board,
    engine: &mut Option<AutorouteEngine>,
    item: ItemId,
    net_no: i32,
    settings: &RouterSettings,
    trace_costs: &[ExpansionCostFactor],
    via_pricing: ViaPricing,
    ripped: &mut BTreeSet<ItemId>,
    ripup_costs: &mut BTreeMap<ItemId, i32>,
    ripup_pass_no: i32,
    start_ripup_costs: i32,
    remove_unconnected_vias: bool,
    trace_pull_tight_accuracy: i32,
    budget: RouterBudget,
    stop: StopCheck<'_>,
    search_budget: Option<&ConnectionBudget>,
) -> AutorouteAttemptResult {
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        route_connection_steps_1_to_8(
            board,
            engine,
            item,
            net_no,
            settings,
            trace_costs,
            via_pricing,
            ripped,
            ripup_costs,
            ripup_pass_no,
            start_ripup_costs,
            remove_unconnected_vias,
            trace_pull_tight_accuracy,
            budget,
            stop,
            search_budget,
        )
    }))
    .unwrap_or_else(|_| AutorouteAttemptResult::new(AutorouteAttemptState::Failed))
}

#[allow(clippy::too_many_arguments)]
fn route_connection_steps_1_to_8(
    board: &mut Board,
    engine: &mut Option<AutorouteEngine>,
    item: ItemId,
    net_no: i32,
    settings: &RouterSettings,
    trace_costs: &[ExpansionCostFactor],
    via_pricing: ViaPricing,
    ripped: &mut BTreeSet<ItemId>,
    ripup_costs: &mut BTreeMap<ItemId, i32>,
    ripup_pass_no: i32,
    start_ripup_costs: i32,
    remove_unconnected_vias: bool,
    trace_pull_tight_accuracy: i32,
    budget: RouterBudget,
    stop: StopCheck<'_>,
    search_budget: Option<&ConnectionBudget>,
) -> AutorouteAttemptResult {
    let search_stop = &|| stop() || search_budget.is_some_and(ConnectionBudget::exceeded);
    let (autoroute_result, context) = match route_connection_steps_1_to_5(
        board,
        engine,
        item,
        net_no,
        settings,
        trace_costs,
        via_pricing,
        ripped,
        ripup_costs,
        ripup_pass_no,
        start_ripup_costs,
        remove_unconnected_vias,
        BatchAutorouter::BENCHMARK_RETAIN_AUTOROUTE_DATABASE,
        true,
        search_stop,
    ) {
        Steps1To5::Early(result) => return result,
        Steps1To5::Ran { result, context } => (result, *context),
    };

    if autoroute_result.state == AutorouteAttemptState::Routed {
        crate::autoroute::instrument::note_mutation(
            crate::autoroute::instrument::Mutation::OptChangedArea,
        );
        board
            .opt_changed_area(
                engine.as_mut(),
                &[],
                None,
                trace_pull_tight_accuracy,
                Some(&context.autoroute_control.trace_costs),
                stop,
                budget.opt_changed_area_ms,
            )
            .expect("an Err here becomes route's own catch (:156-159) via catch_unwind");
    }

    if (autoroute_result.state == AutorouteAttemptState::Failed
        || autoroute_result.state == AutorouteAttemptState::InsertError)
        && settings.get_neck_width_um() > 0.0
    {
        let necked_result = retry_connection_necked(
            board,
            engine,
            net_no,
            &context,
            settings,
            trace_costs,
            ripped,
            ripup_costs,
            ripup_pass_no,
            start_ripup_costs,
            remove_unconnected_vias,
            trace_pull_tight_accuracy,
            budget,
            stop,
            search_budget,
        );
        if let Some(necked_result) = necked_result {
            let strict_result = apply_strict_drc_after_route(
                board,
                settings,
                net_no,
                context.max_item_id_before_route,
                context.strict_drc_board_snapshot,
            );
            if let Some(strict_result) = strict_result {
                return strict_result;
            }
            return necked_result;
        }
        return autoroute_result;
    }

    if autoroute_result.state == AutorouteAttemptState::Routed {
        let strict_result = apply_strict_drc_after_route(
            board,
            settings,
            net_no,
            context.max_item_id_before_route,
            context.strict_drc_board_snapshot,
        );
        if let Some(strict_result) = strict_result {
            return strict_result;
        }
    }

    autoroute_result
}

#[allow(clippy::too_many_arguments)]
fn retry_connection_necked(
    board: &mut Board,
    engine: &mut Option<AutorouteEngine>,
    route_net_no: i32,
    context: &RouteContext,
    settings: &RouterSettings,
    trace_costs: &[ExpansionCostFactor],
    ripped: &mut BTreeSet<ItemId>,
    ripup_costs: &mut BTreeMap<ItemId, i32>,
    ripup_pass_no: i32,
    start_ripup_costs: i32,
    remove_unconnected_vias: bool,
    trace_pull_tight_accuracy: i32,
    budget: RouterBudget,
    stop: StopCheck<'_>,
    search_budget: Option<&ConnectionBudget>,
) -> Option<AutorouteAttemptResult> {
    let search_stop = &|| stop() || search_budget.is_some_and(ConnectionBudget::exceeded);
    let original_control = &context.autoroute_control;

    let board_resolution = board.communication.resolution.max(1);
    let neck_width = (Unit::scale(
        settings.get_neck_width_um() * f64::from(board_resolution),
        Unit::Um,
        board.communication.unit,
    ))
    .round() as i64 as i32;
    let neck_half_width = std::cmp::max(1, neck_width / 2);

    let narrower_somewhere = (0..original_control.layer_count).any(|i| {
        original_control.layer_active[i] && original_control.trace_half_width[i] > neck_half_width
    });
    if !narrower_somewhere {
        return None;
    }

    let mut neck_control = AutorouteControl::priced(
        board,
        route_net_no,
        settings,
        context.current_via_costs,
        trace_costs,
        original_control.via_pricing,
    );
    neck_control.ripup_allowed = true;
    neck_control.ripup_costs = start_ripup_costs * ripup_pass_no;
    neck_control.remove_unconnected_vias = remove_unconnected_vias;
    for i in 0..neck_control.layer_count {
        let compensation =
            neck_control.compensated_trace_half_width[i] - neck_control.trace_half_width[i];
        neck_control.trace_half_width[i] =
            std::cmp::min(neck_control.trace_half_width[i], neck_half_width);
        neck_control.compensated_trace_half_width[i] =
            neck_control.trace_half_width[i] + compensation;
    }

    *engine = Some(board.init_autoroute(
        engine.take(),
        route_net_no,
        neck_control.trace_clearance_class_index,
        Some(connection_time_limit(ripup_pass_no)),
        BatchAutorouter::BENCHMARK_RETAIN_AUTOROUTE_DATABASE,
    ));
    let neck_engine = engine
        .as_mut()
        .expect("initAutoroute always answers an engine");

    let neck_result = neck_engine.autoroute_connection(
        board,
        &context.route_start_set,
        &context.route_dest_set,
        &neck_control,
        ripped,
        Some(ripup_costs),
        search_stop,
    );
    if neck_result.state != AutorouteAttemptState::Routed {
        return None;
    }

    crate::autoroute::instrument::note_mutation(
        crate::autoroute::instrument::Mutation::OptChangedArea,
    );
    board
        .opt_changed_area(
            engine.as_mut(),
            &[],
            None,
            trace_pull_tight_accuracy,
            Some(&neck_control.trace_costs),
            stop,
            budget.opt_changed_area_ms,
        )
        .expect("an Err here becomes route's own catch (:156-159) via catch_unwind");
    Some(neck_result)
}

fn apply_strict_drc_after_route(
    board: &mut Board,
    settings: &RouterSettings,
    route_net_no: i32,
    max_item_id_before: ItemId,
    board_snapshot_before_route: Option<Board>,
) -> Option<AutorouteAttemptResult> {
    if !settings.is_strict_drc() {
        return None;
    }
    let rejection = BatchAutorouter::enforce_strict_drc(board, route_net_no, max_item_id_before);
    if rejection.is_some()
        && let Some(snapshot) = board_snapshot_before_route
    {
        *board = snapshot;
    }
    rejection
}

fn p7t14b_shape(shape: Option<&TileShape>) -> String {
    let Some(shape) = shape else {
        return "null".to_string();
    };
    let mut rendered = String::from("{");
    for (index, corner) in shape.corner_approx_arr().into_iter().enumerate() {
        if index > 0 {
            rendered.push(';');
        }
        rendered.push_str(&format!("{:.3},{:.3}", corner.x, corner.y));
    }
    rendered.push('}');
    rendered
}
