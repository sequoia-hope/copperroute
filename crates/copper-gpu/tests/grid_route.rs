use copper_board::Board;
use copper_board::structure::Unit;
use copper_dsn::{BoardReadResult, DsnReadOptions};
use copper_gpu::{CpuSolver, GridOptions, route_open_connections};

fn read_board(name: &str) -> Board {
    let path = testkit::fixture(name);
    match copper_dsn::read_board(
        std::fs::File::open(&path).unwrap(),
        None,
        None,
        &DsnReadOptions::default(),
    ) {
        BoardReadResult::Success { board, .. } | BoardReadResult::OutlineMissing { board, .. } => {
            *board.unwrap()
        }
        other => panic!("{name} did not read: {other:?}"),
    }
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

#[test]
fn the_cpu_grid_router_closes_connections_without_any_new_violation() {
    let mut board = read_board("Issue508-DAC2020_bm05.dsn");
    let per_mm = board.communication.get_resolution(Unit::Mm);
    let before = violations(&mut board);
    let open_before = copper_drc::DesignRulesChecker::new(&mut board)
        .get_all_unconnected_items()
        .len();
    let options = GridOptions {
        cell: 0.1 * per_mm,
        via_cost: 200,
        max_attempts: 400,
        max_seconds: None,
    };
    let started = std::time::Instant::now();
    let stats = route_open_connections(&mut board, &mut CpuSolver, &options, &|| false);
    let open_after = copper_drc::DesignRulesChecker::new(&mut board)
        .get_all_unconnected_items()
        .len();
    let added: Vec<_> = violations(&mut board)
        .difference(&before)
        .cloned()
        .collect();
    eprintln!(
        "bm05: open {open_before} -> {open_after}; routed {} failed {} vias {} in {:.1}s; new violations {}",
        stats.routed,
        stats.failed,
        stats.vias,
        started.elapsed().as_secs_f64(),
        added.len()
    );
    assert!(stats.routed > 0);
    assert!(open_after < open_before);
    assert!(added.is_empty(), "{:?}", &added[..added.len().min(6)]);
}

#[test]
fn the_gpu_cost_field_equals_the_cpu_reference_exactly() {
    let Some(mut gpu) = copper_gpu::GpuSolver::new() else {
        eprintln!("no GPU adapter; skipped");
        return;
    };
    let mut board = read_board("Issue508-DAC2020_bm05.dsn");
    let per_mm = board.communication.get_resolution(Unit::Mm);
    let gaps = copper_drc::DesignRulesChecker::new(&mut board).get_all_unconnected_items();
    let mut compared = 0;
    let (mut cpu_time, mut gpu_time) = (0.0, 0.0);
    for gap in gaps.iter().filter(|g| g.second_item.is_some()).take(8) {
        let net = board.get_item(gap.first_item).unwrap().net_nos()[0];
        let grid = copper_gpu::RouteGrid::build(&mut board, net, 0.1 * per_mm);
        let sources = copper_gpu::route::terminal_cells(&mut board, &grid, gap.first_item);
        if sources.is_empty() {
            continue;
        }
        let t = std::time::Instant::now();
        let cpu = copper_gpu::search::cost_field(&grid, &sources, 200);
        cpu_time += t.elapsed().as_secs_f64();
        let t = std::time::Instant::now();
        let on_gpu = copper_gpu::FieldSolver::solve(&mut gpu, &grid, &sources, 200);
        gpu_time += t.elapsed().as_secs_f64();
        let mismatches = cpu.iter().zip(&on_gpu).filter(|(a, b)| a != b).count();
        assert_eq!(
            mismatches,
            0,
            "net {net}: {mismatches} of {} nodes differ",
            cpu.len()
        );
        compared += 1;
    }
    eprintln!(
        "{}: {compared} fields identical; cpu {cpu_time:.3}s gpu {gpu_time:.3}s; {} dispatches",
        gpu.adapter_name, gpu.dispatches
    );
    assert!(compared > 0);
}

#[test]
fn routing_on_the_gpu_adds_no_violation() {
    let Some(mut gpu) = copper_gpu::GpuSolver::new() else {
        eprintln!("no GPU adapter; skipped");
        return;
    };
    let mut board = read_board("Issue508-DAC2020_bm05.dsn");
    let per_mm = board.communication.get_resolution(Unit::Mm);
    let before = violations(&mut board);
    let open_before = copper_drc::DesignRulesChecker::new(&mut board)
        .get_all_unconnected_items()
        .len();
    let options = GridOptions {
        cell: 0.1 * per_mm,
        via_cost: 200,
        max_attempts: 400,
        max_seconds: None,
    };
    let started = std::time::Instant::now();
    let stats = route_open_connections(&mut board, &mut gpu, &options, &|| false);
    let open_after = copper_drc::DesignRulesChecker::new(&mut board)
        .get_all_unconnected_items()
        .len();
    let added: Vec<_> = violations(&mut board)
        .difference(&before)
        .cloned()
        .collect();
    eprintln!(
        "gpu bm05: open {open_before} -> {open_after}; routed {} failed {} vias {} in {:.1}s",
        stats.routed,
        stats.failed,
        stats.vias,
        started.elapsed().as_secs_f64()
    );
    assert!(open_after < open_before);
    assert!(added.is_empty(), "{:?}", &added[..added.len().min(6)]);
}

#[test]
fn negotiated_routing_adds_no_violation_and_reports_against_sequential() {
    let per_mm = read_board("Issue508-DAC2020_bm05.dsn")
        .communication
        .get_resolution(Unit::Mm);
    let mut sequential = read_board("Issue508-DAC2020_bm05.dsn");
    route_open_connections(
        &mut sequential,
        &mut CpuSolver,
        &GridOptions {
            cell: 0.1 * per_mm,
            via_cost: 200,
            max_attempts: 400,
            max_seconds: None,
        },
        &|| false,
    );
    let open_sequential = copper_drc::DesignRulesChecker::new(&mut sequential)
        .get_all_unconnected_items()
        .len();

    let mut board = read_board("Issue508-DAC2020_bm05.dsn");
    let before = violations(&mut board);
    let open_before = copper_drc::DesignRulesChecker::new(&mut board)
        .get_all_unconnected_items()
        .len();
    let started = std::time::Instant::now();
    let options = copper_gpu::NegotiationOptions {
        cell: 0.1 * per_mm,
        ..Default::default()
    };
    let stats = copper_gpu::route_negotiated(&mut board, &mut CpuSolver, &options, &|| false);
    let open_after = copper_drc::DesignRulesChecker::new(&mut board)
        .get_all_unconnected_items()
        .len();
    let added: Vec<_> = violations(&mut board)
        .difference(&before)
        .cloned()
        .collect();
    eprintln!(
        "negotiated bm05: open {open_before} -> {open_after} (sequential {open_sequential}); {stats:?} in {:.1}s",
        started.elapsed().as_secs_f64()
    );
    assert!(added.is_empty(), "{:?}", &added[..added.len().min(6)]);
    assert!(stats.committed > 0);
}

#[test]
fn the_gpu_matches_the_cpu_with_congestion_penalties() {
    let Some(mut gpu) = copper_gpu::GpuSolver::new() else {
        eprintln!("no GPU adapter; skipped");
        return;
    };
    let mut board = read_board("Issue508-DAC2020_bm05.dsn");
    let per_mm = board.communication.get_resolution(Unit::Mm);
    let gap = copper_drc::DesignRulesChecker::new(&mut board)
        .get_all_unconnected_items()
        .into_iter()
        .find(|g| g.second_item.is_some())
        .unwrap();
    let net = board.get_item(gap.first_item).unwrap().net_nos()[0];
    let mut grid = copper_gpu::RouteGrid::build(&mut board, net, 0.1 * per_mm);
    let mut seed = 12345u64;
    grid.penalty = (0..grid.blocked.len())
        .map(|_| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 33) % 7) as u32 * 9
        })
        .collect();
    let sources = copper_gpu::route::terminal_cells(&mut board, &grid, gap.first_item);
    let cpu = copper_gpu::search::cost_field(&grid, &sources, 200);
    let on_gpu = copper_gpu::FieldSolver::solve(&mut gpu, &grid, &sources, 200);
    let mismatches = cpu.iter().zip(&on_gpu).filter(|(a, b)| a != b).count();
    assert_eq!(mismatches, 0, "{mismatches} of {} nodes differ", cpu.len());
}
