use copper_board::Board;
use copper_board::structure::Unit;
use copper_dsn::{BoardReadResult, DsnReadOptions};
use copper_gpu::{CpuSolver, GridOptions, route_open_connections};

fn read_board(name: &str) -> Board {
    let path = testkit::fixture(name);
    match copper_dsn::read_board(std::fs::File::open(&path).unwrap(), None, None, &DsnReadOptions::default()) {
        BoardReadResult::Success { board, .. } | BoardReadResult::OutlineMissing { board, .. } => *board.unwrap(),
        other => panic!("{name} did not read: {other:?}"),
    }
}

type Pairs = std::collections::BTreeSet<(copper_board::ItemId, Option<copper_board::ItemId>, String)>;

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
    let open_before = copper_drc::DesignRulesChecker::new(&mut board).get_all_unconnected_items().len();
    let options = GridOptions { cell: 0.1 * per_mm, via_cost: 200, max_attempts: 400 };
    let started = std::time::Instant::now();
    let stats = route_open_connections(&mut board, &mut CpuSolver, &options, &|| false);
    let open_after = copper_drc::DesignRulesChecker::new(&mut board).get_all_unconnected_items().len();
    let added: Vec<_> = violations(&mut board).difference(&before).cloned().collect();
    eprintln!(
        "bm05: open {open_before} -> {open_after}; routed {} failed {} vias {} in {:.1}s; new violations {}",
        stats.routed, stats.failed, stats.vias, started.elapsed().as_secs_f64(), added.len()
    );
    assert!(stats.routed > 0);
    assert!(open_after < open_before);
    assert!(added.is_empty(), "{:?}", &added[..added.len().min(6)]);
}

#[test]
#[ignore = "diagnostic"]
fn print_one_attempt() {
    let mut board = read_board("Issue508-DAC2020_bm05.dsn");
    let per_mm = board.communication.get_resolution(Unit::Mm);
    let gap = copper_drc::DesignRulesChecker::new(&mut board).get_all_unconnected_items().into_iter().find(|g| g.second_item.is_some()).unwrap();
    let net = board.get_item(gap.first_item).unwrap().net_nos()[0];
    let grid = copper_gpu::RouteGrid::build(&mut board, net, 0.1 * per_mm);
    let blocked = grid.blocked.iter().filter(|b| **b).count();
    eprintln!("net {net} grid {}x{} layers {:?} half_width {} via {:?} blocked {blocked}/{}", grid.nx, grid.ny, grid.layers, grid.half_width, grid.via.as_ref().map(|v| v.radius), grid.blocked.len());
    let kinds = |id| format!("{:?}", board.get_item(id).unwrap()).split('(').next().unwrap().to_string();
    eprintln!("from {} {:?} to {:?} {:?}", gap.first_item.0, kinds(gap.first_item), gap.second_item, kinds(gap.second_item.unwrap()));
    let sources = copper_gpu::route::terminal_cells(&mut board, &grid, gap.first_item);
    let targets = copper_gpu::route::terminal_cells(&mut board, &grid, gap.second_item.unwrap());
    let cost = copper_gpu::search::cost_field(&grid, &sources, 200);
    let path = copper_gpu::search::trace_back(&grid, &cost, &targets, 200);
    eprintln!("sources {} targets {} path {:?}", sources.len(), targets.len(), path.as_ref().map(|p| (p.len(), p.first().copied(), p.last().copied())));
    let traces = board.get_items().filter(|i| i.is_trace()).count();
    let stats = copper_gpu::route_open_connections(&mut board, &mut copper_gpu::CpuSolver, &copper_gpu::GridOptions { cell: 0.1 * per_mm, via_cost: 200, max_attempts: 1 }, &|| false);
    eprintln!("after one attempt: {stats:?}; traces {traces} -> {}", board.get_items().filter(|i| i.is_trace()).count());
    let trace = board.get_items().find(|i| i.is_trace()).unwrap().id();
    let contacts: Vec<u32> = board.normal_contacts(trace).into_iter().map(|c| c.0).collect();
    let (first, last) = match board.get_item(trace).unwrap() { copper_board::items::Item::Trace(t) => (t.first_corner().unwrap().to_float(), t.last_corner().unwrap().to_float()), _ => unreachable!() };
    let pin_shape = |board: &mut Board, id: copper_board::ItemId| board.item_tile_shape(id, 0).unwrap();
    let (s_from, s_to) = (pin_shape(&mut board, gap.first_item), pin_shape(&mut board, gap.second_item.unwrap()));
    eprintln!("trace {} contacts {contacts:?}; first {first:?} d_from {:.0} d_to {:.0}; last {last:?} d_from {:.0} d_to {:.0}",
        trace.0, s_from.distance(&first), s_to.distance(&first), s_from.distance(&last), s_to.distance(&last));
    let ctx = board.ctx();
    for id in [gap.first_item, gap.second_item.unwrap(), trace] {
        let it = board.get_item(id).unwrap();
        eprintln!("item {} layers {}..{} nets {:?}", id.0, it.first_layer(&ctx), it.last_layer(&ctx), it.net_nos());
    }
}
