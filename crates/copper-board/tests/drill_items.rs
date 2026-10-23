use copper_board::items::{ItemCtx, TraceExitRestriction};
use copper_board::prelude::*;
use copper_geometry::{
    Direction, FloatPoint, IntBox, IntDirection, IntPoint, Point, Polyline, Shape, ShapeOps,
    TileShape, Vector,
};

fn layers() -> LayerStructure {
    LayerStructure::new(vec![Layer::new("F.Cu", true), Layer::new("B.Cu", true)])
}

fn boxed(x0: i32, y0: i32, x1: i32, y1: i32) -> Shape {
    Shape::Tile(TileShape::Box(IntBox::from_coords(x0, y0, x1, y1)))
}

fn rules() -> BoardRules {
    let ls = layers();
    let cm = ClearanceMatrix::get_default_instance(&ls, 100);
    BoardRules::new(ls, cm)
}

fn library() -> BoardLibrary {
    let mut padstacks = Padstacks::new(layers());
    let tht = padstacks.add(
        "THT",
        vec![Some(boxed(-10, -20, 10, 20)), Some(boxed(-15, -15, 15, 15))],
        true,
        false,
    );
    let smd = padstacks.add(
        "SMD",
        vec![Some(boxed(-100, -50, 100, 50)), None],
        true,
        false,
    );
    let via_pad = padstacks.add(
        "VIA",
        vec![Some(boxed(-30, -30, 30, 30)), None],
        true,
        false,
    );
    assert_eq!((tht.0, smd.0, via_pad.0), (1, 2, 3));

    let mut packages = Packages::new();
    packages.add_pins(vec![
        PackagePin::new("1", tht, Vector::new(30, 0), 0.0),
        PackagePin::new("2", tht, Vector::new(-30, 0), 0.0),
    ]);
    packages.add_pins(vec![
        PackagePin::new("1", smd, Vector::new(0, 0), 0.0),
        PackagePin::new("2", smd, Vector::new(500, 0), 0.0),
    ]);
    packages.add_pins(vec![
        PackagePin::new("1", smd, Vector::new(0, 0), 0.0),
        PackagePin::new("2", smd, Vector::new(500, 0), 0.0),
        PackagePin::new("3", smd, Vector::new(1000, 0), 0.0),
        PackagePin::new("4", smd, Vector::new(1500, 0), 0.0),
    ]);
    BoardLibrary::new(padstacks, packages)
}

fn hdr(id: u32, net_nos: Vec<i32>, component_id: i32) -> ItemHeader {
    ItemHeader::new(ItemId(id), net_nos, 1, component_id, FixedState::Unfixed)
}

const THT_PADSTACK: PadstackId = PadstackId(1);
const VIA_PADSTACK: PadstackId = PadstackId(3);

const BOARD_BOUNDING_BOX: IntBox = IntBox {
    ll: IntPoint {
        x: -10_000,
        y: -10_000,
    },
    ur: IntPoint {
        x: 10_000,
        y: 10_000,
    },
};

#[test]
fn tht_pin_spans_both_layers_and_places_its_shape_per_layer() {
    let library = library();
    let rules = rules();
    let mut components = Components::new();
    components.add_with_generated_name(Some(Point::new(1000, 2000)), 90.0, true, 1);
    let ctx = ItemCtx {
        library: &library,
        components: &components,
        rules: &rules,
        bounding_box: &BOARD_BOUNDING_BOX,
        max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
    };
    let pin = Pin::new(hdr(1, vec![2], 1), 0);

    assert_eq!(pin.first_layer(&ctx), 0);
    assert_eq!(pin.last_layer(&ctx), 1);
    assert!(pin.is_on_layer(0, &ctx));
    assert!(pin.is_on_layer(1, &ctx));
    assert_eq!(pin.tile_shape_count(&ctx), 2);

    assert_eq!(pin.relative_location(&ctx), Vector::new(0, 30));
    assert_eq!(pin.get_center(&ctx), Point::new(1000, 2030));

    assert_eq!(
        pin.get_shape(0, &ctx)
            .expect("layer 0 has copper")
            .bounding_box(),
        IntBox::from_coords(980, 2020, 1020, 2040)
    );
    assert_eq!(
        pin.get_shape(1, &ctx)
            .expect("layer 1 has copper")
            .bounding_box(),
        IntBox::from_coords(985, 2015, 1015, 2045)
    );
    assert_eq!(
        pin.bounding_box(&ctx),
        IntBox::from_coords(980, 2015, 1020, 2045)
    );

    assert!(pin.get_shape_on_layer(0, &ctx).is_some());
    assert!(pin.get_shape_on_layer(2, &ctx).is_none());

    assert!(!pin.drill_allowed(&ctx));
}

#[test]
fn a_back_placed_pin_mirrors_the_padstack_layers() {
    let library = library();
    let rules = rules();
    let mut components = Components::new();
    components.add_with_generated_name(Some(Point::new(1000, 2000)), 0.0, false, 1);
    let ctx = ItemCtx {
        library: &library,
        components: &components,
        rules: &rules,
        bounding_box: &BOARD_BOUNDING_BOX,
        max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
    };
    let pin = Pin::new(hdr(1, vec![2], 1), 0);

    assert_eq!(pin.first_layer(&ctx), 0);
    assert_eq!(pin.last_layer(&ctx), 1);
    assert_eq!(pin.get_padstack_layer(0, &ctx), 1);
    assert_eq!(pin.get_padstack_layer(1, &ctx), 0);
    assert!(!pin.is_placed_on_front(&ctx));
}

#[test]
fn pin_min_width_takes_the_smallest_bounding_box_side_over_signal_layers() {
    let library = library();
    let rules = rules();
    let mut components = Components::new();
    components.add_with_generated_name(Some(Point::new(1000, 2000)), 0.0, true, 1);
    let ctx = ItemCtx {
        library: &library,
        components: &components,
        rules: &rules,
        bounding_box: &BOARD_BOUNDING_BOX,
        max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
    };
    let pin = Pin::new(hdr(1, vec![2], 1), 0);
    assert_eq!(pin.min_width(&ctx), 20.0);
    assert_eq!(pin.get_min_width(0, &ctx), 20.0);
    assert_eq!(pin.get_max_width(0, &ctx), 40.0);
    assert_eq!(pin.get_trace_neckdown_halfwidth(0, &ctx), 9);
}

#[test]
fn an_attachable_via_is_not_an_obstacle_to_a_same_net_smd_pin() {
    let library = library();
    let rules = rules();
    let mut components = Components::new();
    components.add_with_generated_name(Some(Point::new(1000, 2000)), 0.0, true, 2);
    let ctx = ItemCtx {
        library: &library,
        components: &components,
        rules: &rules,
        bounding_box: &BOARD_BOUNDING_BOX,
        max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
    };

    let smd_pin = Item::Pin(Pin::new(hdr(2, vec![5], 1), 0));
    let Item::Pin(p) = &smd_pin else {
        unreachable!()
    };
    assert!(p.drill_allowed(&ctx));

    let attachable = Item::Via(Via::new(
        hdr(1, vec![5], 0),
        VIA_PADSTACK,
        Point::new(1000, 2000),
        true,
    ));
    assert!(!attachable.is_obstacle(&smd_pin, &ctx));

    let plain = Item::Via(Via::new(
        hdr(3, vec![5], 0),
        VIA_PADSTACK,
        Point::new(1000, 2000),
        false,
    ));
    assert!(plain.is_obstacle(&smd_pin, &ctx));

    assert!(!smd_pin.is_obstacle(&attachable, &ctx));
}

#[test]
fn a_through_hole_pin_is_an_obstacle_to_a_same_net_via() {
    let library = library();
    let rules = rules();
    let mut components = Components::new();
    components.add_with_generated_name(Some(Point::new(1000, 2000)), 0.0, true, 1);
    let ctx = ItemCtx {
        library: &library,
        components: &components,
        rules: &rules,
        bounding_box: &BOARD_BOUNDING_BOX,
        max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
    };
    let tht_pin = Item::Pin(Pin::new(hdr(2, vec![5], 1), 0));
    let via = Item::Via(Via::new(
        hdr(1, vec![5], 0),
        VIA_PADSTACK,
        Point::new(1000, 2000),
        true,
    ));
    assert!(tht_pin.is_obstacle(&via, &ctx));
    assert!(via.is_obstacle(&tht_pin, &ctx));
}

fn dirs_and_lengths(restrictions: &[TraceExitRestriction]) -> Vec<(Direction, f64)> {
    restrictions
        .iter()
        .map(|r| (r.direction.clone(), r.min_length))
        .collect()
}

#[test]
fn trace_exit_restrictions_of_a_rectangular_pad_on_a_short_package() {
    let library = library();
    let rules = rules();
    let mut components = Components::new();
    components.add_with_generated_name(Some(Point::new(1000, 2000)), 0.0, true, 2);
    let ctx = ItemCtx {
        library: &library,
        components: &components,
        rules: &rules,
        bounding_box: &BOARD_BOUNDING_BOX,
        max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
    };
    let pin = Pin::new(hdr(1, vec![5], 1), 0);

    assert_eq!(
        pin.get_shape(0, &ctx).expect("SMD copper").bounding_box(),
        IntBox::from_coords(900, 1950, 1100, 2050)
    );

    let restrictions = pin.get_trace_exit_restrictions(0, &ctx);
    assert_eq!(
        dirs_and_lengths(&restrictions),
        vec![
            (Direction::Int(IntDirection::RIGHT), 100.0),
            (Direction::Int(IntDirection::LEFT), 100.0),
            (Direction::Int(IntDirection::UP), 50.0),
            (Direction::Int(IntDirection::DOWN), 50.0),
        ]
    );
    assert!(pin.has_trace_exit_restrictions(&ctx));
}

#[test]
fn trace_exit_restrictions_of_a_rectangular_pad_on_a_long_package() {
    let library = library();
    let rules = rules();
    let mut components = Components::new();
    components.add_with_generated_name(Some(Point::new(1000, 2000)), 0.0, true, 3);
    let ctx = ItemCtx {
        library: &library,
        components: &components,
        rules: &rules,
        bounding_box: &BOARD_BOUNDING_BOX,
        max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
    };
    let pin = Pin::new(hdr(1, vec![5], 1), 0);

    assert_eq!(
        dirs_and_lengths(&pin.get_trace_exit_restrictions(0, &ctx)),
        vec![
            (Direction::Int(IntDirection::RIGHT), 100.0),
            (Direction::Int(IntDirection::LEFT), 100.0),
        ]
    );
}

#[test]
fn trace_exit_restrictions_follow_the_component_rotation() {
    let library = library();
    let rules = rules();
    let mut components = Components::new();
    components.add_with_generated_name(Some(Point::new(1000, 2000)), 90.0, true, 2);
    let ctx = ItemCtx {
        library: &library,
        components: &components,
        rules: &rules,
        bounding_box: &BOARD_BOUNDING_BOX,
        max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
    };
    let pin = Pin::new(hdr(1, vec![5], 1), 0);

    assert_eq!(
        pin.get_shape(0, &ctx).expect("SMD copper").bounding_box(),
        IntBox::from_coords(950, 1900, 1050, 2100)
    );
    assert_eq!(
        dirs_and_lengths(&pin.get_trace_exit_restrictions(0, &ctx)),
        vec![
            (Direction::Int(IntDirection::UP), 100.0),
            (Direction::Int(IntDirection::DOWN), 100.0),
            (Direction::Int(IntDirection::LEFT), 50.0),
            (Direction::Int(IntDirection::RIGHT), 50.0),
        ]
    );
}

fn empty_ctx_parts() -> (BoardLibrary, Components, BoardRules) {
    (library(), Components::new(), rules())
}

#[test]
fn via_shapes_are_the_padstack_shapes_translated_to_the_centre() {
    let (library, components, rules) = empty_ctx_parts();
    let ctx = ItemCtx {
        library: &library,
        components: &components,
        rules: &rules,
        bounding_box: &BOARD_BOUNDING_BOX,
        max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
    };
    let via = Via::new(
        hdr(1, vec![5], 0),
        THT_PADSTACK,
        Point::new(100, 200),
        false,
    );
    assert_eq!(via.first_layer(&ctx), 0);
    assert_eq!(via.last_layer(&ctx), 1);
    assert_eq!(via.tile_shape_count(&ctx), 2);
    assert_eq!(
        via.get_shape(0, &ctx).expect("layer 0").bounding_box(),
        IntBox::from_coords(90, 180, 110, 220)
    );
    assert_eq!(
        via.get_shape(1, &ctx).expect("layer 1").bounding_box(),
        IntBox::from_coords(85, 185, 115, 215)
    );
    assert_eq!(
        via.bounding_box(&ctx),
        IntBox::from_coords(85, 180, 115, 220)
    );
    assert_eq!(via.smallest_radius(&ctx), 10.0);
    assert_eq!(
        via.get_trace_connection_shape(TreeId(0), 0, &ctx),
        Some(TileShape::Box(IntBox::from_coords(100, 200, 100, 200)))
    );
}

#[test]
fn translating_a_via_moves_its_centre_and_drops_the_shape_cache() {
    let (library, components, rules) = empty_ctx_parts();
    let ctx = ItemCtx {
        library: &library,
        components: &components,
        rules: &rules,
        bounding_box: &BOARD_BOUNDING_BOX,
        max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
    };
    let mut via = Via::new(
        hdr(1, vec![5], 0),
        THT_PADSTACK,
        Point::new(100, 200),
        false,
    );
    assert_eq!(
        via.get_shape(0, &ctx).expect("layer 0").bounding_box(),
        IntBox::from_coords(90, 180, 110, 220)
    );
    via.translate_by(&Vector::new(1000, 0));
    assert_eq!(via.get_center(), Point::new(1100, 200));
    assert_eq!(
        via.get_shape(0, &ctx).expect("layer 0").bounding_box(),
        IntBox::from_coords(1090, 180, 1110, 220)
    );
    via.turn_90_degree(1, &IntPoint::ZERO);
    assert_eq!(via.get_center(), Point::new(-200, 1100));
    via.rotate_approx(-90.0, &FloatPoint::ZERO);
    assert_eq!(via.get_center(), Point::new(1100, 200));
}

#[test]
fn a_via_that_changes_side_recomputes_its_minimum_width() {
    let mut padstacks = Padstacks::new(layers());
    let wide = padstacks.add(
        "WIDE",
        vec![Some(boxed(-50, -50, 50, 50)), None],
        true,
        false,
    );
    let narrow = padstacks.add("NARROW", vec![None, Some(boxed(-5, -5, 5, 5))], true, false);
    let mut library = BoardLibrary::new(padstacks, Packages::new());
    library.set_via_padstacks(vec![wide, narrow]);
    let components = Components::new();
    let rules = rules();
    let ctx = ItemCtx {
        library: &library,
        components: &components,
        rules: &rules,
        bounding_box: &BOARD_BOUNDING_BOX,
        max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
    };

    let mut via = Via::new(hdr(1, vec![5], 0), wide, Point::new(0, 0), false);
    assert_eq!(via.min_width(&ctx), 100.0);
    via.change_placement_side(&IntPoint::ZERO, &ctx);
    assert_eq!(via.get_padstack_id(), narrow);
    assert_eq!(via.first_layer(&ctx), 1);
    assert_eq!(via.min_width(&ctx), 10.0);
}

#[test]
fn swapping_two_pins_exchanges_their_nets_and_their_changed_to_aliases() {
    let mut rules = rules();
    rules.nets.add("GND", 1, false, NetClassId(0));
    rules.nets.add("VCC", 1, false, NetClassId(0));
    let mut a = Pin::new(hdr(10, vec![1], 1), 0);
    let mut b = Pin::new(hdr(11, vec![2], 1), 1);

    assert_eq!(a.get_changed_to(), ItemId(10));
    assert_eq!(b.get_changed_to(), ItemId(11));

    assert!(a.swap(&mut b, &rules.nets));
    assert_eq!(a.hdr.net_nos, vec![2]);
    assert_eq!(b.hdr.net_nos, vec![1]);
    assert_eq!(a.get_changed_to(), ItemId(11));
    assert_eq!(b.get_changed_to(), ItemId(10));

    assert!(a.swap(&mut b, &rules.nets));
    assert_eq!(a.hdr.net_nos, vec![1]);
    assert_eq!(b.hdr.net_nos, vec![2]);
    assert_eq!(a.get_changed_to(), ItemId(10));
    assert_eq!(b.get_changed_to(), ItemId(11));
}

#[test]
fn pin_names_come_from_the_component_package() {
    let library = library();
    let rules = rules();
    let mut components = Components::new();
    components.add_with_generated_name(Some(Point::new(0, 0)), 0.0, true, 1);
    let ctx = ItemCtx {
        library: &library,
        components: &components,
        rules: &rules,
        bounding_box: &BOARD_BOUNDING_BOX,
        max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
    };
    let pin = Pin::new(hdr(1, vec![], 1), 1);
    assert_eq!(pin.name(&ctx), Some("2"));
    assert_eq!(pin.get_pin_index(), 1);
    assert_eq!(Item::Pin(pin).to_string(), "pin #1 of component #1");
}

#[test]
fn a_pin_on_more_than_one_net_refuses_to_swap() {
    let rules = rules();
    let mut a = Pin::new(hdr(10, vec![1, 2], 1), 0);
    let mut b = Pin::new(hdr(11, vec![3], 1), 1);
    assert!(!a.swap(&mut b, &rules.nets));
    assert_eq!(a.hdr.net_nos, vec![1, 2]);
    assert_eq!(b.hdr.net_nos, vec![3]);
}

fn smd_exit_fixture(pin_edge_to_turn_dist: f64) -> (BoardLibrary, Components, BoardRules, Pin) {
    let library = library();
    let mut rules = rules();
    rules.set_pin_edge_to_turn_dist(pin_edge_to_turn_dist);
    let mut components = Components::new();
    components.add_with_generated_name(Some(Point::new(1000, 2000)), 0.0, true, 2);
    (library, components, rules, Pin::new(hdr(1, vec![5], 1), 0))
}

#[test]
fn nearest_trace_exit_corner_picks_the_closest_offset_pad_exit() {
    let (library, components, rules, pin) = smd_exit_fixture(10.0);
    let ctx = ItemCtx {
        library: &library,
        components: &components,
        rules: &rules,
        bounding_box: &BOARD_BOUNDING_BOX,
        max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
    };
    for (from, expected) in [
        (
            FloatPoint::new(2000.0, 2000.0),
            FloatPoint::new(1115.0, 2000.0),
        ),
        (FloatPoint::new(0.0, 2000.0), FloatPoint::new(885.0, 2000.0)),
        (
            FloatPoint::new(1000.0, 5000.0),
            FloatPoint::new(1000.0, 2065.0),
        ),
        (
            FloatPoint::new(1000.0, -5000.0),
            FloatPoint::new(1000.0, 1935.0),
        ),
    ] {
        assert_eq!(
            pin.nearest_trace_exit_corner(&from, 5, 0, &ctx),
            Some(expected),
            "from {from:?}"
        );
    }
}

#[test]
fn a_negative_pin_edge_to_turn_dist_disables_both_exit_corner_helpers() {
    let (library, components, rules, pin) = smd_exit_fixture(-1.0);
    let ctx = ItemCtx {
        library: &library,
        components: &components,
        rules: &rules,
        bounding_box: &BOARD_BOUNDING_BOX,
        max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
    };
    assert_eq!(
        pin.nearest_trace_exit_corner(&FloatPoint::new(2000.0, 2000.0), 5, 0, &ctx),
        None
    );
    let polyline = Polyline::from_points(&[
        Point::new(1000, 2000),
        Point::new(3000, 2000),
        Point::new(3000, 4000),
    ]);
    assert_eq!(
        pin.calc_nearest_exit_restriction_direction(&polyline, 5, 0, &ctx),
        None
    );
}

#[test]
fn calc_nearest_exit_restriction_direction_follows_where_the_trace_leaves_the_pad() {
    let (library, components, rules, pin) = smd_exit_fixture(10.0);
    let ctx = ItemCtx {
        library: &library,
        components: &components,
        rules: &rules,
        bounding_box: &BOARD_BOUNDING_BOX,
        max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
    };
    let east = Polyline::from_points(&[
        Point::new(1000, 2000),
        Point::new(3000, 2000),
        Point::new(3000, 4000),
    ]);
    assert_eq!(
        pin.calc_nearest_exit_restriction_direction(&east, 5, 0, &ctx),
        Some(Direction::Int(IntDirection::RIGHT))
    );
    let north = Polyline::from_points(&[
        Point::new(1000, 2000),
        Point::new(1000, 4000),
        Point::new(3000, 4000),
    ]);
    assert_eq!(
        pin.calc_nearest_exit_restriction_direction(&north, 5, 0, &ctx),
        Some(Direction::Int(IntDirection::UP))
    );
}

#[test]
fn pin_transforms_throw_the_centre_away_instead_of_moving_it() {
    let library = library();
    let rules = rules();
    let mut components = Components::new();
    components.add_with_generated_name(Some(Point::new(1000, 2000)), 0.0, true, 1);
    let ctx = ItemCtx {
        library: &library,
        components: &components,
        rules: &rules,
        bounding_box: &BOARD_BOUNDING_BOX,
        max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
    };
    let mut pin = Pin::new(hdr(1, vec![2], 1), 0);
    assert_eq!(pin.get_center(&ctx), Point::new(1030, 2000));
    assert!(pin.drill.raw_center().is_some());

    pin.turn_90_degree(1, &IntPoint::ZERO);
    assert!(pin.drill.raw_center().is_none());
    assert_eq!(pin.get_center(&ctx), Point::new(1030, 2000));

    pin.rotate_approx(37.0, &FloatPoint::ZERO);
    assert!(pin.drill.raw_center().is_none());
    pin.change_placement_side(&IntPoint::ZERO);
    assert!(pin.drill.raw_center().is_none());

    assert_eq!(pin.get_center(&ctx), Point::new(1030, 2000));
    pin.translate_by(&Vector::new(5, 7));
    assert!(pin.drill.raw_center().is_none());
    assert_eq!(pin.get_center(&ctx), Point::new(1030, 2000));
}

#[test]
fn get_trace_connection_shape_is_the_centre_point() {
    let library = library();
    let rules = rules();
    let mut components = Components::new();
    components.add_with_generated_name(Some(Point::new(1000, 2000)), 0.0, true, 1);
    let ctx = ItemCtx {
        library: &library,
        components: &components,
        rules: &rules,
        bounding_box: &BOARD_BOUNDING_BOX,
        max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
    };
    let pin = Pin::new(hdr(1, vec![2], 1), 0);
    assert_eq!(
        pin.get_trace_connection_shape(TreeId(0), 0, &ctx),
        Some(TileShape::Box(IntBox::from_coords(1030, 2000, 1030, 2000)))
    );
}

#[test]
fn shape_layer_clamps_the_index_into_the_pin_layer_range() {
    let library = library();
    let rules = rules();
    let mut components = Components::new();
    components.add_with_generated_name(Some(Point::new(1000, 2000)), 0.0, true, 1);
    let ctx = ItemCtx {
        library: &library,
        components: &components,
        rules: &rules,
        bounding_box: &BOARD_BOUNDING_BOX,
        max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
    };
    let pin = Pin::new(hdr(1, vec![2], 1), 0);
    assert_eq!(pin.shape_layer(0, &ctx), 0);
    assert_eq!(pin.shape_layer(1, &ctx), 1);
    assert_eq!(pin.shape_layer(99, &ctx), 1);
}

#[test]
fn item_dispatch_reaches_the_drill_item_bodies() {
    let library = library();
    let rules = rules();
    let mut components = Components::new();
    components.add_with_generated_name(Some(Point::new(1000, 2000)), 0.0, true, 1);
    let ctx = ItemCtx {
        library: &library,
        components: &components,
        rules: &rules,
        bounding_box: &BOARD_BOUNDING_BOX,
        max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
    };
    let pin = Item::Pin(Pin::new(hdr(1, vec![2], 1), 0));
    let via = Item::Via(Via::new(
        hdr(2, vec![2], 0),
        VIA_PADSTACK,
        Point::new(0, 0),
        true,
    ));

    assert_eq!((pin.first_layer(&ctx), pin.last_layer(&ctx)), (0, 1));
    assert_eq!((via.first_layer(&ctx), via.last_layer(&ctx)), (0, 0));
    assert!(pin.is_on_layer(1, &ctx));
    assert!(!via.is_on_layer(1, &ctx));
    assert_eq!(pin.shape_layer(5, &ctx), 1);
    assert!(pin.shares_layer(&via, &ctx));
    assert_eq!(pin.first_common_layer(&via, &ctx), Some(0));
    assert_eq!(pin.last_common_layer(&via, &ctx), Some(0));
    assert_eq!(pin.tile_shape_count(&ctx), 2);
    assert_eq!(via.tile_shape_count(&ctx), 1);
    assert_eq!(
        via.bounding_box(&ctx),
        IntBox::from_coords(-30, -30, 30, 30)
    );
}

#[test]
fn item_clear_derived_data_drops_the_drill_caches() {
    let library = library();
    let rules = rules();
    let components = Components::new();
    let ctx = ItemCtx {
        library: &library,
        components: &components,
        rules: &rules,
        bounding_box: &BOARD_BOUNDING_BOX,
        max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
    };
    let mut via = Item::Via(Via::new(
        hdr(1, vec![2], 0),
        THT_PADSTACK,
        Point::new(0, 0),
        false,
    ));
    assert_eq!(via.first_layer(&ctx), 0);
    via.clear_derived_data();
    let Item::Via(v) = &via else { unreachable!() };
    assert_eq!(
        v.get_shape(0, &ctx).map(|s| s.bounding_box()),
        Some(IntBox::from_coords(-10, -20, 10, 20))
    );
    via.clear_autoroute_info();
    assert!(via.get_autoroute_info_pur().is_none());
}
