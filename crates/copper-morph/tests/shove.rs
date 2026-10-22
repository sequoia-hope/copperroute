use copper_board::Board;
use copper_dsn::{BoardReadResult, DsnReadOptions};
use copper_geometry::IntVector;
use copper_morph::MoveOracle;

fn routed_board() -> Board {
    let dsn = testkit::fixture("Issue508-DAC2020_bm05.dsn");
    let (mut board, transform) = match copper_dsn::read_board(
        std::fs::File::open(&dsn).unwrap(),
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
    };
    let ses = std::fs::File::open(testkit::fixture("Issue508-DAC2020_bm05.ses")).unwrap();
    copper_dsn::ses_reader::read(ses, &mut board, &transform).unwrap();
    let wiring: Vec<copper_board::ItemId> = board
        .get_items()
        .filter(|i| i.is_trace() || matches!(i, copper_board::items::Item::Via(_)))
        .map(|i| i.id())
        .collect();
    for id in wiring {
        board.get_item_mut(id).unwrap().unfix();
    }
    board
}

type Pairs =
    std::collections::BTreeSet<(copper_board::ItemId, Option<copper_board::ItemId>, String)>;

fn violations(board: &mut Board) -> Pairs {
    copper_drc::DesignRulesChecker::new(board)
        .get_all_violations()
        .into_iter()
        .map(|v| (v.first_item, v.second_item, format!("{:?}", v.kind)))
        .collect()
}

fn unconnected(board: &mut Board) -> usize {
    copper_drc::DesignRulesChecker::new(board)
        .get_all_unconnected_items()
        .len()
}

#[test]
fn shoving_keeps_every_connection_and_adds_no_violation() {
    let mut board = routed_board();
    let oracle = MoveOracle {
        guard: 0.0,
        routable_copper_is_soft: false,
    };
    let open_before = unconnected(&mut board);
    let before = violations(&mut board);
    let traces_before = board.get_items().filter(|i| i.is_trace()).count();
    let span = board.bounding_box.ur.x - board.bounding_box.ll.x;
    let ids: Vec<i32> = board
        .components
        .get_all()
        .filter(|c| !c.position_fixed)
        .map(|c| c.id)
        .collect();
    let mut moved = 0;
    let mut shoved = 0;
    for (i, id) in ids.into_iter().enumerate() {
        let by = match i % 4 {
            0 => IntVector::new(span / 150, 0),
            1 => IntVector::new(0, span / 150),
            2 => IntVector::new(-span / 150, 0),
            _ => IntVector::new(0, -span / 150),
        };
        let own_nets: std::collections::BTreeSet<i32> = board
            .get_component_items(id)
            .iter()
            .filter_map(|i| board.get_item(*i))
            .flat_map(|i| i.net_nos().to_vec())
            .collect();
        let foreign = |board: &Board| -> std::collections::BTreeSet<copper_board::ItemId> {
            board
                .get_items()
                .filter(|i| i.is_trace() && !i.net_nos().iter().any(|n| own_nets.contains(n)))
                .map(|i| i.id())
                .collect()
        };
        let foreign_before = foreign(&board);
        if oracle.translate_shoving(&mut board, &[id], &by) {
            moved += 1;
            if foreign(&board) != foreign_before {
                shoved += 1;
            }
        }
    }
    let added: Vec<_> = violations(&mut board)
        .difference(&before)
        .cloned()
        .collect();
    eprintln!(
        "moved {moved} ({shoved} only by shoving); traces {traces_before} -> {}; unconnected {open_before} -> {}; added {}",
        board.get_items().filter(|i| i.is_trace()).count(),
        unconnected(&mut board),
        added.len()
    );
    assert!(moved >= 5, "only {moved} parts moved");
    assert!(
        shoved >= 1,
        "no move needed the shove engine; the test proves nothing about it"
    );
    assert!(
        unconnected(&mut board) <= open_before,
        "a connection was lost"
    );
    assert!(added.is_empty(), "added {:?}", &added[..added.len().min(6)]);
}
