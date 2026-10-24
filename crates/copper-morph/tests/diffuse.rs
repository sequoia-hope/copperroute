use std::collections::BTreeSet;

use copper_board::Board;
use copper_dsn::{BoardReadResult, DsnReadOptions};
use copper_morph::{DiffuseOptions, MoveOracle, apply_moves, diffusion_moves};

fn board() -> Board {
    let dsn = testkit::fixture("Issue508-DAC2020_bm05.dsn");
    match copper_dsn::read_board(std::fs::File::open(&dsn).unwrap(), None, None, &DsnReadOptions::default()) {
        BoardReadResult::Success { board, .. } | BoardReadResult::OutlineMissing { board, .. } => *board.unwrap(),
        other => panic!("did not read: {other:?}"),
    }
}

fn crowded(board: &Board) -> DiffuseOptions {
    let peak = diffusion_moves(board, &DiffuseOptions { rounds: 0, ..DiffuseOptions::default() }, &[], &BTreeSet::new(), &[]).peak_before;
    DiffuseOptions { target_density: peak * 0.7, rounds: 60, ..DiffuseOptions::default() }
}

fn movable(board: &Board) -> Vec<i32> {
    board.components.get_all().filter(|c| !c.position_fixed).map(|c| c.id).collect()
}

#[test]
fn spreading_lowers_the_peak_density() {
    let board = board();
    let report = diffusion_moves(&board, &crowded(&board), &[], &BTreeSet::new(), &[]);
    assert!(!report.moves.is_empty());
    assert!(report.peak_after < report.peak_before, "{report:?}");
}

#[test]
fn locked_parts_stay_and_groups_move_as_one() {
    let board = board();
    let ids = movable(&board);
    let locked: BTreeSet<i32> = ids.iter().copied().step_by(2).collect();
    let group = vec![ids[1], ids[3]];
    let report = diffusion_moves(&board, &crowded(&board), std::slice::from_ref(&group), &locked, &[]);
    assert!(report.moves.keys().all(|id| !locked.contains(id)));
    assert_eq!(report.moves.get(&group[0]), report.moves.get(&group[1]));
}

#[test]
fn no_part_moves_further_than_the_limit() {
    let board = board();
    let options = DiffuseOptions { max_move_mm: 0.5, ..crowded(&board) };
    let per_mm = board.communication.get_resolution(copper_board::structure::Unit::Mm);
    let report = diffusion_moves(&board, &options, &[], &BTreeSet::new(), &[]);
    assert!(report.moves.values().all(|(x, y)| x.hypot(*y) <= 0.5 * per_mm + 1.0));
}

#[test]
fn demand_pushes_parts_away_from_it() {
    let board = board();
    let options = crowded(&board);
    let b = board.bounding_box;
    let centre = copper_geometry::FloatPoint::new((f64::from(b.ll.x) + f64::from(b.ur.x)) / 2.0, (f64::from(b.ll.y) + f64::from(b.ur.y)) / 2.0);
    let per_mm = board.communication.get_resolution(copper_board::structure::Unit::Mm);
    let plain = diffusion_moves(&board, &options, &[], &BTreeSet::new(), &[]);
    let pushed = diffusion_moves(&board, &options, &[], &BTreeSet::new(), &[(centre, 50.0 * per_mm * per_mm)]);
    assert_ne!(plain.moves, pushed.moves);
}

#[test]
fn applied_moves_add_no_violation() {
    let mut board = board();
    let report = diffusion_moves(&board, &crowded(&board), &[], &BTreeSet::new(), &[]);
    let violations = |board: &mut Board| {
        copper_drc::DesignRulesChecker::new(board)
            .get_all_violations()
            .into_iter()
            .map(|v| (v.first_item, v.second_item, format!("{:?}", v.kind)))
            .collect::<BTreeSet<_>>()
    };
    let before = violations(&mut board);
    let oracle = MoveOracle { guard: 0.0, routable_copper_is_soft: true, fixed_copper_bends: false };
    let made = apply_moves(&mut board, &oracle, &report.moves, &[], 2);
    assert!(!made.is_empty());
    let added: Vec<_> = violations(&mut board).difference(&before).cloned().collect();
    assert!(added.is_empty(), "{added:?}");
}
