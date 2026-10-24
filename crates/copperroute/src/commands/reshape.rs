use std::collections::{BTreeMap, BTreeSet};

use copper_board::{Board, ItemId};
use copper_board::items::Item;
use copper_board::structure::Unit;
use copper_geometry::{FloatPoint, IntVector};
use copper_morph::{PartMove, reshape_step};
use serde::Deserialize;

use crate::ExitCode;
use crate::cli::{Cli, ReshapeArgs};
use crate::ops::fixed_wiring;
use crate::ops::load::{BoardSource, LoadRequest, load};

#[derive(Deserialize)]
struct Plan {
    board_outline: Vec<[f64; 2]>,
    view: Option<[f64; 4]>,
    steps: Vec<PlanStep>,
}

#[derive(Deserialize)]
struct PlanStep {
    label: String,
    outline: Vec<[f64; 2]>,
    /// Part reference to [dx, dy, turn]: millimetres and degrees from where the plan starts.
    parts: BTreeMap<String, [f64; 3]>,
}

pub fn run(cli: &Cli, args: &ReshapeArgs) -> ExitCode {
    let plan: Plan = match std::fs::read_to_string(&args.plan)
        .map_err(|e| e.to_string())
        .and_then(|text| serde_json::from_str(&text).map_err(|e| e.to_string()))
    {
        Ok(plan) => plan,
        Err(error) => {
            tracing::error!("{}: {error}", args.plan.display());
            return ExitCode::UsageError;
        }
    };
    let Some(backend) = copper_core::backend(&args.repair_backend) else {
        tracing::error!("--repair-backend {}: no such backend", args.repair_backend);
        return ExitCode::UsageError;
    };
    let last = args.to.unwrap_or(plan.steps.len().saturating_sub(1));
    if plan.steps.is_empty() || last >= plan.steps.len() {
        tracing::error!("{}: no step {last}", args.plan.display());
        return ExitCode::UsageError;
    }

    let mut request = LoadRequest::for_board(BoardSource::Path(args.input.clone()));
    request.discover_adjacent_rules = true;
    request.kicad_project = args.kicad_project.clone();
    request.settings = super::overrides(cli, &args.set);
    let mut loaded = match load(&request) {
        Ok(loaded) => loaded,
        Err(error) => {
            tracing::error!("{error}");
            return ExitCode::Failure;
        }
    };
    let board = &mut loaded.board;
    let original = fixed_wiring::system_fixed_wiring(board);
    let designed: BTreeSet<ItemId> = board
        .get_items()
        .filter(|i| i.is_trace() || matches!(i, Item::Via(_)))
        .map(Item::id)
        .collect();
    if let Err(error) = fixed_wiring::apply_sidecar(&args.ses, board) {
        tracing::error!("{error}");
        return ExitCode::Failure;
    }
    let session = match std::fs::read(&args.ses) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::error!("Couldn't read {}: {error}", args.ses.display());
            return ExitCode::Failure;
        }
    };
    if let Err(error) = copper_dsn::ses_reader::read(&session[..], board, &loaded.transform) {
        tracing::error!("Couldn't import {}: {error}", args.ses.display());
        return ExitCode::Failure;
    }
    // The session reader fixes what it imports; its routing is the router's to redo, unlike
    // wiring the design itself fixed.
    let routed: Vec<ItemId> = board
        .get_items()
        .filter(|i| (i.is_trace() || matches!(i, Item::Via(_))) && !designed.contains(&i.id()))
        .map(Item::id)
        .collect();
    for id in routed {
        if let Some(item) = board.get_item_mut(id) {
            item.unfix();
        }
    }

    let per_mm = board.communication.get_resolution(Unit::Mm);
    let scale = loaded.transform.scale_factor();
    let (base_x, base_y) = (loaded.transform.base_x(), loaded.transform.base_y());
    let to_board = |p: &[f64; 2]| FloatPoint::new(p[0] * per_mm - base_x * scale, p[1] * per_mm - base_y * scale);
    let ids: BTreeMap<String, i32> = board
        .components
        .get_all()
        .map(|c| (c.name.clone(), c.id))
        .collect();

    let mut recording = None;
    if let Some(dir) = &args.frames {
        match super::frames::record(dir, plan.steps.len() + 1, String::new()) {
            Ok(r) => recording = Some(r),
            Err(error) => {
                tracing::error!("Couldn't record frames in {}: {error}", dir.display());
                return ExitCode::Failure;
            }
        }
    }
    if let (Some(r), Some(v)) = (&recording, plan.view) {
        let (a, b) = (to_board(&[v[0], v[1]]), to_board(&[v[2], v[3]]));
        r.writer.borrow_mut().view = Some([a.x.min(b.x), a.y.min(b.y), a.x.max(b.x), a.y.max(b.y)]);
    }

    let baseline = violation_counts(board);
    let start_open = copper_drc::all_airlines(board).len();
    let mut allowed: BTreeMap<ViolationKey, usize> = BTreeMap::new();
    for (key, _, _) in violation_keys(board) {
        *allowed.entry(key).or_insert(0) += 1;
    }
    let pairs = diff_pairs(board);
    let protected: BTreeSet<i32> = pairs
        .iter()
        .flat_map(|(p, n)| [p, n])
        .flat_map(|name| board.rules.nets.get_by_name(name).into_iter().map(|net| net.net_number).collect::<Vec<_>>())
        .collect();
    tracing::info!("differential pairs, never ripped: {pairs:?}");
    let net_of = |name: &str| board.rules.nets.get_by_name(name).first().map(|n| n.net_number);
    let pair_nets: Vec<(i32, i32)> = pairs
        .iter()
        .filter_map(|(p, n)| Some((net_of(p)?, net_of(n)?)))
        .collect();
    if let Some(r) = &recording {
        let mut stats = step_stats("start", board, &baseline, start_open, 0.0);
        stats["pairs"] = pair_metrics(board, &pairs, per_mm);
        r.writer.borrow_mut().write_with(board, &["start"], &[], &IntVector::ZERO, stats);
    }

    let mut outline: Vec<FloatPoint> = plan.board_outline.iter().map(to_board).collect();
    let tolerated = copper_morph::relax_clearance(board, 0, Some(&outline), &BTreeMap::new()).close_pairs;
    let mut applied: BTreeMap<i32, [f64; 3]> = BTreeMap::new();
    let pause = copper_morph::frames::pause();
    for (k, step) in plan.steps.iter().enumerate().take(last + 1) {
        let started = std::time::Instant::now();
        let target: Vec<FloatPoint> = step.outline.iter().map(to_board).collect();
        if target.len() != outline.len() {
            tracing::error!("step {k}: {} outline points, expected {}", target.len(), outline.len());
            return ExitCode::Failure;
        }
        let mut moves = BTreeMap::new();
        for (name, [dx, dy, turn]) in &step.parts {
            let Some(&id) = ids.get(name) else {
                continue;
            };
            let done = applied.entry(id).or_insert([0.0; 3]);
            let part = PartMove {
                by: ((dx - done[0]) * per_mm, (dy - done[1]) * per_mm),
                turn_degree: turn - done[2],
            };
            *done = [*dx, *dy, *turn];
            if part.by.0.hypot(part.by.1) >= 1.0 || part.turn_degree.abs() > 1e-6 {
                moves.insert(id, part);
            }
        }
        let report = reshape_step(board, &outline, &target, &moves, args.cell_mm, &pair_nets);
        outline = target;
        if args.relax > 0 {
            let r = copper_morph::relax_clearance(board, args.relax, Some(&outline), &tolerated);
            tracing::info!(
                "step {k}: relaxed {} traces, overlaps {} -> {}",
                r.traces_moved,
                r.overlaps_before,
                r.overlaps_after
            );
        }
        let due = if args.repair_at_keyframes {
            !step.label.contains('→')
        } else {
            args.repair_every > 0 && (k % args.repair_every == args.repair_every - 1 || k == last)
        };
        let ripped = if due {
            repair(board, &allowed, &protected, &loaded.settings, args.repair_ms, backend)
        } else {
            0
        };
        let seconds = started.elapsed().as_secs_f64();
        let open = copper_drc::all_airlines(board).len();
        tracing::info!(
            "step {k} ({}): {} parts, {} traces, {} vias, {} areas; {ripped} ripped; {open} open; {seconds:.1} s",
            step.label,
            report.parts,
            report.traces,
            report.vias,
            report.areas
        );
        if let Some(r) = &recording {
            let mut stats = step_stats(&step.label, board, &baseline, open, seconds);
            stats["ripped"] = serde_json::json!(ripped);
            stats["pairs"] = pair_metrics(board, &pairs, per_mm);
            r.writer.borrow_mut().write_with(
                board,
                &["reshape", &step.label],
                &[],
                &IntVector::ZERO,
                stats,
            );
        }
    }
    drop(pause);

    let superseded = fixed_wiring::supersede(board, &original, &loaded.transform);
    let mut bytes = Vec::new();
    let outline_mm: Vec<[f64; 2]> = plan.steps[last].outline.clone();
    if let Err(error) = copper_core::save_as_specctra_session_ses(
        board,
        &loaded.transform,
        &loaded.job.name,
        &mut bytes,
    )
    .map_err(|e| e.to_string())
    .and_then(|()| std::fs::write(&args.output, &bytes).map_err(|e| e.to_string()))
    .and_then(|()| fixed_wiring::write_sidecar(&args.output, &superseded).map_err(|e| e.to_string()))
    .and_then(|()| {
        let text = serde_json::json!({ "step": last, "label": plan.steps[last].label, "outline_mm": outline_mm });
        std::fs::write(args.output.with_extension("outline.json"), text.to_string()).map_err(|e| e.to_string())
    }) {
        tracing::error!("Couldn't write {}: {error}", args.output.display());
        return ExitCode::Failure;
    }
    ExitCode::Ok
}

fn violation_counts(board: &mut Board) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for v in copper_drc::DesignRulesChecker::new(board).get_all_violations() {
        let kind = format!("{:?}", v.kind);
        let name = kind.split(['(', ' ', '{']).next().unwrap_or_default().to_lowercase();
        *counts.entry(name).or_insert(0) += 1;
    }
    counts
}

fn step_stats(
    label: &str,
    board: &mut Board,
    baseline: &BTreeMap<String, usize>,
    open: usize,
    seconds: f64,
) -> serde_json::Value {
    let now = violation_counts(board);
    let kinds: BTreeSet<&String> = now.keys().chain(baseline.keys()).collect();
    let added: BTreeMap<&String, i64> = kinds
        .into_iter()
        .map(|k| (k, now.get(k).copied().unwrap_or(0) as i64 - baseline.get(k).copied().unwrap_or(0) as i64))
        .filter(|(_, d)| *d != 0)
        .collect();
    let fixed = board
        .get_items()
        .filter(|i| matches!(i, Item::Trace(_)) && i.is_user_fixed())
        .count();
    serde_json::json!({
        "shape": label,
        "new violations": added.values().filter(|d| **d > 0).sum::<i64>(),
        "violation changes": serde_json::to_value(&added).unwrap_or_default(),
        "airlines": open,
        "fixed traces": fixed,
        "step s": (seconds * 10.0).round() / 10.0,
    })
}

/// What a violation is between, not where: kind and the nets on each side.
type ViolationKey = (String, Vec<String>);

fn violation_keys(board: &mut Board) -> Vec<(ViolationKey, ItemId, Option<ItemId>)> {
    let net_names = |board: &Board, id: Option<ItemId>| -> String {
        id.and_then(|id| board.get_item(id))
            .map(|item| {
                item.net_nos()
                    .iter()
                    .filter_map(|n| board.rules.nets.get(*n).map(|net| net.name.clone()))
                    .collect::<Vec<_>>()
                    .join("+")
            })
            .unwrap_or_default()
    };
    let violations = copper_drc::DesignRulesChecker::new(board).get_all_violations();
    violations
        .into_iter()
        .map(|v| {
            let kind = format!("{:?}", v.kind);
            let kind = kind.split(['(', ' ', '{']).next().unwrap_or_default().to_lowercase();
            let mut nets = vec![net_names(board, Some(v.first_item)), net_names(board, v.second_item)];
            nets.sort();
            ((kind, nets), v.first_item, v.second_item)
        })
        .collect()
}

/// The differential pairs, by net name: the last DP, _P or + swapped for DM, _N or - names a net
/// too (USB_DP_C and USB_DM_C).
fn diff_pairs(board: &Board) -> Vec<(String, String)> {
    let names: BTreeSet<String> = (1..=board.rules.nets.max_net_number())
        .filter_map(|n| board.rules.nets.get(n).map(|net| net.name.clone()))
        .collect();
    let mut pairs = Vec::new();
    for name in &names {
        for (p, n) in [("DP", "DM"), ("_P", "_N"), ("+", "-")] {
            if let Some(i) = name.rfind(p) {
                let partner = format!("{}{n}{}", &name[..i], &name[i + p.len()..]);
                if names.contains(&partner) {
                    pairs.push((name.clone(), partner));
                }
            }
        }
    }
    pairs
}

const SIMPLIFY_MM: f64 = 0.01;
const OPT_CHANGED_AREA_MS: i32 = 1000;

/// The items in violations beyond `allowed`, counted per kind and net pair, in the checker's order.
fn new_violation_items(board: &mut Board, allowed: &BTreeMap<ViolationKey, usize>) -> Vec<ItemId> {
    let mut seen: BTreeMap<ViolationKey, usize> = BTreeMap::new();
    let mut items = Vec::new();
    for (key, first, second) in violation_keys(board) {
        let count = seen.entry(key.clone()).or_insert(0);
        *count += 1;
        if *count <= allowed.get(&key).copied().unwrap_or(0) {
            continue;
        }
        tracing::debug!(
            "new {key:?} between {:?} and {:?}",
            board.get_item(first).map(Item::kind),
            second.and_then(|id| board.get_item(id)).map(Item::kind)
        );
        for id in std::iter::once(first).chain(second) {
            if !items.contains(&id) {
                items.push(id);
            }
        }
    }
    items
}

/// Rips the traces and vias in violations beyond `allowed` (counted per kind and net pair), never
/// on a differential pair's nets, then lets the router close the gaps. Returns how many items were
/// ripped.
fn repair(
    board: &mut Board,
    allowed: &BTreeMap<ViolationKey, usize>,
    protected: &BTreeSet<i32>,
    settings: &copper_settings::RouterSettings,
    deadline_ms: i32,
    backend: &'static dyn copper_core::RouterBackend,
) -> usize {
    let doomed: BTreeSet<ItemId> = new_violation_items(board, allowed)
        .into_iter()
        .filter(|id| {
            board.get_item(*id).is_some_and(|item| {
                (item.is_trace() || matches!(item, Item::Via(_)))
                    && !item.net_nos().iter().any(|n| protected.contains(n))
            })
        })
        .collect();
    for &id in &doomed {
        if let Some(item) = board.get_item_mut(id) {
            item.set_fixed_state(copper_board::structure::FixedState::Unfixed);
        }
        board.remove_item(id);
    }
    if !doomed.is_empty() {
        let per_mm = board.communication.get_resolution(Unit::Mm);
        let simplified = copper_morph::simplify_traces(board, SIMPLIFY_MM * per_mm);
        tracing::info!("repair: {simplified} traces simplified");
        let mut quick = settings.clone();
        quick.set_run_optimizer(false);
        // Unbounded, the via optimizer can shuffle vias on warped copper for as long as it is let.
        if quick.opt_changed_area_ms.unwrap_or(0) <= 0 {
            quick.opt_changed_area_ms = Some(OPT_CHANGED_AREA_MS);
        }
        let started = std::time::Instant::now();
        let progress = copper_core::SyncProgressSink::new(move |event| {
            tracing::debug!("repair {:.1} s: {event:?}", started.elapsed().as_secs_f64());
        });
        let ctx = copper_core::Ctx {
            settings: &quick,
            cancel: copper_core::CancelToken::with_timeout(std::time::Duration::from_millis(
                u64::try_from(deadline_ms).unwrap_or(0),
            )),
            progress: &progress,
            budget: crate::ops::route::budget_for(&quick),
            backend,
        };
        let open_before = copper_drc::all_airlines(board).len();
        match copper_core::RoutingPipeline::run(board, &ctx) {
            Ok(result) => tracing::info!(
                "repair reroute: {open_before} airlines before, {} after; {} passes, timed out {}; {}",
                copper_drc::all_airlines(board).len(),
                result.pipeline.router_passes_completed,
                result.timed_out,
                result.unrouted_report.lines().take(6).collect::<Vec<_>>().join(" | ")
            ),
            Err(error) => tracing::warn!("repair reroute: {error}"),
        }
    }
    doomed.len()
}

/// Each pair's smallest gap between its two nets' traces on a shared layer, and the difference of
/// their copper lengths, in millimetres.
fn pair_metrics(board: &Board, pairs: &[(String, String)], per_mm: f64) -> serde_json::Value {
    let traces = |name: &str| -> Vec<(usize, f64, Vec<FloatPoint>)> {
        let Some(net) = board.rules.nets.get_by_name(name).first().map(|n| n.net_number) else {
            return Vec::new();
        };
        board
            .get_items()
            .filter_map(|item| match item {
                Item::Trace(t) if item.contains_net(net) => {
                    let pl = t.polyline();
                    let pts = (0..pl.corner_count()).filter_map(|i| pl.corner(i).map(|p| p.to_float())).collect();
                    Some((t.get_layer(), f64::from(t.get_half_width()), pts))
                }
                _ => None,
            })
            .collect()
    };
    let length = |ts: &[(usize, f64, Vec<FloatPoint>)]| -> f64 {
        ts.iter()
            .flat_map(|(_, _, p)| p.windows(2).map(|w| (w[0].x - w[1].x).hypot(w[0].y - w[1].y)))
            .sum()
    };
    let mut out = serde_json::Map::new();
    for (p, n) in pairs {
        let (a, b) = (traces(p), traces(n));
        let mut gap = f64::INFINITY;
        let mut at = FloatPoint::new(0.0, 0.0);
        for (la, wa, pa) in &a {
            for (lb, wb, pb) in &b {
                if la != lb {
                    continue;
                }
                for sa in pa.windows(2) {
                    for sb in pb.windows(2) {
                        let g = segment_distance(sa[0], sa[1], sb[0], sb[1]) - wa - wb;
                        if g < gap {
                            gap = g;
                            at = FloatPoint::new((sa[0].x + sa[1].x) / 2.0, (sa[0].y + sa[1].y) / 2.0);
                        }
                    }
                }
            }
        }
        out.insert(
            format!("{p}/{n}"),
            serde_json::json!({
                "gap mm": if gap.is_finite() { ((gap / per_mm) * 1000.0).round() / 1000.0 } else { -1.0 },
                "skew mm": (((length(&a) - length(&b)).abs() / per_mm) * 1000.0).round() / 1000.0,
                "gap at": [at.x.round(), at.y.round()],
            }),
        );
    }
    serde_json::Value::Object(out)
}

fn segment_distance(a: FloatPoint, b: FloatPoint, c: FloatPoint, d: FloatPoint) -> f64 {
    let point = |p: FloatPoint, s: FloatPoint, e: FloatPoint| {
        let (dx, dy) = (e.x - s.x, e.y - s.y);
        let len = dx * dx + dy * dy;
        let t = if len == 0.0 { 0.0 } else { (((p.x - s.x) * dx + (p.y - s.y) * dy) / len).clamp(0.0, 1.0) };
        (p.x - s.x - t * dx).hypot(p.y - s.y - t * dy)
    };
    let cross = |o: FloatPoint, p: FloatPoint, q: FloatPoint| (p.x - o.x) * (q.y - o.y) - (p.y - o.y) * (q.x - o.x);
    let (d1, d2, d3, d4) = (cross(c, d, a), cross(c, d, b), cross(a, b, c), cross(a, b, d));
    if d1 * d2 < 0.0 && d3 * d4 < 0.0 {
        return 0.0;
    }
    point(a, c, d).min(point(b, c, d)).min(point(c, a, b)).min(point(d, a, b))
}
