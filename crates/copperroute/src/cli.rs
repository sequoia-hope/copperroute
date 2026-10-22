use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

use crate::ops::route::OutputFormat;
use copper_core::RouterBackend;
use copper_governor::MemoryCeiling;

#[derive(Parser, Debug)]
#[command(
    name = "copperroute",
    version,
    propagate_version = true,
    about = "Headless PCB autorouter"
)]
pub struct Cli {
    /// Raise the log level: -v for debug, -vv for trace.
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,
    /// Set the log level outright: off, error, warn, info, debug, trace.
    #[arg(long, global = true)]
    pub log_level: Option<String>,
    /// A settings JSON, applied below the design's own settings and below --set.
    #[arg(long, global = true, value_name = "FILE")]
    pub settings: Option<PathBuf>,
    /// Scheduling niceness for route and mcp, 0..=19. Defaults to $COPPERROUTE_NICE, else 10.
    #[arg(long, global = true)]
    pub nice: Option<i32>,
    /// Stop routing once resident memory reaches this size (512M, 4G), keeping what is routed.
    /// `auto` is half the memory available at start; `off` disables the ceiling. Defaults to
    /// $COPPERROUTE_MAX_MEMORY, else auto.
    #[arg(long, global = true, value_parser = MemoryCeiling::parse)]
    pub max_memory: Option<MemoryCeiling>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Route a board and write the session.
    Route(RouteArgs),
    /// Check a board against its design rules and write the KiCad DRC report.
    Drc(DrcArgs),
    /// Print the board summary as JSON.
    Info(InfoArgs),
    /// Move parts on a routed board toward target displacements, bending the copper with them,
    /// and write the session with the new placement.
    Settle(SettleArgs),
    /// Serve the routing tools over JSON-RPC on stdin/stdout.
    Mcp,
}

#[derive(Args, Debug)]
pub struct RouteArgs {
    /// A Specctra DSN, a KiCad board JSON, or a KiCad .kicad_pcb board.
    pub input: PathBuf,
    /// Where to write the session: a .ses (Specctra) or a .json (KiCad session).
    #[arg(short, long, value_parser = parse_output_path)]
    pub output: PathBuf,
    /// A Specctra .rules file. Without it, a .rules beside the input is used when present.
    #[arg(long)]
    pub rules: Option<PathBuf>,
    /// A session to import before routing, so the run continues from it.
    #[arg(long)]
    pub ses: Option<PathBuf>,
    /// A KiCad .kicad_pro whose design rules are applied before routing.
    #[arg(long)]
    pub kicad_project: Option<PathBuf>,
    /// How many routing passes to run; 0 means unlimited.
    #[arg(long)]
    pub max_passes: Option<u32>,
    /// Wall-clock budget for the whole job, as hh:mm:ss or seconds.
    #[arg(long)]
    pub timeout: Option<String>,
    /// Write the result manifest here.
    #[arg(long)]
    pub result_json: Option<PathBuf>,
    /// The routing approach to run.
    #[arg(long, default_value = copper_core::DEFAULT_BACKEND, value_parser = parse_backend)]
    pub backend: &'static dyn RouterBackend,
    /// Override one setting: --set router.<section>.<field>=<value>. Repeatable.
    /// router.max_items=N stops the optimizer as well as the router; --max-passes stops
    /// only the router.
    #[arg(long = "set", value_name = "FIELD=VALUE")]
    pub set: Vec<String>,
    /// Write SVG frames showing maze rooms over the routed PCB.
    #[arg(long, value_name = "DIR")]
    pub visualize: Option<PathBuf>,
    /// Capture one visualization frame for every N maze steps; route commits are always kept.
    #[arg(long, default_value_t = 1, requires = "visualize")]
    pub visualize_every: u64,
    /// Stop writing visualization frames after this many images; routing continues.
    #[arg(long, default_value_t = 1_000, requires = "visualize")]
    pub visualize_max_frames: u64,
    #[arg(long, default_value_t = 1280, requires = "visualize")]
    pub visualize_width: u32,
    #[arg(long, default_value_t = 720, requires = "visualize")]
    pub visualize_height: u32,
}

#[derive(Args, Debug)]
pub struct DrcArgs {
    /// A Specctra DSN, a KiCad board JSON, or a KiCad .kicad_pcb board.
    pub input: PathBuf,
    /// A session to apply before checking, so the report describes the routed board.
    #[arg(long)]
    pub ses: Option<PathBuf>,
    /// A Specctra .rules file to apply before checking.
    #[arg(long)]
    pub rules: Option<PathBuf>,
    /// A KiCad .kicad_pro whose design rules are applied before checking.
    #[arg(long)]
    pub kicad_project: Option<PathBuf>,
    /// Where to write the report; stdout when omitted.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct SettleArgs {
    /// A Specctra DSN, a KiCad board JSON, or a KiCad .kicad_pcb board.
    pub input: PathBuf,
    /// The routed session to start from.
    #[arg(long)]
    pub ses: PathBuf,
    /// JSON object of component reference to [dx, dy] in millimetres.
    #[arg(long)]
    pub targets: PathBuf,
    /// Where to write the settled session (.ses), placement included.
    #[arg(short, long)]
    pub output: PathBuf,
    /// A KiCad .kicad_pro whose design rules are applied.
    #[arg(long)]
    pub kicad_project: Option<PathBuf>,
    /// Allow ripping routed copper that is in the way; it then needs rerouting.
    #[arg(long)]
    pub soft: bool,
    /// Largest step per part per cycle, in millimetres.
    #[arg(long, default_value_t = 0.05)]
    pub step_mm: f64,
    #[arg(long, default_value_t = 600)]
    pub max_cycles: usize,
    /// Clearance kept beyond the rules, in millimetres.
    #[arg(long, default_value_t = 0.015)]
    pub guard_mm: f64,
}

#[derive(Args, Debug)]
pub struct InfoArgs {
    /// A Specctra DSN, a KiCad board JSON, or a KiCad .kicad_pcb board.
    pub input: PathBuf,
    /// A KiCad .kicad_pro whose design rules are applied before summarising.
    #[arg(long)]
    pub kicad_project: Option<PathBuf>,
}

fn parse_backend(value: &str) -> Result<&'static dyn RouterBackend, String> {
    copper_core::backend(value).ok_or_else(|| {
        let known: Vec<_> = copper_core::backends()
            .iter()
            .map(|b| format!("{} ({})", b.name(), b.summary()))
            .collect();
        format!("unknown backend '{value}'; known: {}", known.join(", "))
    })
}

fn parse_output_path(value: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    match OutputFormat::from_path(&path) {
        Some(_) => Ok(path),
        None => Err(format!(
            "'{value}' must end in .ses (a Specctra session) or .json (a KiCad session)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_route_with_max_passes_timeout_and_set() {
        let cli = Cli::try_parse_from([
            "copperroute",
            "route",
            "a.dsn",
            "-o",
            "b.ses",
            "--max-passes",
            "3",
            "--timeout",
            "0:05:00",
            "--set",
            "router.scoring.via_costs=1",
        ])
        .unwrap();
        let Command::Route(r) = cli.command else {
            panic!("expected route")
        };
        assert_eq!(r.input, PathBuf::from("a.dsn"));
        assert_eq!(r.max_passes, Some(3));
        assert_eq!(r.timeout.as_deref(), Some("0:05:00"));
        assert_eq!(r.set, vec!["router.scoring.via_costs=1".to_string()]);
    }

    #[test]
    fn the_output_must_be_a_session_extension() {
        for bad in ["b.dsn", "b.txt", "b"] {
            let error =
                Cli::try_parse_from(["copperroute", "route", "a.dsn", "-o", bad]).expect_err(bad);
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::ValueValidation,
                "{bad}"
            );
        }
        for good in ["b.ses", "b.json", "B.SES"] {
            Cli::try_parse_from(["copperroute", "route", "a.dsn", "-o", good]).expect(good);
        }
    }

    #[test]
    fn the_dead_flags_are_gone() {
        for flag in [
            "--threads",
            "--kicad-json",
            "--optimizer-improvement-threshold",
            "--update-strategy",
            "--hybrid-ratio",
            "--item-selection",
            "--ignore-net-classes",
        ] {
            let error =
                Cli::try_parse_from(["copperroute", "route", "a.dsn", "-o", "b.ses", flag, "1"])
                    .expect_err(flag);
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::UnknownArgument,
                "{flag}"
            );
        }
        let error = Cli::try_parse_from(["copperroute", "-de", "a.dsn", "-do", "b.ses"])
            .expect_err("the legacy form");
        assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
    }

    #[test]
    fn parses_bounded_routing_visualization() {
        let cli = Cli::try_parse_from([
            "copperroute",
            "route",
            "a.dsn",
            "-o",
            "b.ses",
            "--visualize",
            "frames",
            "--visualize-every",
            "25",
            "--visualize-max-frames",
            "80",
        ])
        .unwrap();
        let Command::Route(route) = cli.command else {
            panic!("expected route")
        };
        assert_eq!(route.visualize, Some(PathBuf::from("frames")));
        assert_eq!(route.visualize_every, 25);
        assert_eq!(route.visualize_max_frames, 80);
        assert_eq!((route.visualize_width, route.visualize_height), (1280, 720));
    }

    #[test]
    fn parses_drc_and_info_and_mcp() {
        let cli = Cli::try_parse_from([
            "copperroute",
            "drc",
            "a.dsn",
            "--kicad-project",
            "p.kicad_pro",
        ])
        .unwrap();
        let Command::Drc(d) = cli.command else {
            panic!("expected drc")
        };
        assert_eq!(d.kicad_project, Some(PathBuf::from("p.kicad_pro")));
        assert!(matches!(
            Cli::try_parse_from(["copperroute", "info", "a.dsn"])
                .unwrap()
                .command,
            Command::Info(_)
        ));
        assert!(matches!(
            Cli::try_parse_from(["copperroute", "mcp"]).unwrap().command,
            Command::Mcp
        ));
    }

    #[test]
    fn backend_defaults_to_maze_and_rejects_unknown_names() {
        let parse = |extra: &[&str]| {
            let base = ["copperroute", "route", "a.dsn", "-o", "b.ses"];
            Cli::try_parse_from(base.iter().chain(extra))
        };
        let Command::Route(r) = parse(&[]).unwrap().command else {
            panic!("expected route")
        };
        assert_eq!(r.backend.name(), "maze");
        let Command::Route(r) = parse(&["--backend", "none"]).unwrap().command else {
            panic!("expected route")
        };
        assert_eq!(r.backend.name(), "none");
        assert!(parse(&["--backend", "quantum"]).is_err());
    }

    #[test]
    fn governance_flags_are_global() {
        let cli = Cli::try_parse_from([
            "copperroute",
            "route",
            "a.dsn",
            "-o",
            "b.ses",
            "--nice",
            "15",
            "--max-memory",
            "2G",
        ])
        .unwrap();
        assert_eq!(cli.nice, Some(15));
        assert_eq!(cli.max_memory, Some(MemoryCeiling::Bytes(2 << 30)));
        let cli = Cli::try_parse_from(["copperroute", "--max-memory", "off", "mcp"]).unwrap();
        assert_eq!(cli.max_memory, Some(MemoryCeiling::Off));
        assert!(Cli::try_parse_from(["copperroute", "--max-memory", "lots", "mcp"]).is_err());
    }

    #[test]
    fn settings_is_global_and_names_a_json_file() {
        let cli =
            Cli::try_parse_from(["copperroute", "--settings", "s.json", "info", "a.dsn"]).unwrap();
        assert_eq!(cli.settings, Some(PathBuf::from("s.json")));
    }
}
