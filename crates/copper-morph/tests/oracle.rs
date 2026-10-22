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
    assert!(
        added.is_empty(),
        "{} new DRC violations after {moved} moves: {:?}",
        added.len(),
        &added[..added.len().min(5)]
    );
    assert!(moved >= 5, "only {moved} moved");
}
