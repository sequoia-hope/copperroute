use copper_board::Board;
use copper_dsn::{BoardReadResult, DsnReadOptions};
use copper_geometry::IntVector;
use copper_morph::{MoveOracle, scale};

fn read_board(name: &str) -> Board {
    let path = testkit::fixture(name);
    let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("cannot open {path:?}: {e}"));
    match copper_dsn::read_board(file, None, None, &DsnReadOptions::default()) {
        BoardReadResult::Success { board, .. } | BoardReadResult::OutlineMissing { board, .. } => {
            *board.expect("the fixture produces a board")
        }
        other => panic!("{name} did not read: {other:?}"),
    }
}

fn oracle() -> MoveOracle {
    MoveOracle {
        guard: 0.0,
        routable_copper_is_soft: true,
    }
}

fn movable_components(board: &Board) -> Vec<i32> {
    board
        .components
        .get_all()
        .filter(|c| !c.position_fixed)
        .map(|c| c.id)
        .filter(|id| !board.get_component_items(*id).is_empty())
        .collect()
}

#[test]
fn a_line_searched_move_never_adds_a_conflict() {
    let mut board = read_board("Issue756-tomu-fpga.dsn");
    let oracle = oracle();
    let span = board.bounding_box.ur.x - board.bounding_box.ll.x;
    let mut moved = 0;
    for id in movable_components(&board) {
        let before = oracle.conflicts(&mut board, id, &IntVector::ZERO);
        let push = IntVector::new(span / 40, -span / 60);
        let (fraction, _) = oracle.line_search(&mut board, id, &push);
        let step = scale(&push, fraction);
        if step != IntVector::ZERO {
            moved += 1;
        }
        assert!(
            oracle.translate(&mut board, id, &step),
            "component {id} moves"
        );
        let after = oracle.conflicts(&mut board, id, &IntVector::ZERO);
        let added: Vec<_> = after.difference(&before).collect();
        assert!(
            added.is_empty(),
            "component {id} gained conflicts {added:?} moving {step:?}"
        );
    }
    assert!(
        moved >= 3,
        "only {moved} components moved; the test proves nothing"
    );
}

#[test]
fn a_move_across_the_board_is_clamped() {
    let mut board = read_board("Issue756-tomu-fpga.dsn");
    let oracle = oracle();
    let span = board.bounding_box.ur.x - board.bounding_box.ll.x;
    let clamped = movable_components(&board).into_iter().any(|id| {
        let (fraction, blockers) = oracle.line_search(&mut board, id, &IntVector::new(span, 0));
        fraction < 1.0 && !blockers.is_empty()
    });
    assert!(clamped, "some component is stopped short of the far edge");
}

#[test]
fn standing_still_is_always_admissible() {
    let mut board = read_board("Issue756-tomu-fpga.dsn");
    let oracle = oracle();
    for id in movable_components(&board) {
        let (fraction, blockers) = oracle.line_search(&mut board, id, &IntVector::ZERO);
        assert_eq!((fraction, blockers.len()), (1.0, 0), "component {id}");
    }
}

#[test]
#[ignore = "diagnostic: prints line-search fractions per fixture"]
fn print_fractions() {
    for name in [
        "Issue313-FastTest.dsn",
        "Issue756-tomu-fpga.dsn",
        "Issue143-rpi_splitter.dsn",
        "Issue508-DAC2020_bm05.dsn",
    ] {
        let mut board = read_board(name);
        let oracle = oracle();
        let span = board.bounding_box.ur.x - board.bounding_box.ll.x;
        let ids = movable_components(&board);
        let mut fractions = Vec::new();
        for &id in ids.iter().take(15) {
            let base = oracle.conflicts(&mut board, id, &IntVector::ZERO).len();
            let (f, b) = oracle.line_search(&mut board, id, &IntVector::new(span / 50, 0));
            fractions.push(format!("{f:.2}/{base}/{}", b.len()));
        }
        println!(
            "{name}: {} movable of {}; fraction/base/blockers: {}",
            ids.len(),
            board.components.count(),
            fractions.join(" ")
        );
    }
}

#[test]
fn the_drc_checker_agrees_no_violation_was_added() {
    let mut board = read_board("Issue756-tomu-fpga.dsn");
    let oracle = oracle();
    let pairs = |board: &mut Board| -> std::collections::BTreeSet<(
        copper_board::ItemId,
        Option<copper_board::ItemId>,
        String,
    )> {
        copper_drc::DesignRulesChecker::new(board)
            .get_all_violations()
            .into_iter()
            .map(|v| (v.first_item, v.second_item, format!("{:?}", v.kind)))
            .collect()
    };
    let before = pairs(&mut board);
    let span = board.bounding_box.ur.x - board.bounding_box.ll.x;
    let mut moved = 0;
    for (i, id) in movable_components(&board).into_iter().enumerate() {
        let push = match i % 4 {
            0 => IntVector::new(span / 30, 0),
            1 => IntVector::new(0, span / 30),
            2 => IntVector::new(-span / 30, 0),
            _ => IntVector::new(0, -span / 30),
        };
        let (fraction, _) = oracle.line_search(&mut board, id, &push);
        let step = scale(&push, fraction);
        if step != IntVector::ZERO && oracle.translate(&mut board, id, &step) {
            moved += 1;
        }
    }
    let after = pairs(&mut board);
    let added: Vec<_> = after.difference(&before).collect();
    for (a, b, kind) in &added {
        let detail = copper_drc::DesignRulesChecker::new(&mut board.clone())
            .get_all_violations()
            .into_iter()
            .find(|v| v.first_item == *a && v.second_item == *b)
            .map(|v| format!("{v:?}"))
            .unwrap_or_default();
        let describe = |id: copper_board::ItemId| {
            let item = board.get_item(id).unwrap();
            format!(
                "{}#{} comp {} nets {:?} class {}",
                format!("{item:?}").split('(').next().unwrap(),
                id.0,
                item.component_id(),
                item.net_nos(),
                item.clearance_class()
            )
        };
        eprintln!(
            "matrix clearance class 2/2 layer 0: {}",
            board.rules.clearance_matrix.get_value(2, 2, 0, false)
        );
        eprintln!(
            "ADDED {kind}: {} / {}\n  {}",
            describe(*a),
            b.map(describe).unwrap_or_default(),
            &detail[..detail.len().min(500)]
        );
    }
    assert!(
        added.is_empty(),
        "{} new DRC violations after {moved} moves: {:?}",
        added.len(),
        &added[..added.len().min(5)]
    );
    assert!(moved >= 5, "only {moved} moved");
}

#[test]
fn a_nudged_placement_survives_a_session_round_trip() {
    let read = || {
        let path = testkit::fixture("Issue756-tomu-fpga.dsn");
        match copper_dsn::read_board(
            std::fs::File::open(&path).unwrap(),
            None,
            None,
            &DsnReadOptions::default(),
        ) {
            BoardReadResult::Success {
                board,
                coordinate_transform,
                ..
            }
            | BoardReadResult::OutlineMissing {
                board,
                coordinate_transform,
                ..
            } => (*board.unwrap(), coordinate_transform.unwrap()),
            other => panic!("did not read: {other:?}"),
        }
    };
    let (mut board, transform) = read();
    let oracle = oracle();
    let span = board.bounding_box.ur.x - board.bounding_box.ll.x;
    let mut moved = Vec::new();
    for id in movable_components(&board) {
        let (fraction, _) =
            oracle.line_search(&mut board, id, &IntVector::new(span / 60, span / 90));
        let step = scale(&IntVector::new(span / 60, span / 90), fraction);
        if step != IntVector::ZERO && oracle.translate(&mut board, id, &step) {
            moved.push(id);
        }
    }
    assert!(!moved.is_empty());
    let mut session = Vec::new();
    copper_dsn::ses_writer::write(&board, &transform, &mut session, "roundtrip").unwrap();

    let (mut fresh, transform) = read();
    let summary = copper_dsn::ses_reader::read(session.as_slice(), &mut fresh, &transform).unwrap();
    assert_eq!(summary.components_moved, moved.len());
    for component in board.components.get_all() {
        let again = fresh
            .components
            .get_all()
            .find(|c| c.name == component.name)
            .unwrap();
        assert_eq!(
            component.get_location(),
            again.get_location(),
            "{}",
            component.name
        );
    }
}

#[test]
fn pushing_moves_further_than_a_plain_line_search_and_adds_no_violation() {
    let mut board = read_board("Issue508-DAC2020_bm05.dsn");
    let oracle = oracle();
    let span = board.bounding_box.ur.x - board.bounding_box.ll.x;
    let want = IntVector::new(span / 8, 0);
    let pairs = |board: &mut Board| -> std::collections::BTreeSet<(
        copper_board::ItemId,
        Option<copper_board::ItemId>,
        String,
    )> {
        copper_drc::DesignRulesChecker::new(board)
            .get_all_violations()
            .into_iter()
            .map(|v| (v.first_item, v.second_item, format!("{:?}", v.kind)))
            .collect()
    };
    let before = pairs(&mut board);
    let mut gained = 0;
    for id in movable_components(&board) {
        let (plain, blockers) = oracle.line_search(&mut board.clone(), id, &want);
        if plain >= 1.0 || blockers.is_empty() {
            continue;
        }
        let mut trial = board.clone();
        let moves = oracle.push(&mut trial, id, &want, 2);
        let own = moves
            .iter()
            .rev()
            .find(|(c, _)| *c == id)
            .map_or(0, |(_, v)| v.x);
        if f64::from(own) > plain * f64::from(want.x) + 1.0 && moves.len() > 1 {
            gained += 1;
            let added: Vec<_> = pairs(&mut trial).difference(&before).cloned().collect();
            assert!(added.is_empty(), "pushing {id} added {added:?}");
        }
    }
    assert!(
        gained > 0,
        "no push ever got further than the plain line search"
    );
}

#[test]
#[ignore = "diagnostic: who blocks whom"]
fn print_blockers() {
    let mut board = read_board("Issue508-DAC2020_bm05.dsn");
    let oracle = oracle();
    let span = board.bounding_box.ur.x - board.bounding_box.ll.x;
    let want = IntVector::new(span / 8, 0);
    for id in movable_components(&board).into_iter().take(20) {
        let (plain, blockers) = oracle.line_search(&mut board.clone(), id, &want);
        let kinds: Vec<String> = blockers
            .iter()
            .map(|(_, o)| {
                let item = board.get_item(*o).unwrap();
                let c = item.component_id();
                let kind = format!("{item:?}").split('(').next().unwrap().to_string();
                let movable = oracle.line_search(&mut board.clone(), c.max(1), &want).0;
                format!("{kind}@c{c}(fr{movable:.2})")
            })
            .collect();
        let mut trial = board.clone();
        let moves = oracle.push(&mut trial, id, &want, 2);
        println!("c{id}: plain {plain:.2}; blockers {kinds:?}; push moves {moves:?}");
    }
}

#[test]
#[ignore = "diagnostic: step through one push"]
fn print_one_push() {
    let mut board = read_board("Issue508-DAC2020_bm05.dsn");
    let oracle = oracle();
    let span = board.bounding_box.ur.x - board.bounding_box.ll.x;
    let want = IntVector::new(span / 8, 0);
    let (f, b) = oracle.line_search(&mut board, 4, &want);
    let describe =
        |board: &Board,
         b: &std::collections::BTreeSet<(copper_board::ItemId, copper_board::ItemId)>|
         -> Vec<String> {
            b.iter()
                .map(|(m, o)| {
                    let mi = board.get_item(*m).unwrap();
                    let oi = board.get_item(*o).unwrap();
                    format!(
                        "{}c{} vs {}c{}",
                        format!("{mi:?}").split('(').next().unwrap(),
                        mi.component_id(),
                        format!("{oi:?}").split('(').next().unwrap(),
                        oi.component_id()
                    )
                })
                .collect()
        };
    println!(
        "before: fraction {f:.3} blockers {:?}",
        describe(&board, &b)
    );
    let loc = |board: &Board, c: i32| {
        board
            .components
            .get_all()
            .find(|x| x.id == c)
            .unwrap()
            .get_location()
            .cloned()
    };
    println!("c26 at {:?}", loc(&board, 26));
    let (f26, _) = oracle.line_search(&mut board, 26, &want);
    println!(
        "c26 line search {f26:.3}; translate ok {}",
        oracle.translate(&mut board, 26, &scale(&want, f26))
    );
    println!("c26 now at {:?}", loc(&board, 26));
    let (f, b) = oracle.line_search(&mut board, 4, &want);
    println!("after: fraction {f:.3} blockers {:?}", describe(&board, &b));
}

#[test]
fn every_item_shape_moves_by_exactly_the_vector() {
    let mut board = read_board("Issue508-DAC2020_bm05.dsn");
    let oracle = oracle();
    let by = IntVector::new(12_345, -6_789);
    for id in movable_components(&board).into_iter().take(8) {
        let items = board.get_component_items(id);
        let boxes = |board: &mut Board| -> Vec<copper_geometry::IntBox> {
            let mut out = Vec::new();
            for &item in &items {
                let ctx = board.ctx();
                let count = board.get_item(item).unwrap().tile_shape_count(&ctx);
                for i in 0..count {
                    out.push(board.item_tile_shape(item, i).unwrap().bounding_box());
                }
            }
            out
        };
        let before = boxes(&mut board);
        assert!(oracle.translate(&mut board, id, &by));
        let after = boxes(&mut board);
        assert!(!before.is_empty());
        for (b, a) in before.iter().zip(&after) {
            assert_eq!(
                (
                    a.ll.x - b.ll.x,
                    a.ll.y - b.ll.y,
                    a.ur.x - b.ur.x,
                    a.ur.y - b.ur.y
                ),
                (by.x, by.y, by.x, by.y),
                "component {id}"
            );
        }
        let probe = board.item_tile_shape(items[0], 0).unwrap();
        let hits = board.overlapping_items_with_clearance(&probe, None, &[], 0);
        assert!(
            hits.contains(&items[0]),
            "the search tree finds component {id}'s item where it now is"
        );
    }
}
