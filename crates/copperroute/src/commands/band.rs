use std::collections::BTreeMap;

use copper_board::structure::Unit;
use copper_geometry::IntVector;

use crate::ExitCode;
use crate::cli::{BandArgs, Cli};

pub fn run(cli: &Cli, args: &BandArgs) -> ExitCode {
    let (mut loaded, original) =
        match super::reshape::load_routed(cli, &args.input, &args.ses, args.kicad_project.clone(), &args.set) {
            Ok(loaded) => loaded,
            Err(code) => return code,
        };
    let board = &mut loaded.board;
    let per_mm = board.communication.get_resolution(Unit::Mm);
    let baseline = super::reshape::violation_counts(board);
    let mut allowed = BTreeMap::new();
    for found in super::reshape::violation_keys(board) {
        *allowed.entry(found.key).or_insert(0usize) += 1;
    }
    let split = copper_morph::subdivide_traces(board, args.segment_mm * per_mm);
    tracing::info!("{split} traces subdivided to {} mm", args.segment_mm);

    let chunks = args.rounds.div_ceil(args.frame_every.max(1)) + args.via_rounds.div_ceil(args.frame_every.max(1));
    let mut recording = None;
    if let Some(dir) = &args.frames {
        match super::frames::record(dir, chunks + 1, String::new()) {
            Ok(r) => recording = Some(r),
            Err(error) => {
                tracing::error!("Couldn't record frames in {}: {error}", dir.display());
                return ExitCode::Failure;
            }
        }
    }
    let pause = copper_morph::frames::pause();
    let frame = |board: &mut copper_board::Board, label: &str, rounds: usize| {
        let open = copper_drc::all_airlines(board).len();
        let mut stats = super::reshape::step_stats(label, board, &baseline, open, 0.0);
        stats["rounds"] = serde_json::json!(rounds);
        stats["errors"] = super::reshape::error_marks(board, &allowed);
        stats["copper length mm"] = serde_json::json!((copper_length(board) / per_mm * 10.0).round() / 10.0);
        tracing::info!("{label} after {rounds} rounds: {stats}");
        if let Some(r) = &recording {
            r.writer.borrow_mut().write_with(board, &["band", label], &[], &IntVector::ZERO, stats);
        }
    };
    frame(board, "start", 0);
    let mut done = 0;
    for (phase, rounds, free_vias) in [("traces", args.rounds, false), ("vias", args.via_rounds, true)] {
        let mut left = rounds;
        while left > 0 {
            let n = left.min(args.frame_every.max(1));
            copper_morph::relax_clearance(board, n, None, &BTreeMap::new(), args.tension, free_vias);
            left -= n;
            done += n;
            frame(board, phase, done);
        }
    }
    drop(pause);
    let simplified = copper_morph::simplify_traces(board, 0.005 * per_mm);
    tracing::info!("{simplified} traces simplified");
    frame(board, "simplified", done);

    super::reshape::new_violation_items(board, &allowed);
    if let Err(error) = super::reshape::write_routed(&mut loaded, &original, &args.output) {
        tracing::error!("Couldn't write {}: {error}", args.output.display());
        return ExitCode::Failure;
    }
    ExitCode::Ok
}

fn copper_length(board: &copper_board::Board) -> f64 {
    board
        .get_items()
        .filter_map(|item| match item {
            copper_board::items::Item::Trace(trace) => Some(trace.get_length()),
            _ => None,
        })
        .sum()
}
