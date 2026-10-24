use copper_board::items::Item;
use copper_board::prelude::*;
use copper_board::{BoardError, ItemId};
use copper_geometry::{FloatLine, FloatPoint, IntPoint, Point, Side, Vector};
use copper_settings::ExpansionCostFactor;

use crate::board_ext::drill_item_mover::DrillItemMover;
use crate::board_ext::tightener::PolylineTraceExt;

pub struct ViaOptimizer;

impl ViaOptimizer {
    pub fn opt_via_location(
        board: &mut Board,
        via: ItemId,
        trace_costs: Option<&[ExpansionCostFactor]>,
        trace_pull_tight_accuracy: i32,
        max_recursion_depth: i32,
    ) -> Result<bool, BoardError> {
        Self::opt_via_location_until(
            board,
            via,
            trace_costs,
            trace_pull_tight_accuracy,
            max_recursion_depth,
            &|| false,
        )
    }

    /// `opt_via_location`, giving up between moves once `stop` says so: on copper that is not
    /// pulled tight, each move finds another and one chain can run for minutes.
    pub fn opt_via_location_until(
        board: &mut Board,
        via: ItemId,
        trace_costs: Option<&[ExpansionCostFactor]>,
        trace_pull_tight_accuracy: i32,
        max_recursion_depth: i32,
        stop: StopCheck<'_>,
    ) -> Result<bool, BoardError> {
        if stop() {
            return Ok(false);
        }
        let Some(item) = board.get_item(via) else {
            return Ok(false);
        };
        if !matches!(item, Item::Via(_)) {
            return Ok(false);
        }
        if item.is_shove_fixed(&board.rules) {
            return Ok(false);
        }
        if max_recursion_depth <= 0 {
            return Ok(false);
        }
        let contacts: Vec<ItemId> = board.normal_contacts(via).into_iter().rev().collect();
        let mut is_plane_or_fanout_via = contacts.len() == 1;
        let mut first_trace: Option<ItemId> = None;
        let mut second_trace: Option<ItemId> = None;
        if !is_plane_or_fanout_via {
            if contacts.len() != 2 {
                return Ok(false);
            }
            for (slot, current_item) in [&mut first_trace, &mut second_trace]
                .into_iter()
                .zip(contacts.iter().copied())
            {
                match Self::contact_role(board, current_item) {
                    ContactRole::FreeTrace => *slot = Some(current_item),
                    ContactRole::Plane => is_plane_or_fanout_via = true,
                    ContactRole::Unusable => return Ok(false),
                }
            }
        }
        if is_plane_or_fanout_via {
            return Self::opt_plane_or_fanout_via_until(
                board,
                via,
                trace_pull_tight_accuracy,
                max_recursion_depth,
                stop,
            );
        }
        let (Some(first_trace), Some(second_trace)) = (first_trace, second_trace) else {
            return Ok(false);
        };
        let Some(via_center) = board.drill_center(via) else {
            return Ok(false);
        };
        let first_layer = Self::trace_layer(board, first_trace);
        let second_layer = Self::trace_layer(board, second_trace);

        let Some(first_trace_from_corner) = Self::from_corner(board, first_trace, &via_center)
        else {
            return Ok(false);
        };
        let Some(second_trace_from_corner) = Self::from_corner(board, second_trace, &via_center)
        else {
            return Ok(false);
        };

        let (first_layer_trace_costs, second_layer_trace_costs) = match trace_costs {
            Some(costs) => (costs[first_layer], costs[second_layer]),
            None => {
                let unit = ExpansionCostFactor {
                    horizontal: 1.0,
                    vertical: 1.0,
                };
                (unit, unit)
            }
        };

        let new_location = Self::reposition_via_general(
            board,
            via,
            Self::trace_half_width(board, first_trace),
            Self::trace_clearance_class(board, first_trace),
            first_layer,
            first_layer_trace_costs,
            &first_trace_from_corner,
            Self::trace_half_width(board, second_trace),
            Self::trace_clearance_class(board, second_trace),
            second_layer,
            second_layer_trace_costs,
            &second_trace_from_corner,
        );
        let Some(new_location) = new_location else {
            return Ok(false);
        };
        if new_location == via_center {
            return Ok(false);
        }
        let delta = new_location.difference_by(&via_center);
        // that reason. `FRLogger.warn("OptViaAlgo.opt_via_location: move via failed")` is dropped.
        if !DrillItemMover::insert(board, via, &delta, 9, 9, None, &|| false)? {
            return Ok(false);
        }
        for layer in [first_layer, second_layer] {
            let picked: Vec<ItemId> = board
                .pick_traces(&new_location, Some(layer))
                .into_iter()
                .rev()
                .collect();
            for current_item in picked {
                <Board as PolylineTraceExt>::pull_tight(
                    board,
                    current_item,
                    true,
                    trace_pull_tight_accuracy,
                    &|| false,
                )?;
            }
        }
        let first_picked_via = board
            .pick_items(&new_location, Some(first_layer))
            .into_iter()
            .rev()
            .find(|id| matches!(board.get_item(*id), Some(Item::Via(_))));
        if let Some(current_item) = first_picked_via {
            Self::opt_via_location_until(
                board,
                current_item,
                trace_costs,
                trace_pull_tight_accuracy,
                max_recursion_depth - 1,
                stop,
            )?;
        }
        Ok(true)
    }

    pub fn opt_plane_or_fanout_via(
        board: &mut Board,
        via: ItemId,
        trace_pull_tight_accuracy: i32,
        max_recursion_depth: i32,
    ) -> Result<bool, BoardError> {
        Self::opt_plane_or_fanout_via_until(
            board,
            via,
            trace_pull_tight_accuracy,
            max_recursion_depth,
            &|| false,
        )
    }

    fn opt_plane_or_fanout_via_until(
        board: &mut Board,
        via: ItemId,
        trace_pull_tight_accuracy: i32,
        max_recursion_depth: i32,
        stop: StopCheck<'_>,
    ) -> Result<bool, BoardError> {
        if max_recursion_depth <= 0 || stop() {
            return Ok(false);
        }
        let contact_list: Vec<ItemId> = board.normal_contacts(via).into_iter().rev().collect();
        if contact_list.is_empty() {
            return Ok(false);
        }
        let mut contact_plane: Option<ItemId> = None;
        let mut contact_trace: Option<ItemId> = None;
        for current_contact in contact_list {
            match board.get_item(current_contact) {
                Some(Item::ConductionArea(_)) => {
                    if contact_plane.is_some() {
                        return Ok(false);
                    }
                    contact_plane = Some(current_contact);
                }
                Some(item) if item.is_trace() => {
                    if item.is_shove_fixed(&board.rules) || contact_trace.is_some() {
                        return Ok(false);
                    }
                    contact_trace = Some(current_contact);
                }
                _ => return Ok(false),
            }
        }
        let Some(contact_trace) = contact_trace else {
            return Ok(false);
        };
        let Some(via_center) = board.drill_center(via) else {
            return Ok(false);
        };

        let at_first_corner = {
            let first = Self::trace_corner(board, contact_trace, TraceEnd::First);
            let last = Self::trace_corner(board, contact_trace, TraceEnd::Last);
            if first.as_ref() == Some(&via_center) {
                true
            } else if last.as_ref() == Some(&via_center) {
                false
            } else {
                return Ok(false);
            }
        };
        let Some(Item::Trace(trace)) = board.get_item(contact_trace) else {
            return Ok(false);
        };
        let trace_polyline = trace.polyline().clone();
        let corner_count = trace_polyline.corner_count();
        let check_corner = if at_first_corner {
            trace_polyline.corner(1)
        } else {
            trace_polyline.corner(corner_count.wrapping_sub(2))
        };
        let Some(check_corner) = check_corner else {
            return Ok(false);
        };
        let rounded_check_corner: IntPoint = check_corner.to_float().round();
        let trace_half_width = Self::trace_half_width(board, contact_trace);
        let trace_layer = Self::trace_layer(board, contact_trace);
        let trace_cl_class_no = Self::trace_clearance_class(board, contact_trace);
        let mut new_via_location = Self::reposition_via_toward_location(
            board,
            via,
            &rounded_check_corner,
            trace_half_width,
            trace_layer,
            trace_cl_class_no,
        );
        if new_via_location.is_none() && corner_count >= 3 {
            let prev_corner = if at_first_corner {
                trace_polyline.corner(2)
            } else {
                trace_polyline.corner(corner_count - 3)
            };
            if let Some(prev_corner) = prev_corner {
                let float_check_corner = check_corner.to_float();
                let float_via_center = via_center.to_float();
                let float_prev_corner = prev_corner.to_float();
                if float_check_corner.scalar_product(&float_via_center, &float_prev_corner) != 0.0 {
                    let Some(projection) = Self::fanout_projection(
                        float_via_center,
                        float_check_corner,
                        float_prev_corner,
                    ) else {
                        return Ok(false);
                    };
                    let diff_vector = projection.difference_by(&via_center);
                    let mut projection_ok = true;
                    let angle_restriction = board.rules.trace_angle_restriction;
                    if projection == via_center
                        || angle_restriction == AngleRestriction::NinetyDegree
                            && !diff_vector.is_orthogonal()
                        || angle_restriction == AngleRestriction::FortyFiveDegree
                            && !diff_vector.is_multiple_of_45_degree()
                    {
                        projection_ok = false;
                    }
                    if projection_ok
                        && DrillItemMover::check(board, via, &diff_vector, 0, 0, None, None)
                    {
                        let net_numbers = Self::net_nos(board, via);
                        let ok_length = board.check_trace_segment(
                            &via_center,
                            &projection,
                            trace_layer,
                            &net_numbers,
                            trace_half_width,
                            trace_cl_class_no,
                            false,
                        );
                        if ok_length >= f64::from(i32::MAX) {
                            new_via_location = Some(projection);
                        }
                    }
                }
            }
        }
        let Some(new_via_location) = new_via_location else {
            return Ok(false);
        };
        // A positive sub-grid allowance can round back to the current location.
        // As in opt_via_location, this is not progress: reporting a successful
        // move would keep changed-area fanout optimization running indefinitely.
        if new_via_location == via_center {
            return Ok(false);
        }
        if let Some(contact_plane) = contact_plane {
            let plane_layer = match board.get_item(contact_plane) {
                Some(Item::ConductionArea(area)) => area.get_layer(),
                _ => return Ok(false),
            };
            let picked: Vec<ItemId> = board
                .pick_items(&new_via_location, Some(plane_layer))
                .into_iter()
                .rev()
                .filter(|id| matches!(board.get_item(*id), Some(Item::ConductionArea(_))))
                .collect();
            if !picked.contains(&contact_plane) {
                return Ok(false);
            }
        }
        let diff_vector = new_via_location.difference_by(&via_center);
        if !DrillItemMover::insert(board, via, &diff_vector, 9, 9, None, &|| false)? {
            return Ok(false);
        }
        let picked: Vec<ItemId> = board
            .pick_traces(&new_via_location, Some(trace_layer))
            .into_iter()
            .rev()
            .collect();
        for current_item in picked {
            <Board as PolylineTraceExt>::pull_tight(
                board,
                current_item,
                true,
                trace_pull_tight_accuracy,
                &|| false,
            )?;
        }
        if new_via_location == check_corner {
            Self::opt_plane_or_fanout_via_until(
                board,
                via,
                trace_pull_tight_accuracy,
                max_recursion_depth - 1,
                stop,
            )?;
        }
        Ok(true)
    }

    fn fanout_projection(
        via: FloatPoint,
        corner: FloatPoint,
        previous: FloatPoint,
    ) -> Option<Point> {
        let line = FloatLine::new(corner, previous);
        let projection = Point::Int(line.perpendicular_projection(&via).round());
        // Exact perpendicular projection shortens the path to the corner.
        // Rounding can reverse that improvement and recreate a just-consumed
        // one-unit stub, causing changed-area optimization to cycle forever.
        if projection.to_float().distance_square(&corner) >= via.distance_square(&corner) {
            return None;
        }
        Some(projection)
    }

    pub fn reposition_via_toward_location(
        board: &mut Board,
        via: ItemId,
        to_location: &IntPoint,
        trace_half_width: i32,
        trace_layer: usize,
        trace_cl_class: usize,
    ) -> Option<Point> {
        let from_location = board.drill_center(via)?;
        let to_point = Point::Int(*to_location);
        if from_location == to_point {
            return None;
        }
        let net_numbers = Self::net_nos(board, via);
        let mut ok_length = board.check_trace_segment(
            &from_location,
            &to_point,
            trace_layer,
            &net_numbers,
            trace_half_width,
            trace_cl_class,
            false,
        );
        if ok_length <= 0.0 {
            return None;
        }
        let float_from_location = from_location.to_float();
        let float_to_location = to_point.to_float();
        let new_float_to_location = if ok_length >= f64::from(i32::MAX) {
            float_to_location
        } else {
            float_from_location.change_length(&float_to_location, ok_length)
        };
        let new_to_location = Point::Int(new_float_to_location.round());
        let delta = new_to_location.difference_by(&from_location);
        if !Self::angle_allows_delta(board.rules.trace_angle_restriction, &delta) {
            return None;
        }
        let check_ok = DrillItemMover::check(board, via, &delta, 0, 0, None, None);
        if check_ok {
            return Some(new_to_location);
        }
        let min_length = 0.3 * f64::from(trace_half_width) + 1.0;
        ok_length = (ok_length).min(float_from_location.distance(&float_to_location));
        let mut current_length = ok_length / 2.0;
        ok_length = 0.0;
        let mut result: Option<Point> = None;
        while current_length >= min_length {
            let check_point = Point::Int(
                float_from_location
                    .change_length(&float_to_location, ok_length + current_length)
                    .round(),
            );
            let delta = check_point.difference_by(&from_location);
            if Self::angle_allows_delta(board.rules.trace_angle_restriction, &delta)
                && DrillItemMover::check(board, via, &delta, 0, 0, None, None)
            {
                ok_length += current_length;
                result = Some(check_point);
            }
            current_length /= 2.0;
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    pub fn reposition_via_check_candidate(
        board: &mut Board,
        via: ItemId,
        to_location: &IntPoint,
        trace_half_width_1: i32,
        trace_layer_1: usize,
        trace_cl_class_1: usize,
        connect_location: &IntPoint,
        trace_half_width_2: i32,
        trace_layer_2: usize,
        trace_cl_class_2: usize,
    ) -> bool {
        let Some(from_location) = board.drill_center(via) else {
            return false;
        };
        let to_point = Point::Int(*to_location);
        if from_location == to_point {
            return false;
        }
        let delta = to_point.difference_by(&from_location);
        if !Self::angle_allows_delta(board.rules.trace_angle_restriction, &delta)
            || board.rules.trace_angle_restriction == AngleRestriction::None
                && delta.length_approx() <= 1.5
        {
            return false;
        }
        let net_numbers = Self::net_nos(board, via);
        let ok_length = board.check_trace_segment(
            &from_location,
            &to_point,
            trace_layer_1,
            &net_numbers,
            trace_half_width_1,
            trace_cl_class_1,
            false,
        );
        if ok_length < f64::from(i32::MAX) {
            return false;
        }
        let ok_length = board.check_trace_segment(
            &to_point,
            &Point::Int(*connect_location),
            trace_layer_2,
            &net_numbers,
            trace_half_width_2,
            trace_cl_class_2,
            false,
        );
        if ok_length < f64::from(i32::MAX) {
            return false;
        }
        DrillItemMover::check(board, via, &delta, 0, 0, None, None)
    }

    // Every `Some` this returns comes from `reposition_via_toward_location` or
    // `reposition_via_check_candidate`, both of which already refuse an angle-restricted
    // delta; this function never applies a delta of its own.
    #[allow(clippy::too_many_arguments)]
    pub fn reposition_via_general(
        board: &mut Board,
        via: ItemId,
        first_trace_half_width: i32,
        first_trace_cl_class: usize,
        first_trace_layer: usize,
        first_trace_costs: ExpansionCostFactor,
        first_trace_from_corner: &Point,
        second_trace_half_width: i32,
        second_trace_cl_class: usize,
        second_trace_layer: usize,
        second_trace_costs: ExpansionCostFactor,
        second_trace_from_corner: &Point,
    ) -> Option<Point> {
        let via_location = board.drill_center(via)?;
        let first_delta = first_trace_from_corner.difference_by(&via_location);
        let second_delta = second_trace_from_corner.difference_by(&via_location);
        let scalar_product = first_delta.scalar_product(&second_delta);
        let float_via_location = via_location.to_float();
        let float_first_trace_from_corner = first_trace_from_corner.to_float();
        let float_second_trace_from_corner = second_trace_from_corner.to_float();
        let first_trace_from_corner_distance =
            float_via_location.distance(&float_first_trace_from_corner);
        let second_trace_from_corner_distance =
            float_via_location.distance(&float_second_trace_from_corner);
        let rounded_first_trace_from_corner = float_first_trace_from_corner.round();
        let rounded_second_trace_from_corner = float_second_trace_from_corner.round();

        if via_location.side_of(first_trace_from_corner, second_trace_from_corner)
            == Side::Collinear
            && scalar_product > 0.0
        {
            if second_trace_from_corner_distance < first_trace_from_corner_distance {
                return Self::reposition_via_toward_location(
                    board,
                    via,
                    &rounded_second_trace_from_corner,
                    first_trace_half_width,
                    first_trace_layer,
                    first_trace_cl_class,
                );
            }
            return Self::reposition_via_toward_location(
                board,
                via,
                &rounded_first_trace_from_corner,
                second_trace_half_width,
                second_trace_layer,
                second_trace_cl_class,
            );
        }
        let mut result: Option<Point>;

        let mut current_weighted_distance_1 = float_via_location.weighted_distance(
            &float_first_trace_from_corner,
            first_trace_costs.horizontal,
            first_trace_costs.vertical,
        );
        let mut current_weighted_distance_2 = float_via_location.weighted_distance(
            &float_first_trace_from_corner,
            second_trace_costs.horizontal,
            second_trace_costs.vertical,
        );

        if current_weighted_distance_1 > current_weighted_distance_2 {
            result = Self::reposition_via_toward_location(
                board,
                via,
                &rounded_first_trace_from_corner,
                second_trace_half_width,
                second_trace_layer,
                second_trace_cl_class,
            );
            if result.is_some() {
                return result;
            }
        }

        current_weighted_distance_1 = float_via_location.weighted_distance(
            &float_second_trace_from_corner,
            second_trace_costs.horizontal,
            second_trace_costs.vertical,
        );
        current_weighted_distance_2 = float_via_location.weighted_distance(
            &float_second_trace_from_corner,
            first_trace_costs.horizontal,
            first_trace_costs.vertical,
        );

        if current_weighted_distance_1 > current_weighted_distance_2 {
            result = Self::reposition_via_toward_location(
                board,
                via,
                &rounded_second_trace_from_corner,
                first_trace_half_width,
                first_trace_layer,
                first_trace_cl_class,
            );
            if result.is_some() {
                return result;
            }
        }

        if scalar_product > 0.0
            && board.rules.trace_angle_restriction != AngleRestriction::NinetyDegree
        {
            let to_point_1: IntPoint;
            let to_point_2: IntPoint;
            let float_to_point_1: FloatPoint;
            let float_to_point_2: FloatPoint;
            if first_trace_from_corner_distance < second_trace_from_corner_distance {
                to_point_1 = rounded_first_trace_from_corner;
                float_to_point_1 = float_first_trace_from_corner;
                float_to_point_2 = float_via_location.change_length(
                    &float_second_trace_from_corner,
                    first_trace_from_corner_distance,
                );
                to_point_2 = float_to_point_2.round();
            } else {
                float_to_point_1 = float_via_location.change_length(
                    &float_first_trace_from_corner,
                    second_trace_from_corner_distance,
                );
                to_point_1 = float_to_point_1.round();
                to_point_2 = rounded_second_trace_from_corner;
                float_to_point_2 = float_second_trace_from_corner;
            }
            current_weighted_distance_1 = float_to_point_1.weighted_distance(
                &float_to_point_2,
                first_trace_costs.horizontal,
                first_trace_costs.vertical,
            );
            current_weighted_distance_2 = float_to_point_1.weighted_distance(
                &float_to_point_2,
                second_trace_costs.horizontal,
                second_trace_costs.vertical,
            );

            if current_weighted_distance_1 > current_weighted_distance_2 {
                result = Self::reposition_via_toward_location(
                    board,
                    via,
                    &to_point_1,
                    second_trace_half_width,
                    second_trace_layer,
                    second_trace_cl_class,
                );
                if result.is_none() {
                    result = Self::reposition_via_toward_location(
                        board,
                        via,
                        &to_point_2,
                        first_trace_half_width,
                        first_trace_layer,
                        first_trace_cl_class,
                    );
                }
            } else {
                result = Self::reposition_via_toward_location(
                    board,
                    via,
                    &to_point_2,
                    first_trace_half_width,
                    first_trace_layer,
                    first_trace_cl_class,
                );
                if result.is_none() {
                    result = Self::reposition_via_toward_location(
                        board,
                        via,
                        &to_point_1,
                        second_trace_half_width,
                        second_trace_layer,
                        second_trace_cl_class,
                    );
                }
            }
            if result.is_some() {
                return result;
            }
        }

        if !first_delta.is_orthogonal() {
            let mut float_check_location =
                FloatPoint::new(float_via_location.x, float_first_trace_from_corner.y);

            current_weighted_distance_1 = float_via_location.weighted_distance(
                &float_first_trace_from_corner,
                first_trace_costs.horizontal,
                first_trace_costs.vertical,
            );
            current_weighted_distance_2 = float_via_location.weighted_distance(
                &float_check_location,
                second_trace_costs.horizontal,
                second_trace_costs.vertical,
            );
            let mut current_weighted_distance_3 = float_check_location.weighted_distance(
                &float_first_trace_from_corner,
                first_trace_costs.horizontal,
                first_trace_costs.vertical,
            );

            if current_weighted_distance_1
                > current_weighted_distance_2 + current_weighted_distance_3
            {
                let check_location = float_check_location.round();
                let check_ok = Self::reposition_via_check_candidate(
                    board,
                    via,
                    &check_location,
                    second_trace_half_width,
                    second_trace_layer,
                    second_trace_cl_class,
                    &rounded_first_trace_from_corner,
                    first_trace_half_width,
                    first_trace_layer,
                    first_trace_cl_class,
                );
                if check_ok {
                    return Some(Point::Int(check_location));
                }
            }

            float_check_location =
                FloatPoint::new(float_first_trace_from_corner.x, float_via_location.y);

            current_weighted_distance_2 = float_via_location.weighted_distance(
                &float_check_location,
                second_trace_costs.horizontal,
                second_trace_costs.vertical,
            );
            current_weighted_distance_3 = float_check_location.weighted_distance(
                &float_first_trace_from_corner,
                first_trace_costs.horizontal,
                first_trace_costs.vertical,
            );

            if current_weighted_distance_1
                > current_weighted_distance_2 + current_weighted_distance_3
            {
                let check_location = float_check_location.round();
                let check_ok = Self::reposition_via_check_candidate(
                    board,
                    via,
                    &check_location,
                    second_trace_half_width,
                    second_trace_layer,
                    second_trace_cl_class,
                    &rounded_first_trace_from_corner,
                    first_trace_half_width,
                    first_trace_layer,
                    first_trace_cl_class,
                );
                if check_ok {
                    return Some(Point::Int(check_location));
                }
            }
        }

        if !second_delta.is_orthogonal() {
            let mut float_check_location =
                FloatPoint::new(float_via_location.x, float_second_trace_from_corner.y);

            current_weighted_distance_1 = float_via_location.weighted_distance(
                &float_second_trace_from_corner,
                second_trace_costs.horizontal,
                second_trace_costs.vertical,
            );
            current_weighted_distance_2 = float_via_location.weighted_distance(
                &float_check_location,
                first_trace_costs.horizontal,
                first_trace_costs.vertical,
            );
            let mut current_weighted_distance_3 = float_check_location.weighted_distance(
                &float_second_trace_from_corner,
                second_trace_costs.horizontal,
                second_trace_costs.vertical,
            );

            if current_weighted_distance_1
                > current_weighted_distance_2 + current_weighted_distance_3
            {
                let check_location = float_check_location.round();
                let check_ok = Self::reposition_via_check_candidate(
                    board,
                    via,
                    &check_location,
                    first_trace_half_width,
                    first_trace_layer,
                    first_trace_cl_class,
                    &rounded_second_trace_from_corner,
                    second_trace_half_width,
                    second_trace_layer,
                    second_trace_cl_class,
                );
                if check_ok {
                    return Some(Point::Int(check_location));
                }
            }

            float_check_location =
                FloatPoint::new(float_second_trace_from_corner.x, float_via_location.y);

            current_weighted_distance_2 = float_via_location.weighted_distance(
                &float_check_location,
                first_trace_costs.horizontal,
                first_trace_costs.vertical,
            );
            current_weighted_distance_3 = float_check_location.weighted_distance(
                &float_second_trace_from_corner,
                second_trace_costs.horizontal,
                second_trace_costs.vertical,
            );

            if current_weighted_distance_1
                > current_weighted_distance_2 + current_weighted_distance_3
            {
                let check_location = float_check_location.round();
                let check_ok = Self::reposition_via_check_candidate(
                    board,
                    via,
                    &check_location,
                    first_trace_half_width,
                    first_trace_layer,
                    first_trace_cl_class,
                    &rounded_second_trace_from_corner,
                    second_trace_half_width,
                    second_trace_layer,
                    second_trace_cl_class,
                );
                if check_ok {
                    return Some(Point::Int(check_location));
                }
            }
        }
        None
    }

    fn contact_role(board: &Board, contact: ItemId) -> ContactRole {
        let Some(item) = board.get_item(contact) else {
            return ContactRole::Unusable;
        };
        if item.is_shove_fixed(&board.rules) || !item.is_trace() {
            if matches!(item, Item::ConductionArea(_)) {
                ContactRole::Plane
            } else {
                ContactRole::Unusable
            }
        } else {
            ContactRole::FreeTrace
        }
    }

    fn from_corner(board: &Board, trace: ItemId, via_center: &Point) -> Option<Point> {
        let first = Self::trace_corner(board, trace, TraceEnd::First);
        let last = Self::trace_corner(board, trace, TraceEnd::Last);
        let Some(Item::Trace(polyline_trace)) = board.get_item(trace) else {
            return None;
        };
        let polyline = polyline_trace.polyline();
        if first.as_ref() == Some(via_center) {
            polyline.corner(1)
        } else if last.as_ref() == Some(via_center) {
            polyline.corner(polyline.corner_count().wrapping_sub(2))
        } else {
            None
        }
    }

    fn angle_allows_delta(angle_restriction: AngleRestriction, delta: &Vector) -> bool {
        match angle_restriction {
            AngleRestriction::None => true,
            AngleRestriction::NinetyDegree => delta.is_orthogonal(),
            AngleRestriction::FortyFiveDegree => delta.is_multiple_of_45_degree(),
        }
    }

    fn trace_corner(board: &Board, trace: ItemId, end: TraceEnd) -> Option<Point> {
        match board.get_item(trace) {
            Some(Item::Trace(t)) => match end {
                TraceEnd::First => t.first_corner(),
                TraceEnd::Last => t.last_corner(),
            },
            _ => None,
        }
    }

    fn trace_layer(board: &Board, trace: ItemId) -> usize {
        match board.get_item(trace) {
            Some(Item::Trace(t)) => t.get_layer(),
            _ => 0,
        }
    }

    fn trace_half_width(board: &Board, trace: ItemId) -> i32 {
        match board.get_item(trace) {
            Some(Item::Trace(t)) => t.get_half_width(),
            _ => 0,
        }
    }

    fn trace_clearance_class(board: &Board, trace: ItemId) -> usize {
        match board.get_item(trace) {
            Some(item) => item.clearance_class(),
            None => 0,
        }
    }

    fn net_nos(board: &Board, item: ItemId) -> Vec<i32> {
        match board.get_item(item) {
            Some(item) => item.net_nos().to_vec(),
            None => Vec::new(),
        }
    }
}

enum ContactRole {
    FreeTrace,
    Plane,
    Unusable,
}

enum TraceEnd {
    First,
    Last,
}

#[cfg(test)]
mod tests {
    use super::*;
    use copper_geometry::IntBox;

    fn layers() -> LayerStructure {
        LayerStructure::new(vec![Layer::new("front", true), Layer::new("back", true)])
    }

    fn board_with_trace(points: &[Point]) -> (Board, ItemId) {
        let bounding_box = IntBox::from_coords(-100_000, -100_000, 100_000, 100_000);
        let clearance_matrix = ClearanceMatrix::get_default_instance(&layers(), 100);
        let rules = BoardRules::new(layers(), clearance_matrix);
        let mut board = Board::new(
            Vec::new(),
            0,
            bounding_box,
            rules,
            BoardLibrary::new(Padstacks::new(layers()), Packages::new()),
            Components::new(),
            Communication::default(),
        );
        let default_class = board.rules.get_default_net_class();
        board.rules.nets.add("N1", 1, false, default_class);
        let trace = board
            .insert_trace_at_points(points, 0, 100, vec![1], 1, FixedState::Unfixed)
            .expect("a trace");
        (board, trace)
    }

    #[test]
    fn matching_the_first_corner_returns_its_neighbour() {
        let (board, trace) =
            board_with_trace(&[Point::new(0, 0), Point::new(0, 100), Point::new(100, 100)]);
        assert_eq!(
            ViaOptimizer::from_corner(&board, trace, &Point::new(0, 0)),
            Some(Point::new(0, 100))
        );
    }

    #[test]
    fn matching_the_last_corner_returns_its_neighbour() {
        let (board, trace) =
            board_with_trace(&[Point::new(100, 100), Point::new(0, 100), Point::new(0, 0)]);
        assert_eq!(
            ViaOptimizer::from_corner(&board, trace, &Point::new(0, 0)),
            Some(Point::new(0, 100))
        );
    }

    #[test]
    fn a_two_corner_trace_returns_the_other_endpoint_from_either_end() {
        let (board, trace) = board_with_trace(&[Point::new(0, 0), Point::new(0, 100)]);
        assert_eq!(
            ViaOptimizer::from_corner(&board, trace, &Point::new(0, 0)),
            Some(Point::new(0, 100)),
            "matching the first corner walks forward to the only other corner"
        );
        assert_eq!(
            ViaOptimizer::from_corner(&board, trace, &Point::new(0, 100)),
            Some(Point::new(0, 0)),
            "matching the last corner walks backward to the only other corner"
        );
    }

    #[test]
    fn a_point_one_unit_off_either_end_matches_neither() {
        let (board, trace) =
            board_with_trace(&[Point::new(0, 0), Point::new(0, 100), Point::new(100, 100)]);
        assert_eq!(
            ViaOptimizer::from_corner(&board, trace, &Point::new(1, 0)),
            None,
            "the retired tolerance-based match would have accepted this; exact match must not"
        );
        assert_eq!(
            ViaOptimizer::from_corner(&board, trace, &Point::new(99, 100)),
            None
        );
    }
}

#[cfg(test)]
mod projection_regression_tests {
    use super::*;

    #[test]
    fn rounded_fanout_projection_does_not_reverse_a_corner_move() {
        // Starling's stalled via alternates x=1366118 and x=1366119.
        // The exact perpendicular projection is useful, but integer rounding
        // moves the via farther from its next corner and recreates the stub
        // consumed by the preceding optimization step.
        let via = FloatPoint::new(1366118.0, -765089.0);
        let corner = FloatPoint::new(1366118.0, -765088.0);
        let previous = FloatPoint::new(1357463.0, -756433.0);
        let snapped = FloatLine::new(corner, previous)
            .perpendicular_projection(&via)
            .round();
        let snapped = Point::Int(snapped).to_float();
        assert!(snapped.distance(&corner) >= via.distance(&corner));
        assert_eq!(ViaOptimizer::fanout_projection(via, corner, previous), None);
    }

    #[test]
    fn fanout_projection_preserves_a_shortening_grid_move() {
        assert_eq!(
            ViaOptimizer::fanout_projection(
                FloatPoint::new(4.0, 2.0),
                FloatPoint::new(0.0, 0.0),
                FloatPoint::new(10.0, 10.0),
            ),
            Some(Point::new(3, 3))
        );
    }
}
