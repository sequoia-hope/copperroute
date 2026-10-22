use copper_board::{Board, FixedState, Item, ItemId, TreeObject};
use copper_geometry::{Line, Polyline};

use crate::autoroute::expansion::{ExpandableRef, ObstacleExpansionRoom, RoomRef};
use crate::autoroute::maze::engine::tree_of;
use crate::autoroute::maze::{
    ALREADY_RIPPED_COSTS, MazeAdjustment, MazeListElement, MazeSearchEngine, TRACE_WIDTH_TOLERANCE,
};
use crate::autoroute::path::Connection;

const FANOUT_COST_CONSTANT: f64 = 20000.0;

pub struct MazeRipupResolver;

impl MazeRipupResolver {
    pub fn calc_fanout_via_ripup_cost_factor(board: &Board, trace: ItemId) -> f64 {
        let Some(Item::Trace(obstacle_trace)) = board.get_item(trace) else {
            return 1.0;
        };
        let half_width = f64::from(obstacle_trace.get_half_width());
        let length = obstacle_trace.get_length();
        for end in 0..2 {
            let current_end_contacts = if end == 0 {
                board.trace_start_contacts(trace)
            } else {
                board.trace_end_contacts(trace)
            };
            if current_end_contacts.len() != 1 {
                continue;
            }
            let current_trace_contact = *current_end_contacts
                .iter()
                .next()
                .expect("the length is exactly one");
            let mut protect_fanout_via = false;
            if let Some(contact) = board.get_item(current_trace_contact) {
                let ctx = board.ctx();
                match contact {
                    Item::Pin(_) => {
                        if contact.first_layer(&ctx) == contact.last_layer(&ctx) {
                            protect_fanout_via = true;
                        }
                    }
                    Item::Trace(contact_trace) => {
                        if contact.get_fixed_state() == FixedState::ShoveFixed
                            && contact_trace.corner_count() == 2
                        {
                            protect_fanout_via = true;
                        }
                    }
                    _ => {}
                }
            }
            if protect_fanout_via {
                let mut fanout_via_cost_factor = half_width / length;
                fanout_via_cost_factor *= fanout_via_cost_factor;
                fanout_via_cost_factor *= FANOUT_COST_CONSTANT;
                return (fanout_via_cost_factor).max(1.0);
            }
        }
        1.0
    }

    pub fn check_ripup(
        search: &mut MazeSearchEngine<'_>,
        board: &mut Board,
        list_element: &MazeListElement,
        obstacle_item: ItemId,
        door_is_small: bool,
    ) -> i32 {
        if !board.get_item(obstacle_item).is_some_and(Item::is_routable) {
            return -1;
        }
        if door_is_small
            && !Self::enter_through_small_door(search, board, list_element, obstacle_item)
        {
            return -1;
        }
        let previous_room = list_element
            .next_room
            .and_then(|room| search.engine.expandable_other_room(list_element.door, room));
        let room_was_shoved = list_element.adjustment != MazeAdjustment::None;
        let previous_item = match previous_room {
            Some(RoomRef::Obstacle(room)) => search
                .engine
                .rooms
                .obstacle_room(room)
                .map(ObstacleExpansionRoom::get_item),
            _ => None,
        };
        if room_was_shoved {
            if let Some(previous_item) = previous_item
                && previous_item != obstacle_item
                && shares_net(board, previous_item, obstacle_item)
            {
                return -1;
            }
        } else if previous_item == Some(obstacle_item) {
            return ALREADY_RIPPED_COSTS;
        }

        let mut fanout_via_cost_factor = 1.0f64;
        let mut cost_factor = 1.0f64;
        let preserve_fanout_protection = !search.ctrl.remove_unconnected_vias
            && search.ctrl.ripup_costs <= search.ctrl.start_ripup_costs.wrapping_mul(2);
        match board.get_item(obstacle_item) {
            Some(Item::Trace(obstacle_trace)) => {
                cost_factor = f64::from(obstacle_trace.get_half_width());
                if preserve_fanout_protection {
                    fanout_via_cost_factor =
                        Self::calc_fanout_via_ripup_cost_factor(board, obstacle_item);
                }
            }
            Some(Item::Via(_)) => {
                let mut look_if_fanout_via = preserve_fanout_protection;
                let contact_list = board.normal_contacts(obstacle_item);
                let mut contact_count = 0i32;
                for current_contact in contact_list.into_iter().rev() {
                    let Some(contact_item) = board.get_item(current_contact) else {
                        return -1;
                    };
                    let Item::Trace(obstacle_trace) = contact_item else {
                        return -1;
                    };
                    if contact_item.is_user_fixed() {
                        return -1;
                    }
                    let contact_half_width = f64::from(obstacle_trace.get_half_width());
                    contact_count += 1;
                    cost_factor = (cost_factor).max(contact_half_width);
                    if look_if_fanout_via && !search.ctrl.is_fanout {
                        let current_fanout_via_cost_factor =
                            Self::calc_fanout_via_ripup_cost_factor(board, current_contact);
                        if current_fanout_via_cost_factor > 1.0 {
                            fanout_via_cost_factor = current_fanout_via_cost_factor;
                            look_if_fanout_via = false;
                        }
                    }
                }
                if fanout_via_cost_factor <= 1.0 {
                    cost_factor *= 0.5 * f64::from((contact_count - 1).max(0));
                }
            }
            _ => {}
        }

        let mut ripup_cost = f64::from(search.ctrl.ripup_costs) * cost_factor;
        let mut detour = 1.0f64;
        if fanout_via_cost_factor <= 1.0 && !search.ctrl.is_fanout {
            if let Some(obstacle_connection) =
                Connection::get(board, &mut search.engine.connections, obstacle_item)
                && let Some(connection) = search.engine.connections.get(obstacle_connection.0)
            {
                detour = connection.get_detour(board);
            }
        }
        let randomize = search.ctrl.ripup_pass_no >= 4 && search.ctrl.ripup_pass_no % 3 != 0;
        if randomize {
            let random_number = search.random_generator.next_double();
            let random_factor = 0.5 + random_number * random_number;
            detour *= random_factor;
        }
        ripup_cost /= detour;
        ripup_cost *= fanout_via_cost_factor;
        if super::ripup_history::enabled()
            && let Some(item) = board.get_item(obstacle_item)
        {
            ripup_cost *= super::ripup_history::cost_multiplier(
                (0..item.net_count()).map(|i| item.get_net_number(i)),
            );
        }
        let result = (ripup_cost as i32).max(1);
        let max_ripup_costs = i32::MAX / 100;
        result.min(max_ripup_costs)
    }

    pub fn check_leaving_ripped_item(
        search: &mut MazeSearchEngine<'_>,
        board: &mut Board,
        list_element: &MazeListElement,
    ) -> bool {
        let ExpandableRef::Door(current_door) = list_element.door else {
            return false;
        };
        let Some(next_room) = list_element.next_room else {
            return false;
        };
        let from_room = search
            .engine
            .rooms
            .door(current_door)
            .and_then(|door| door.other_complete_room(next_room));
        let Some(RoomRef::Obstacle(obstacle_room)) = from_room else {
            return false;
        };
        let Some(current_item) = search
            .engine
            .rooms
            .obstacle_room(obstacle_room)
            .map(ObstacleExpansionRoom::get_item)
        else {
            return false;
        };
        if !board.get_item(current_item).is_some_and(Item::is_routable) {
            return false;
        }
        Self::enter_through_small_door(search, board, list_element, current_item)
    }

    pub fn enter_through_small_door(
        search: &mut MazeSearchEngine<'_>,
        board: &mut Board,
        list_element: &MazeListElement,
        ignore_item: ItemId,
    ) -> bool {
        if Self::expandable_dimension(search, list_element.door) != 1 {
            return false;
        }
        let ExpandableRef::Door(door) = list_element.door else {
            return false;
        };
        let Some(door_shape) = search.engine.rooms.door_shape(door) else {
            return false;
        };
        let mut door_line: Option<Line> = None;
        let Some(mut prev_corner) = door_shape.corner_approx(0) else {
            return false;
        };
        let corner_count = door_shape.border_line_count();
        for i in 1..corner_count {
            let Some(next_corner) = door_shape.corner_approx(i) else {
                break;
            };
            if next_corner.distance_square(&prev_corner) > 1.0 {
                door_line = door_shape.border_line(i - 1);
                break;
            }
            prev_corner = next_corner;
        }
        let Some(door_line) = door_line else {
            return false;
        };

        let door_centre = door_shape.centre_of_gravity().round();
        let Some(current_layer) = list_element
            .next_room
            .and_then(|room| search.engine.rooms.room_layer(board, room))
        else {
            return false;
        };
        let check_radius =
            search.ctrl.compensated_trace_half_width[current_layer] + TRACE_WIDTH_TOLERANCE;
        let lines = vec![
            door_line.translate(f64::from(check_radius)),
            Line::from_direction(door_centre, &door_line.direction().turn_45_degree(2)),
            door_line.translate(-f64::from(check_radius)),
        ];
        let Ok(check_polyline) = Polyline::from_lines(lines) else {
            return false;
        };
        let Some(check_shape) = check_polyline.offset_shape(check_radius, 0) else {
            return false;
        };

        let ignore_net_nos = [search.ctrl.net_number];
        let overlapping_objects = {
            let ctx = board.ctx();
            tree_of(board, search.search_tree).overlapping_objects_with_rooms(
                &check_shape,
                Some(current_layer),
                &ignore_net_nos,
                &board.items,
                &search.engine.rooms,
                &ctx,
            )
        };

        for current_object in overlapping_objects {
            let TreeObject::Item(current_item) = current_object else {
                continue;
            };
            if current_item == ignore_item {
                continue;
            }
            if !shares_net(board, current_item, ignore_item) {
                return false;
            }
            if !board.normal_contacts(current_item).contains(&ignore_item) {
                return false;
            }
        }
        true
    }

    fn expandable_dimension(search: &MazeSearchEngine<'_>, object: ExpandableRef) -> i32 {
        search.engine.expandable_dimension(object)
    }
}

fn shares_net(board: &Board, a: ItemId, b: ItemId) -> bool {
    match (board.get_item(a), board.get_item(b)) {
        (Some(a), Some(b)) => a.shares_net(b),
        _ => false,
    }
}
