#[path = "board_builder.rs"]
mod board_builder;

use board_builder::{BoardFixture, WIDE_CLEARANCE_CLASS};
use copper_board::prelude::*;
use copper_geometry::{Area, IntBox, IntOctagon, Point, Polyline, Shape, TileShape, Vector};

fn bx(llx: i32, lly: i32, urx: i32, ury: i32) -> TileShape {
    TileShape::Box(IntBox::from_coords(llx, lly, urx, ury))
}

#[allow(clippy::too_many_arguments)]
fn oct(lx: i32, by: i32, rx: i32, ty: i32, ulx: i32, lrx: i32, llx: i32, urx: i32) -> TileShape {
    TileShape::Octagon(IntOctagon::new(lx, by, rx, ty, ulx, lrx, llx, urx))
}

fn leaves(tree: &ShapeSearchTree) -> Vec<(u32, usize, TileShape)> {
    tree.tree()
        .to_array()
        .into_iter()
        .map(|leaf| {
            let entry = tree.tree().leaf_entry(leaf);
            let TreeObject::Item(ItemId(id)) = entry.object else {
                panic!("the fixture stores no expansion rooms")
            };
            (
                id,
                entry.shape_index,
                tree.tree().leaf_bounds(leaf).to_tile_shape(),
            )
        })
        .collect()
}

fn shapes(f: &BoardFixture, tree: TreeId, id: u32) -> Vec<Option<TileShape>> {
    let item = &f.items[&ItemId(id)];
    (0..item.tree_shape_count(tree))
        .map(|i| item.get_tree_shape(tree, i).cloned())
        .collect()
}

fn pairs(entries: &[TreeEntry<TreeObject>]) -> Vec<(u32, usize)> {
    entries
        .iter()
        .map(|entry| {
            let TreeObject::Item(ItemId(id)) = entry.object else {
                panic!("the fixture stores no expansion rooms")
            };
            (id, entry.shape_index)
        })
        .collect()
}

fn object_ids(objects: &std::collections::BTreeSet<TreeObject>) -> Vec<u32> {
    objects
        .iter()
        .map(|object| match object {
            TreeObject::Item(ItemId(id)) => *id,
            TreeObject::Room(_) => panic!("the fixture stores no expansion rooms"),
        })
        .collect()
}

fn probe() -> TileShape {
    bx(-600, -100, 600, 500)
}

#[test]
fn tree_keys_reproduce_the_java_strings() {
    assert_eq!(
        ShapeSearchTree::new(TreeId(0), AngleRestriction::None, 0).get_key(),
        "ShapeSearchTree_FortyfiveDegree_cc0"
    );
    assert_eq!(
        ShapeSearchTree::new(TreeId(1), AngleRestriction::FortyFiveDegree, 1).get_key(),
        "ShapeSearchTree45Degree_FortyfiveDegree_cc1"
    );
    assert_eq!(
        ShapeSearchTree::new(TreeId(2), AngleRestriction::NinetyDegree, 1).get_key(),
        "ShapeSearchTree90Degree_Orthogonal_cc1"
    );
    assert_eq!(
        ShapeSearchTree::new(TreeId(3), AngleRestriction::FortyFiveDegree, 2).to_string(),
        "ShapeSearchTree45Degree_FortyfiveDegree_cc2"
    );
}

#[test]
fn the_default_tree_is_the_base_class_whatever_the_board_angle_is() {
    for angle in [
        AngleRestriction::None,
        AngleRestriction::FortyFiveDegree,
        AngleRestriction::NinetyDegree,
    ] {
        let f = BoardFixture::with_angle(angle);
        assert_eq!(
            f.manager.get_default_tree().get_key(),
            "ShapeSearchTree_FortyfiveDegree_cc0"
        );
        assert!(!f.manager.is_clearance_compensation_used());
    }
}

#[test]
fn clearance_compensation_values_match_the_jvm() {
    let mut f = BoardFixture::new();
    f.insert_all();
    let default_id = f.manager.get_default_tree().id();
    let auto1 = f.build_autoroute_tree(1);
    let auto2 = f.build_autoroute_tree(2);

    for cc in 0..=2 {
        assert_eq!(
            f.tree(default_id)
                .clearance_compensation_value(cc, 0, &f.rules),
            0,
            "an uncompensated tree never inflates (cc={cc})"
        );
    }
    assert_eq!(
        f.tree(auto1).clearance_compensation_value(1, 0, &f.rules),
        100
    );
    assert_eq!(
        f.tree(auto1).clearance_compensation_value(2, 0, &f.rules),
        500
    );
    assert_eq!(
        f.tree(auto1).clearance_compensation_value(0, 0, &f.rules),
        0
    );
    assert_eq!(
        f.tree(auto2).clearance_compensation_value(1, 0, &f.rules),
        0
    );
    assert_eq!(
        f.tree(auto2).clearance_compensation_value(2, 0, &f.rules),
        400
    );

    let Item::Trace(trace) = &f.items[&ItemId(4)] else {
        panic!("item 4 is the signal trace")
    };
    assert_eq!(
        f.tree(default_id).compensated_half_width(trace, &f.rules),
        30
    );
    assert_eq!(f.tree(auto1).compensated_half_width(trace, &f.rules), 130);
}

#[test]
fn the_default_tree_stores_what_the_jvm_stores() {
    let mut f = BoardFixture::new();
    f.insert_all();
    let id = f.manager.get_default_tree().id();
    let tree = f.tree(id);
    assert_eq!(tree.size(), 8);

    let pin2 = oct(-550, -50, -450, 50, -600, -400, -600, -400);
    let pin3 = oct(430, -70, 570, 70, 360, 640, 360, 640);
    let trace4 = [
        oct(-530, -30, 30, 30, -542, 42, -542, 42),
        oct(-30, -30, 30, 430, -442, 42, -42, 442),
        oct(-30, 370, 530, 430, -442, 142, 358, 942),
    ];
    let trace5 = [
        oct(-840, 260, -760, 940, -1757, -1043, -557, 157),
        oct(-840, 860, 340, 940, -1757, -543, 43, 1257),
    ];

    assert_eq!(
        leaves(tree),
        vec![
            (2, 0, pin2.clone()),
            (5, 0, trace5[0].clone()),
            (5, 1, trace5[1].clone()),
            (4, 0, trace4[0].clone()),
            (4, 1, trace4[1].clone()),
            (4, 2, trace4[2].clone()),
            (3, 0, pin3.clone()),
            (3, 1, pin3.clone()),
        ]
    );

    assert_eq!(shapes(&f, id, 1), Vec::<Option<TileShape>>::new());
    assert_eq!(shapes(&f, id, 2), vec![Some(pin2)]);
    assert_eq!(shapes(&f, id, 3), vec![Some(pin3.clone()), Some(pin3)]);
    assert_eq!(
        shapes(&f, id, 4),
        trace4.into_iter().map(Some).collect::<Vec<_>>()
    );
    assert_eq!(
        shapes(&f, id, 5),
        trace5.into_iter().map(Some).collect::<Vec<_>>()
    );
}

#[test]
fn a_zero_shape_insert_still_clears_the_entry_array() {
    let mut f = BoardFixture::new();
    f.insert_all();
    let tree_id = f.manager.get_default_tree().id();
    let stale_entries = f.items[&ItemId(4)]
        .get_search_tree_entries(tree_id)
        .expect("the trace was inserted")
        .to_vec();
    assert!(!stale_entries.is_empty());

    let mut outline = Item::ComponentOutline(ComponentOutline::new(
        ItemHeader::new(ItemId(99), Vec::new(), 0, 0, FixedState::Unfixed),
        Area::Shape(Shape::Tile(bx(0, 0, 10, 10))),
        true,
        Vector::ZERO,
        0.0,
        false,
        false,
        true,
    ));
    outline.set_tree_entries(tree_id, stale_entries);
    let ctx = ItemCtx {
        library: &f.library,
        components: &f.components,
        rules: &f.rules,
        bounding_box: &f.bounding_box,
        max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
    };

    f.manager.insert(&mut outline, &ctx);

    assert_eq!(
        outline.get_search_tree_entries(tree_id),
        Some([].as_slice())
    );
}

#[test]
fn a_45_degree_autoroute_tree_is_octagon_keyed_and_a_90_degree_one_is_box_keyed() {
    let mut f45 = BoardFixture::with_angle(AngleRestriction::FortyFiveDegree);
    f45.insert_all();
    let id45 = f45.build_autoroute_tree(1);
    let tree45 = f45.tree(id45);
    assert_eq!(
        tree45.get_key(),
        "ShapeSearchTree45Degree_FortyfiveDegree_cc1"
    );
    assert!(
        leaves(tree45)
            .iter()
            .all(|(_, _, bounds)| matches!(bounds, TileShape::Octagon(_))),
        "a 45-degree tree keys every leaf on an IntOctagon"
    );
    assert_eq!(
        leaves(tree45),
        vec![
            (5, 0, oct(-1340, -240, -260, 1440, -2464, -336, -1264, 864)),
            (2, 0, oct(-650, -150, -350, 150, -800, -200, -800, -200)),
            (4, 0, oct(-630, -130, 130, 130, -684, 184, -684, 184)),
            (3, 0, oct(330, -170, 670, 170, 160, 840, 160, 840)),
            (3, 1, oct(330, -170, 670, 170, 160, 840, 160, 840)),
            (4, 1, oct(-130, -130, 130, 530, -584, 184, -184, 584)),
            (5, 1, oct(-1340, 360, 840, 1440, -2464, 164, -664, 1964)),
            (4, 2, oct(-130, 270, 630, 530, -584, 284, 216, 1084)),
        ]
    );

    let mut f90 = BoardFixture::with_angle(AngleRestriction::NinetyDegree);
    f90.insert_all();
    let id90 = f90.build_autoroute_tree(1);
    let tree90 = f90.tree(id90);
    assert_eq!(tree90.get_key(), "ShapeSearchTree90Degree_Orthogonal_cc1");
    assert!(
        leaves(tree90)
            .iter()
            .all(|(_, _, bounds)| matches!(bounds, TileShape::Box(_))),
        "a 90-degree tree keys every leaf on an IntBox"
    );
    assert_eq!(
        leaves(tree90),
        vec![
            (5, 0, bx(-1340, -240, -260, 1440)),
            (2, 0, bx(-650, -150, -350, 150)),
            (4, 0, bx(-630, -130, 130, 130)),
            (3, 0, bx(330, -170, 670, 170)),
            (3, 1, bx(330, -170, 670, 170)),
            (4, 1, bx(-130, -130, 130, 530)),
            (5, 1, bx(-1340, 360, 840, 1440)),
            (4, 2, bx(-130, 270, 630, 530)),
        ]
    );
    assert_eq!(
        shapes(&f90, id90, 4),
        vec![
            Some(bx(-630, -130, 130, 130)),
            Some(bx(-130, -130, 130, 530)),
            Some(bx(-130, 270, 630, 530)),
        ]
    );
}

#[test]
fn the_base_class_enlarges_where_the_45_degree_subclass_offsets() {
    let mut base = BoardFixture::with_angle(AngleRestriction::None);
    base.insert_all();
    let base_id = base.build_autoroute_tree(1);
    assert_eq!(
        base.tree(base_id).get_key(),
        "ShapeSearchTree_FortyfiveDegree_cc1"
    );
    assert_eq!(
        shapes(&base, base_id, 2),
        vec![Some(oct(-650, -150, -350, 150, -741, -259, -741, -259))]
    );
    assert_eq!(
        shapes(&base, base_id, 3),
        vec![
            Some(oct(330, -170, 670, 170, 219, 781, 219, 781)),
            Some(oct(330, -170, 670, 170, 219, 781, 219, 781)),
        ]
    );

    let mut f45 = BoardFixture::with_angle(AngleRestriction::FortyFiveDegree);
    f45.insert_all();
    let id45 = f45.build_autoroute_tree(1);
    assert_eq!(
        shapes(&f45, id45, 2),
        vec![Some(oct(-650, -150, -350, 150, -800, -200, -800, -200))]
    );
    assert_eq!(
        shapes(&f45, id45, 3),
        vec![
            Some(oct(330, -170, 670, 170, 160, 840, 160, 840)),
            Some(oct(330, -170, 670, 170, 160, 840, 160, 840)),
        ]
    );
}

#[test]
fn overlapping_objects_and_entries_come_back_in_java_order() {
    let mut f = BoardFixture::new();
    f.insert_all();
    let id = f.manager.get_default_tree().id();
    let tree = f.tree(id);
    let ctx = f.ctx();

    let objects = tree.overlapping_objects(&probe(), Some(0), &[], &f.items, &ctx);
    assert_eq!(object_ids(&objects), vec![4, 3, 2]);
    let entries = tree.overlapping_tree_entries(&probe(), Some(0), &[], &f.items, &ctx);
    assert_eq!(
        pairs(&entries),
        vec![(4, 0), (4, 1), (4, 2), (3, 0), (2, 0)]
    );
}

#[test]
fn the_layer_filter_matches_java_including_the_ignored_layer() {
    let mut f = BoardFixture::new();
    f.insert_all();
    let id = f.manager.get_default_tree().id();
    let tree = f.tree(id);
    let ctx = f.ctx();

    let entries = tree.overlapping_tree_entries(&probe(), None, &[], &f.items, &ctx);
    assert_eq!(
        pairs(&entries),
        vec![(4, 0), (4, 1), (4, 2), (3, 0), (3, 1), (2, 0)]
    );
    let entries = tree.overlapping_tree_entries(&probe(), Some(1), &[], &f.items, &ctx);
    assert_eq!(pairs(&entries), vec![(3, 1)]);
    assert_eq!(
        object_ids(&tree.overlapping_objects(&probe(), Some(1), &[], &f.items, &ctx)),
        vec![3]
    );
}

#[test]
fn ignore_net_nos_drops_every_item_that_sits_on_one_of_them() {
    let mut f = BoardFixture::new();
    f.insert_all();
    let id = f.manager.get_default_tree().id();
    let tree = f.tree(id);
    let ctx = f.ctx();
    let mut counter = 0;

    assert!(
        tree.overlapping_tree_entries(&probe(), Some(0), &[1], &f.items, &ctx)
            .is_empty()
    );
    assert!(
        tree.overlapping_objects(&probe(), Some(0), &[1], &f.items, &ctx)
            .is_empty()
    );
    let with_clearance = tree.overlapping_tree_entries_with_clearance(
        &probe(),
        Some(0),
        &[1],
        1,
        &f.items,
        &ctx,
        &mut counter,
    );
    assert_eq!(pairs(&with_clearance), vec![(5, 0)]);
    assert_eq!(
        tree.overlapping_items_with_clearance(
            &probe(),
            Some(0),
            &[1],
            1,
            &f.items,
            &ctx,
            &mut counter
        ),
        vec![ItemId(5)]
    );
}

#[test]
fn overlapping_tree_entries_with_clearance_reproduces_the_1_2x_walk() {
    let mut f = BoardFixture::new();
    f.insert_all();
    let id = f.manager.get_default_tree().id();
    let tree = f.tree(id);
    let ctx = f.ctx();
    let mut counter = 0;

    let entries = tree.overlapping_tree_entries_with_clearance(
        &probe(),
        Some(0),
        &[],
        1,
        &f.items,
        &ctx,
        &mut counter,
    );
    assert_eq!(
        pairs(&entries),
        vec![(5, 0), (4, 0), (4, 1), (4, 2), (3, 0), (2, 0)]
    );

    let entries = tree.overlapping_tree_entries_with_clearance(
        &probe(),
        Some(0),
        &[],
        WIDE_CLEARANCE_CLASS,
        &f.items,
        &ctx,
        &mut counter,
    );
    assert_eq!(
        pairs(&entries),
        vec![(4, 0), (4, 1), (4, 2), (3, 0), (2, 0), (5, 0), (5, 1)]
    );

    assert_eq!(
        tree.overlapping_items_with_clearance(
            &probe(),
            Some(0),
            &[],
            1,
            &f.items,
            &ctx,
            &mut counter
        ),
        vec![ItemId(5), ItemId(4), ItemId(3), ItemId(2)]
    );
}

#[test]
fn an_ignored_layer_makes_every_clearance_zero() {
    let mut f = BoardFixture::new();
    f.insert_all();
    let id = f.manager.get_default_tree().id();
    let tree = f.tree(id);
    let ctx = f.ctx();
    let mut counter = 0;

    let entries = tree.overlapping_tree_entries_with_clearance(
        &probe(),
        None,
        &[],
        1,
        &f.items,
        &ctx,
        &mut counter,
    );
    assert_eq!(
        pairs(&entries),
        vec![(4, 0), (4, 1), (4, 2), (3, 0), (3, 1), (2, 0)]
    );
}

#[test]
fn a_compensated_tree_answers_the_plain_overlap_query() {
    let mut f = BoardFixture::new();
    f.insert_all();
    let id = f.build_autoroute_tree(1);
    let tree = f.tree(id);
    let ctx = f.ctx();
    let mut counter = 0;

    assert!(tree.is_clearance_compensation_used());
    let plain = tree.overlapping_tree_entries(&probe(), Some(0), &[], &f.items, &ctx);
    let auto = tree.overlapping_tree_entries_with_clearance_auto(
        &probe(),
        Some(0),
        &[],
        1,
        &f.items,
        &ctx,
        &mut counter,
    );
    assert_eq!(
        pairs(&plain),
        vec![(5, 0), (5, 1), (4, 0), (4, 1), (4, 2), (3, 0), (2, 0)]
    );
    assert_eq!(pairs(&plain), pairs(&auto));
    assert_eq!(
        counter, 0,
        "the compensated branch mints no EntrySortedByClearance ids"
    );
    assert_eq!(
        object_ids(&tree.overlapping_objects(&probe(), Some(0), &[], &f.items, &ctx)),
        vec![5, 4, 3, 2]
    );
}

#[test]
fn a_small_probe_reaches_only_what_it_touches() {
    let mut f = BoardFixture::new();
    f.insert_all();
    let id = f.manager.get_default_tree().id();
    let tree = f.tree(id);
    let ctx = f.ctx();
    let mut counter = 0;
    let small = bx(-520, -20, -480, 20);

    assert_eq!(
        pairs(&tree.overlapping_tree_entries(&small, Some(0), &[], &f.items, &ctx)),
        vec![(4, 0), (2, 0)]
    );
    assert_eq!(
        pairs(&tree.overlapping_tree_entries_with_clearance(
            &small,
            Some(0),
            &[],
            1,
            &f.items,
            &ctx,
            &mut counter
        )),
        vec![(4, 0), (2, 0)]
    );
    assert!(
        tree.overlapping_tree_entries_with_clearance(
            &small,
            Some(0),
            &[1],
            1,
            &f.items,
            &ctx,
            &mut counter
        )
        .is_empty()
    );
}

#[test]
fn the_entry_id_counter_only_ever_grows() {
    let mut f = BoardFixture::new();
    f.insert_all();
    let id = f.manager.get_default_tree().id();
    let ctx_owner = BoardFixture::new();
    let ctx = ctx_owner.ctx();

    let mut counter = 0;
    let first = f.tree(id).overlapping_tree_entries_with_clearance(
        &probe(),
        Some(0),
        &[],
        1,
        &f.items,
        &ctx,
        &mut counter,
    );
    let after_first = counter;
    assert!(after_first >= first.len() as u64);
    let second = f.tree(id).overlapping_tree_entries_with_clearance(
        &probe(),
        Some(0),
        &[],
        1,
        &f.items,
        &ctx,
        &mut counter,
    );
    assert!(
        counter > after_first,
        "the second query must mint fresh ids, not reuse the first query's"
    );
    assert_eq!(
        pairs(&first),
        pairs(&second),
        "a monotonic counter keeps the answer stable across queries"
    );
}

#[test]
fn change_item_shape_replaces_one_leaf_and_leaves_the_rest_alone() {
    let mut f = BoardFixture::new();
    f.insert_all();
    let id = f.manager.get_default_tree().id();

    let mut item = f.items.remove(&ItemId(4)).expect("item 4");
    f.manager
        .get_default_tree_mut()
        .change_item_shape(&mut item, 1, bx(-20, -20, 20, 420));
    f.items.insert(ItemId(4), item);

    assert_eq!(
        shapes(&f, id, 4),
        vec![
            Some(oct(-530, -30, 30, 30, -542, 42, -542, 42)),
            Some(bx(-20, -20, 20, 420)),
            Some(oct(-30, 370, 530, 430, -442, 142, 358, 942)),
        ]
    );
    let observed: Vec<(u32, usize)> = leaves(f.tree(id))
        .into_iter()
        .map(|(id, index, _)| (id, index))
        .collect();
    assert_eq!(
        observed,
        vec![
            (2, 0),
            (5, 0),
            (5, 1),
            (4, 0),
            (4, 1),
            (4, 2),
            (3, 0),
            (3, 1)
        ]
    );
    assert_eq!(
        leaves(f.tree(id))[4].2,
        oct(-20, -20, 20, 420, -440, 40, -40, 440)
    );
    assert!(f.manager.validate_entries(&f.items[&ItemId(4)]));
}

#[test]
fn change_entries_rebuilds_only_the_middle_of_a_trace() {
    let mut f = BoardFixture::new();
    f.insert_all();
    let id = f.manager.get_default_tree().id();

    let shifted = Polyline::from_points(&[
        Point::new(-500, 0),
        Point::new(0, 0),
        Point::new(0, 600),
        Point::new(500, 600),
    ]);
    let mut item = f.items.remove(&ItemId(4)).expect("item 4");
    {
        let Item::Trace(trace) = &mut item else {
            panic!("item 4 is the signal trace")
        };
        let rules = &f.rules;
        f.manager
            .get_default_tree_mut()
            .change_entries(trace, &shifted, 1, 1, rules);
    }
    f.items.insert(ItemId(4), item);

    assert_eq!(
        shapes(&f, id, 4),
        vec![
            Some(oct(-530, -30, 30, 30, -542, 42, -542, 42)),
            Some(oct(-30, -30, 30, 630, -642, 42, -42, 642)),
            Some(oct(-30, 370, 530, 430, -442, 142, 358, 942)),
        ]
    );
    let observed: Vec<(u32, usize)> = leaves(f.tree(id))
        .into_iter()
        .map(|(id, index, _)| (id, index))
        .collect();
    assert_eq!(
        observed,
        vec![
            (2, 0),
            (5, 0),
            (5, 1),
            (4, 0),
            (4, 1),
            (4, 2),
            (3, 0),
            (3, 1)
        ]
    );
    assert!(f.manager.validate_entries(&f.items[&ItemId(4)]));
}

#[test]
#[should_panic(expected = "keepAtEndCount (4) exceeds oldShapeCount (3)")]
fn change_entries_skips_the_removal_loop_when_more_tail_is_kept_than_exists() {
    let mut f = BoardFixture::new();
    f.insert_all();

    let shifted = Polyline::from_points(&[
        Point::new(-500, 0),
        Point::new(0, 0),
        Point::new(0, 600),
        Point::new(500, 600),
    ]);
    let mut item = f.items.remove(&ItemId(4)).expect("item 4");
    let Item::Trace(trace) = &mut item else {
        panic!("item 4 is the signal trace")
    };
    let rules = &f.rules;
    f.manager
        .get_default_tree_mut()
        .change_entries(trace, &shifted, 0, 4, rules);
}

#[test]
fn insert_and_remove_track_on_the_board_and_the_entry_arrays() {
    let mut f = BoardFixture::new();
    assert!(f.items.values().all(|item| !item.is_on_the_board()));
    f.insert_all();
    assert!(f.items.values().all(Item::is_on_the_board));
    let id = f.manager.get_default_tree().id();
    assert_eq!(f.tree(id).size(), 8);

    let mut item = f.items.remove(&ItemId(4)).expect("item 4");
    f.manager.remove(&mut item);
    assert!(!item.is_on_the_board());
    assert!(item.get_search_tree_entries(id).is_none());
    assert_eq!(item.tree_shape_count(id), 0);
    f.items.insert(ItemId(4), item);
    assert_eq!(f.tree(id).size(), 5);

    let mut item = f.items.remove(&ItemId(4)).expect("item 4");
    f.manager.remove(&mut item);
    f.items.insert(ItemId(4), item);
    assert_eq!(f.tree(id).size(), 5);
}

#[test]
fn get_autoroute_tree_reuses_a_tree_with_the_same_compensated_class() {
    let mut f = BoardFixture::new();
    f.insert_all();
    let default_id = f.manager.get_default_tree().id();
    assert_eq!(f.build_autoroute_tree(0), default_id);

    let first = f.build_autoroute_tree(1);
    assert_ne!(first, default_id);
    assert_eq!(f.build_autoroute_tree(1), first);
    let second = f.build_autoroute_tree(2);
    assert_ne!(second, first);
    assert_eq!(
        f.tree(second).get_key(),
        "ShapeSearchTree45Degree_FortyfiveDegree_cc2"
    );

    f.manager.reset_compensated_trees();
    assert_eq!(f.manager.trees().count(), 1);
    assert_eq!(f.manager.get_default_tree().id(), default_id);
}

#[test]
fn clearance_class_removed_refuses_to_drop_the_default_tree() {
    let mut f = BoardFixture::new();
    f.insert_all();
    let auto = f.build_autoroute_tree(1);
    assert_eq!(f.manager.trees().count(), 2);

    f.manager.clearance_class_removed(0);
    assert_eq!(f.manager.trees().count(), 2);

    f.manager.clearance_class_removed(1);
    assert_eq!(f.manager.trees().count(), 1);
    assert!(f.manager.trees().all(|tree| tree.id() != auto));
}

#[test]
fn set_clearance_compensation_used_rebuilds_the_default_tree_as_the_base_class() {
    let mut f = BoardFixture::new();
    f.insert_all();
    f.build_autoroute_tree(1);
    assert_eq!(f.manager.trees().count(), 2);

    let mut items = std::mem::take(&mut f.items);
    let ctx = ItemCtx {
        library: &f.library,
        components: &f.components,
        rules: &f.rules,
        bounding_box: &f.bounding_box,
        max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
    };
    let mut refs: Vec<&mut Item> = items.values_mut().rev().collect();
    f.manager
        .set_clearance_compensation_used(true, &mut refs, &ctx);
    drop(refs);
    f.items = items;

    assert!(f.manager.is_clearance_compensation_used());
    assert_eq!(f.manager.trees().count(), 1);
    let id = f.manager.get_default_tree().id();
    assert_eq!(
        f.manager.get_default_tree().get_key(),
        "ShapeSearchTree_FortyfiveDegree_cc1"
    );
    assert_eq!(f.tree(id).size(), 8);
    let observed: Vec<(u32, usize)> = leaves(f.tree(id))
        .into_iter()
        .map(|(id, index, _)| (id, index))
        .collect();
    assert_eq!(
        observed,
        vec![
            (5, 0),
            (2, 0),
            (4, 0),
            (3, 0),
            (3, 1),
            (4, 1),
            (5, 1),
            (4, 2)
        ]
    );
    assert_eq!(
        shapes(&f, id, 2),
        vec![Some(oct(-650, -150, -350, 150, -741, -259, -741, -259))]
    );
    assert_eq!(
        shapes(&f, id, 3),
        vec![
            Some(oct(330, -170, 670, 170, 219, 781, 219, 781)),
            Some(oct(330, -170, 670, 170, 219, 781, 219, 781)),
        ]
    );
    let ctx = f.ctx();
    let mut counter = 0;
    assert_eq!(
        pairs(&f.tree(id).overlapping_tree_entries_with_clearance_auto(
            &probe(),
            Some(0),
            &[],
            1,
            &f.items,
            &ctx,
            &mut counter
        )),
        vec![(5, 0), (5, 1), (4, 0), (4, 1), (4, 2), (3, 0), (2, 0)]
    );

    let before = f.manager.get_default_tree().id();
    let mut items = std::mem::take(&mut f.items);
    let ctx = ItemCtx {
        library: &f.library,
        components: &f.components,
        rules: &f.rules,
        bounding_box: &f.bounding_box,
        max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
    };
    let mut refs: Vec<&mut Item> = items.values_mut().rev().collect();
    f.manager
        .set_clearance_compensation_used(true, &mut refs, &ctx);
    drop(refs);
    f.items = items;
    assert_eq!(f.manager.get_default_tree().id(), before);
}

#[test]
fn reinsert_tree_shapes_recomputes_after_a_rule_change() {
    let mut f = BoardFixture::new();
    f.insert_all();
    let id = f.manager.get_default_tree().id();
    let before = shapes(&f, id, 3);

    f.rules.set_hole_clearance(500);
    let mut items = std::mem::take(&mut f.items);
    let ctx = ItemCtx {
        library: &f.library,
        components: &f.components,
        rules: &f.rules,
        bounding_box: &f.bounding_box,
        max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
    };
    let mut refs: Vec<&mut Item> = items.values_mut().rev().collect();
    f.manager.reinsert_tree_shapes(&mut refs, &ctx);
    drop(refs);
    f.items = items;

    let after = shapes(&f, id, 3);
    assert_ne!(
        before, after,
        "a hole-clearance change must reach the tree shapes"
    );
    assert_eq!(f.tree(id).size(), 8);
    assert!(f.items.values().all(Item::is_on_the_board));
}

fn area_shapes(f: &board_builder::AreaFixture, tree: TreeId, id: u32) -> Vec<Option<TileShape>> {
    let item = &f.items[&ItemId(id)];
    (0..item.tree_shape_count(tree))
        .map(|i| item.get_tree_shape(tree, i).cloned())
        .collect()
}

#[test]
fn obstacle_and_conduction_areas_are_split_enlarged_and_regularised() {
    let mut f = board_builder::AreaFixture::new();
    let default_id = f.manager.get_default_tree().id();
    assert_eq!(
        area_shapes(&f, default_id, 3),
        vec![Some(bx(0, 1000, 1000, 2000)), Some(bx(0, 0, 2000, 1000))]
    );
    assert_eq!(
        area_shapes(&f, default_id, 4),
        vec![Some(bx(-2000, -1500, -1000, -500))]
    );

    let auto = f.build_autoroute_tree(1);
    assert_eq!(
        area_shapes(&f, auto, 3),
        vec![
            Some(oct(-100, 900, 1100, 2100, -2141, 141, 859, 3141)),
            Some(oct(-100, -100, 2100, 1100, -1141, 2141, -141, 3141)),
        ]
    );
    assert_eq!(
        area_shapes(&f, auto, 4),
        vec![Some(oct(
            -2100, -1600, -900, -400, -1641, 641, -3641, -1359
        ))]
    );
}

#[test]
fn a_board_outline_contributes_line_bands_per_layer_and_keepout_pieces_when_asked() {
    let mut f = board_builder::AreaFixture::new();
    let default_id = f.manager.get_default_tree().id();
    let bands = area_shapes(&f, default_id, 1);
    assert_eq!(bands.len(), 12);
    for i in 0..4 {
        assert_eq!(bands[i], bands[i + 4]);
        assert_eq!(bands[i], bands[i + 8]);
    }
    assert_eq!(
        bands[0],
        Some(oct(-3100, -2100, 3100, -1900, -1141, 5141, -5141, 1141))
    );
    assert_eq!(
        bands[3],
        Some(oct(-3100, -2100, -2900, 2100, -5141, -859, -5141, -859))
    );
    assert_eq!(f.tree(default_id).size(), 18);

    f.generate_keepout_outside();
    let keepout = area_shapes(&f, default_id, 1);
    assert_eq!(keepout.len(), 12);
    assert_eq!(
        keepout[0],
        Some(oct(-5000, -5000, 5000, -2000, -3000, 10000, -10000, 3000))
    );
    assert_eq!(
        keepout[2],
        Some(oct(3000, -2000, 5000, 2000, 1000, 7000, 1000, 7000))
    );
    assert_eq!(f.tree(default_id).size(), 18);
}

#[test]
fn a_drill_layer_without_a_pad_falls_back_to_the_synthesised_hole_obstacle() {
    let f = board_builder::AreaFixture::new();
    let default_id = f.manager.get_default_tree().id();
    assert_eq!(
        area_shapes(&f, default_id, 2),
        vec![
            Some(oct(854, -146, 1146, 146, 747, 1253, 747, 1253)),
            Some(oct(854, -146, 1146, 146, 794, 1207, 794, 1207)),
            Some(oct(854, -146, 1146, 146, 747, 1253, 747, 1253)),
        ]
    );
    assert_eq!(f.items[&ItemId(2)].tree_shape_count(default_id), 3);
}

#[test]
fn a_drill_layer_without_a_pad_has_no_leaf_when_hole_clearance_is_off() {
    let mut f = board_builder::AreaFixture::new();
    f.rules.set_hole_clearance(0);
    let mut items = std::mem::take(&mut f.items);
    let ctx = ItemCtx {
        library: &f.library,
        components: &f.components,
        rules: &f.rules,
        bounding_box: &f.bounding_box,
        max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
    };
    let mut refs: Vec<&mut Item> = items.values_mut().rev().collect();
    f.manager.reinsert_tree_shapes(&mut refs, &ctx);
    drop(refs);
    f.items = items;

    let default_id = f.manager.get_default_tree().id();
    let shapes = area_shapes(&f, default_id, 2);
    assert_eq!(shapes.len(), 3, "the empty layer keeps its slot");
    assert!(
        shapes[1].is_none(),
        "layer 1 has neither pad nor hole shape"
    );
    assert_eq!(
        f.items[&ItemId(2)]
            .get_search_tree_entries(default_id)
            .expect("entries")
            .iter()
            .filter(|leaf| leaf.is_some())
            .count(),
        2,
        "only the two pad layers get a leaf"
    );
    assert_eq!(
        shapes[0],
        Some(oct(920, -80, 1080, 80, 840, 1160, 840, 1160))
    );
}

fn joined_polyline() -> Polyline {
    Polyline::from_points(&[
        Point::new(-500, 0),
        Point::new(0, 0),
        Point::new(0, 400),
        Point::new(500, 400),
        Point::new(500, 900),
    ])
}

fn trace_shapes(f: &board_builder::TraceFixture, id: u32) -> Vec<Option<TileShape>> {
    let tree = f.tree().id();
    let item = &f.items[&ItemId(id)];
    (0..item.tree_shape_count(tree))
        .map(|i| item.get_tree_shape(tree, i).cloned())
        .collect()
}

#[test]
fn merge_entries_at_end_appends_the_source_trace_behind_the_link_shapes() {
    let mut f = board_builder::TraceFixture::new();
    assert_eq!(f.tree().size(), 4);
    let mut trace_a = f.take_trace(2);
    let mut trace_b = f.take_trace(3);
    let rules_snapshot = f.rules.clone();
    let tree = f.manager.get_default_tree_mut();
    tree.merge_entries_at_end(
        &mut trace_b,
        &mut trace_a,
        &joined_polyline(),
        1,
        4,
        &rules_snapshot,
    );
    f.put_trace(2, trace_a);
    f.put_trace(3, trace_b);

    assert_eq!(f.tree().size(), 4);
    assert_eq!(
        leaves(f.tree())
            .into_iter()
            .map(|(id, index, _)| (id, index))
            .collect::<Vec<_>>(),
        vec![(2, 0), (2, 1), (2, 2), (2, 3)],
        "every leaf now belongs to the target trace"
    );
    assert_eq!(
        trace_shapes(&f, 2),
        vec![
            Some(oct(-530, -30, 30, 30, -542, 42, -542, 42)),
            Some(oct(-30, -30, 30, 430, -442, 42, -42, 442)),
            Some(oct(-30, 370, 530, 430, -442, 142, 358, 942)),
            Some(oct(470, 370, 530, 930, -442, 142, 858, 1442)),
        ]
    );
    assert_eq!(trace_shapes(&f, 3).len(), 2);
    assert!(f.manager.validate_entries(&f.items[&ItemId(2)]));
}

fn change_order_joined_polyline() -> copper_geometry::Polyline {
    copper_geometry::Polyline::from_points(&[
        Point::new(-500, -500),
        Point::new(-500, 0),
        Point::new(0, 0),
        Point::new(500, 0),
        Point::new(500, 500),
    ])
}

#[test]
fn merge_entries_in_front_reverses_the_source_trace_when_the_two_meet_head_to_head() {
    let mut f = board_builder::TraceFixture::merge_pair(
        &[
            Point::new(0, 0),
            Point::new(-500, 0),
            Point::new(-500, -500),
        ],
        &[Point::new(0, 0), Point::new(500, 0), Point::new(500, 500)],
    );
    assert_eq!(f.tree().size(), 4);
    let mut trace_a = f.take_trace(2);
    let mut trace_b = f.take_trace(3);
    assert_eq!(trace_a.first_corner(), trace_b.first_corner());
    let rules_snapshot = f.rules.clone();
    f.manager.get_default_tree_mut().merge_entries_in_front(
        &mut trace_a,
        &mut trace_b,
        &change_order_joined_polyline(),
        1,
        4,
        &rules_snapshot,
    );
    f.put_trace(2, trace_a);
    f.put_trace(3, trace_b);

    assert_eq!(f.tree().size(), 4);
    assert_eq!(
        leaves(f.tree())
            .into_iter()
            .map(|(id, index, _)| (id, index))
            .collect::<Vec<_>>(),
        vec![(3, 3), (3, 2), (3, 1), (3, 0)],
        "every leaf now belongs to the target trace"
    );
    assert_eq!(
        trace_shapes(&f, 3),
        vec![
            Some(oct(-530, -530, -470, 30, -542, 42, -1042, -458)),
            Some(oct(-530, -30, 530, 30, -542, 542, -542, 542)),
            Some(oct(470, -30, 530, 530, -42, 542, 458, 1042)),
            Some(oct(470, -30, 530, 530, -42, 542, 458, 1042)),
        ]
    );
    assert!(f.manager.validate_entries(&f.items[&ItemId(3)]));
}

#[test]
fn merge_entries_at_end_reverses_the_source_trace_when_the_two_meet_tail_to_tail() {
    let mut f = board_builder::TraceFixture::merge_pair(
        &[
            Point::new(-500, -500),
            Point::new(-500, 0),
            Point::new(0, 0),
        ],
        &[Point::new(500, 500), Point::new(500, 0), Point::new(0, 0)],
    );
    let mut trace_a = f.take_trace(2);
    let mut trace_b = f.take_trace(3);
    assert_eq!(trace_a.last_corner(), trace_b.last_corner());
    let rules_snapshot = f.rules.clone();
    f.manager.get_default_tree_mut().merge_entries_at_end(
        &mut trace_a,
        &mut trace_b,
        &change_order_joined_polyline(),
        1,
        4,
        &rules_snapshot,
    );
    f.put_trace(2, trace_a);
    f.put_trace(3, trace_b);

    assert_eq!(f.tree().size(), 4);
    assert_eq!(
        leaves(f.tree())
            .into_iter()
            .map(|(id, index, _)| (id, index))
            .collect::<Vec<_>>(),
        vec![(3, 3), (3, 1), (3, 0), (3, 2)],
        "every leaf now belongs to the target trace"
    );
    assert_eq!(
        trace_shapes(&f, 3),
        vec![
            Some(oct(470, -30, 530, 530, -42, 542, 458, 1042)),
            Some(oct(-530, -30, 530, 30, -542, 542, -542, 542)),
            Some(oct(470, -30, 530, 530, -42, 542, 458, 1042)),
            Some(oct(-530, -530, -470, 30, -542, 42, -1042, -458)),
        ]
    );
    assert!(f.manager.validate_entries(&f.items[&ItemId(3)]));
}

#[test]
fn merge_entries_in_front_prepends_the_source_trace_before_the_link_shapes() {
    let mut f = board_builder::TraceFixture::new();
    let mut trace_a = f.take_trace(2);
    let mut trace_b = f.take_trace(3);
    let rules_snapshot = f.rules.clone();
    let tree = f.manager.get_default_tree_mut();
    tree.merge_entries_in_front(
        &mut trace_a,
        &mut trace_b,
        &joined_polyline(),
        1,
        4,
        &rules_snapshot,
    );
    f.put_trace(2, trace_a);
    f.put_trace(3, trace_b);

    assert_eq!(f.tree().size(), 4);
    assert_eq!(
        leaves(f.tree())
            .into_iter()
            .map(|(id, index, _)| (id, index))
            .collect::<Vec<_>>(),
        vec![(3, 0), (3, 1), (3, 2), (3, 3)]
    );
    assert_eq!(
        trace_shapes(&f, 3),
        vec![
            Some(oct(-530, -30, 30, 30, -542, 42, -542, 42)),
            Some(oct(-30, -30, 30, 430, -442, 42, -42, 442)),
            Some(oct(-30, 370, 530, 430, -442, 142, 358, 942)),
            Some(oct(470, 370, 530, 930, -442, 142, 858, 1442)),
        ]
    );
    assert!(f.manager.validate_entries(&f.items[&ItemId(3)]));
}

#[test]
fn reuse_entries_after_cutout_hands_the_ends_over_and_holes_the_source_array() {
    let mut f = board_builder::TraceFixture::new();
    let mut long_item = Item::Trace(board_builder::trace_piece(
        4,
        &[
            (-1000, -1000),
            (-1000, 0),
            (0, 0),
            (0, 1000),
            (1000, 1000),
            (1000, 2000),
        ],
    ));
    let ctx = ItemCtx {
        library: &f.library,
        components: &f.components,
        rules: &f.rules,
        bounding_box: &f.bounding_box,
        max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
    };
    f.manager.insert(&mut long_item, &ctx);
    assert_eq!(f.manager.get_default_tree().size(), 9);

    let Item::Trace(mut long_trace) = long_item else {
        unreachable!()
    };
    let mut start_piece = board_builder::trace_piece(5, &[(-1000, -1000), (-1000, 0), (0, 0)]);
    let mut end_piece = board_builder::trace_piece(6, &[(0, 1000), (1000, 1000), (1000, 2000)]);
    f.manager.get_default_tree_mut().reuse_entries_after_cutout(
        &mut long_trace,
        &mut start_piece,
        &mut end_piece,
        &ctx,
    );

    let tree_id = f.manager.get_default_tree().id();
    let from_entries = long_trace
        .hdr
        .get_tree_entries(tree_id)
        .expect("entries")
        .to_vec();
    assert_eq!(from_entries.len(), 5);
    assert!(
        from_entries[0].is_none(),
        "index 0 was handed to the start piece"
    );
    assert!(
        from_entries[4].is_none(),
        "index 4 was handed to the end piece"
    );
    assert!(from_entries[1..4].iter().all(Option::is_some));
    assert_eq!(
        start_piece
            .hdr
            .get_tree_entries(tree_id)
            .expect("entries")
            .len(),
        2
    );
    assert_eq!(
        end_piece
            .hdr
            .get_tree_entries(tree_id)
            .expect("entries")
            .len(),
        2
    );
    assert_eq!(f.manager.get_default_tree().size(), 11);
    assert!(
        f.manager
            .get_default_tree()
            .validate_entries(&Item::Trace(start_piece))
    );
    assert!(
        f.manager
            .get_default_tree()
            .validate_entries(&Item::Trace(end_piece))
    );
}

#[test]
fn reduce_trace_shape_at_tie_pin_cuts_the_pin_out_of_the_end_tile() {
    let mut f = BoardFixture::new();
    f.insert_all();
    let id = f.manager.get_default_tree().id();

    let Item::Pin(pin) = f.items[&ItemId(2)].clone() else {
        panic!("item 2 is the SMD pin")
    };
    let Item::Trace(mut trace) = f.items.remove(&ItemId(4)).expect("item 4") else {
        panic!("item 4 is the signal trace")
    };
    {
        let ctx = ItemCtx {
            library: &f.library,
            components: &f.components,
            rules: &f.rules,
            bounding_box: &f.bounding_box,
            max_tree_shape_width: DEFAULT_MAX_TREE_SHAPE_WIDTH,
        };
        f.manager
            .get_default_tree_mut()
            .reduce_trace_shape_at_tie_pin(&pin, &mut trace, &ctx);
    }
    f.items.insert(ItemId(4), Item::Trace(trace));

    assert_eq!(
        shapes(&f, id, 4),
        vec![
            Some(oct(-450, -30, 30, 30, -480, 42, -480, 42)),
            Some(oct(-30, -30, 30, 430, -442, 42, -42, 442)),
            Some(oct(-30, 370, 530, 430, -442, 142, 358, 942)),
        ]
    );
    assert_eq!(f.tree(id).size(), 8);
    assert!(f.manager.validate_entries(&f.items[&ItemId(4)]));
}

#[test]
fn the_45_degree_override_regularises_the_outline_line_bands_too() {
    let mut f = board_builder::TraceFixture::skewed_outline();
    let default_id = f.manager.get_default_tree().id();
    let Item::BoardOutline(board_outline) = &f.items[&ItemId(1)] else {
        unreachable!()
    };
    let pieces: usize = (0..3)
        .map(|i| board_outline.piece_cuts(0, i).len() + 1)
        .sum();
    let outline = &f.items[&ItemId(1)];
    assert_eq!(outline.tree_shape_count(default_id), 2 * pieces);
    assert!(
        (0..2 * pieces).all(|i| matches!(
            outline.get_tree_shape(default_id, i),
            Some(TileShape::Simplex(_))
        )),
        "the base tree keeps the raw offset shapes"
    );

    let auto = f.build_autoroute_tree(1);
    let outline = &f.items[&ItemId(1)];
    assert!(
        (0..2 * pieces)
            .all(|i| matches!(outline.get_tree_shape(auto, i), Some(TileShape::Octagon(_)))),
        "the 45-degree tree regularises every band"
    );
    assert_eq!(
        outline.get_tree_shape(auto, 0).cloned(),
        Some(oct(-200, -200, 3200, 700, -283, 2783, -283, 3783))
    );
    assert_eq!(
        outline.get_tree_shape(auto, 1).cloned(),
        Some(oct(800, 300, 3200, 2700, -1783, 2783, 3217, 3783))
    );
    for i in 0..pieces {
        assert_eq!(
            outline.get_tree_shape(auto, i).cloned(),
            outline.get_tree_shape(auto, i + pieces).cloned()
        );
    }
}

#[test]
fn a_long_outline_edge_off_the_45_degree_grid_does_not_wall_in_the_board_beside_it() {
    let mut f = board_builder::TraceFixture::outline(&[
        Point::new(-4000, -4000),
        Point::new(4000, -800),
        Point::new(-4000, 4000),
    ]);
    let auto = f.build_autoroute_tree(1);
    let outline = &f.items[&ItemId(1)];
    let layer_shapes = outline.tree_shape_count(auto) / 2;
    let shapes: Vec<&TileShape> = (0..layer_shapes)
        .filter_map(|i| outline.get_tree_shape(auto, i))
        .collect();
    let on_the_edge = Point::new(0, -2400);
    let inside_by_1000 = Point::new(-371, -1472);
    assert!(shapes.iter().any(|shape| shape.contains(&on_the_edge)));
    assert!(
        !shapes.iter().any(|shape| shape.contains(&inside_by_1000)),
        "an octagon around the whole edge reaches 1000 units into the board"
    );
}

#[test]
fn a_maze_legal_gap_is_not_rejected_by_an_extra_insertion_margin() {
    let mut f = BoardFixture::new();
    f.items.retain(|id, _| *id == ItemId(2));
    f.insert_all();
    let tree = f.manager.get_default_tree();
    let ctx = f.ctx();
    let mut counter = 0;
    // Pin 2 spans x=-550..-450. The maze leaves two units beyond the 200-unit
    // rule; the uncompensated insertion query must accept the same gap.
    let legal = tree.overlapping_tree_entries_with_clearance(
        &bx(-248, -20, -200, 20),
        Some(0),
        &[],
        1,
        &f.items,
        &ctx,
        &mut counter,
    );
    assert!(
        legal.is_empty(),
        "202 units satisfies the 200-unit clearance"
    );
    let illegal = tree.overlapping_tree_entries_with_clearance(
        &bx(-252, -20, -200, 20),
        Some(0),
        &[],
        1,
        &f.items,
        &ctx,
        &mut counter,
    );
    assert_eq!(
        pairs(&illegal),
        vec![(2, 0)],
        "198 units still violates the rule"
    );
}
