use copper_settings::sources::cli::{
    DeSlots, LegacyBridge, apply_command_line_arguments, classify_de_arguments,
};
use copper_settings::sources::{CliSettings, DefaultSettings};
use copper_settings::{
    BoardUpdateStrategy, HostEnvironment, ItemSelectionStrategy, RouterSettings, SettingsMerger,
    SettingsSource, SourceKind, priority,
};

fn host() -> HostEnvironment {
    HostEnvironment::with_processors(4)
}

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| (*a).to_string()).collect()
}

fn cli(args: &[&str]) -> CliSettings {
    CliSettings::new(&argv(args))
}

fn bridge(args: &[&str]) -> LegacyBridge {
    apply_command_line_arguments(&argv(args))
}

fn de(args: &[&str]) -> DeSlots {
    classify_de_arguments(&argv(args))
}

struct JsonTestSource {
    settings: RouterSettings,
}

impl SettingsSource for JsonTestSource {
    fn get_settings(&self) -> Option<&RouterSettings> {
        Some(&self.settings)
    }

    fn get_source_name(&self) -> String {
        "JSON test source".to_string()
    }

    fn get_priority(&self) -> i32 {
        priority::JSON_FILE
    }

    fn kind(&self) -> SourceKind {
        SourceKind::JsonFile
    }
}

fn json_source_disabling_the_router() -> JsonTestSource {
    let mut settings = RouterSettings::new();
    settings.enabled = Some(false);
    JsonTestSource { settings }
}

#[test]
fn priority_and_source_name() {
    let empty = cli(&[]);
    assert_eq!(empty.get_priority(), 60);
    assert_eq!(empty.get_priority(), priority::CLI);
    assert_eq!(empty.get_source_name(), "CLI Arguments");
    assert_eq!(empty.kind(), SourceKind::Cli);
    assert!(empty.get_settings().is_some());
}

#[test]
fn a_long_option_reaches_the_router_settings() {
    let cli = cli(&["--router.neck_width_um=250"]);
    assert_eq!(cli.get_settings().unwrap().get_neck_width_um(), 250.0);
    assert_eq!(
        cli.get_parsed_arguments().get("router.neck_width_um"),
        Some(&"250".to_string())
    );
}

#[test]
fn a_long_option_outside_the_router_namespace_is_ignored() {
    let cli = cli(&["--gui.foo=1", "--router.max_passes=7"]);
    assert_eq!(cli.get_settings().unwrap().max_passes, Some(7));
    assert_eq!(cli.get_parsed_arguments().len(), 1);
    assert!(cli.errors().is_empty());
}

#[test]
fn a_long_option_without_an_equals_sign_is_skipped() {
    let cli = cli(&["--router.max_passes"]);
    assert_eq!(cli.get_settings().unwrap().max_passes, None);
    assert!(cli.get_parsed_arguments().is_empty());
}

#[test]
fn a_nested_long_option_is_applied_under_the_stripped_prefix() {
    let cli = cli(&[
        "--router.optimizer.max_threads=3",
        "--router.optimizer.max_search_steps=123456",
    ]);
    assert_eq!(
        cli.get_settings()
            .unwrap()
            .optimizer
            .as_ref()
            .unwrap()
            .max_threads,
        Some(3)
    );
    assert_eq!(
        cli.get_settings()
            .unwrap()
            .optimizer
            .as_ref()
            .unwrap()
            .max_search_steps,
        Some(123_456)
    );
}

#[test]
fn only_mp_and_mt_map_to_a_property() {
    assert_eq!(
        cli(&["-mp", "5"]).get_settings().unwrap().max_passes,
        Some(5)
    );
    assert_eq!(
        cli(&["-mt", "5"]).get_settings().unwrap().max_threads,
        Some(5)
    );
    assert_eq!(cli(&["-mpx", "5"]).get_settings().unwrap().max_passes, None);
    assert!(cli(&["-mpx", "5"]).get_parsed_arguments().is_empty());
    assert!(cli(&["-oit", "5"]).get_parsed_arguments().is_empty());
}

#[test]
fn a_short_flag_consumes_a_value_only_when_it_does_not_start_with_a_dash() {
    let at_end = cli(&["-mp"]);
    assert_eq!(at_end.get_settings().unwrap().max_passes, None);
    assert!(at_end.get_parsed_arguments().is_empty());
    assert_eq!(at_end.errors().len(), 1);

    let negative = cli(&["-mp", "-5"]);
    assert_eq!(negative.get_settings().unwrap().max_passes, None);
    assert!(negative.get_parsed_arguments().is_empty());
    assert_eq!(negative.errors().len(), 1);
}

#[test]
fn cli_can_disable_the_router_and_the_optimizer() {
    let merger = SettingsMerger::new(vec![
        Box::new(DefaultSettings::new(&host())),
        Box::new(cli(&[
            "--router.enabled=false",
            "--router.optimizer.enabled=false",
        ])),
    ]);
    let merged = merger.merge(&host());
    assert!(!merged.get_run_router());
    assert!(!merged.get_run_optimizer());
}

#[test]
fn de_plus_do_forces_the_router_on_over_a_lower_priority_false() {
    let merger = SettingsMerger::new(vec![
        Box::new(DefaultSettings::new(&host())),
        Box::new(json_source_disabling_the_router()),
        Box::new(cli(&["-de", "a.dsn", "-do", "a.ses"])),
    ]);
    assert!(merger.merge(&host()).get_run_router());
    assert_eq!(
        cli(&["-de", "a.dsn", "-do", "a.ses"])
            .get_settings()
            .unwrap()
            .enabled,
        Some(true)
    );
}

#[test]
fn de_alone_does_not_force_the_router_on() {
    assert_eq!(cli(&["-de", "a.dsn"]).get_settings().unwrap().enabled, None);
    assert_eq!(cli(&["-do", "a.ses"]).get_settings().unwrap().enabled, None);
}

#[test]
fn an_explicit_router_enabled_argument_beats_the_batch_mode_forcing() {
    let merger = SettingsMerger::new(vec![
        Box::new(DefaultSettings::new(&host())),
        Box::new(cli(&[
            "-de",
            "a.dsn",
            "-do",
            "a.ses",
            "--router.enabled=false",
        ])),
    ]);
    assert!(!merger.merge(&host()).get_run_router());
}

#[test]
fn an_empty_router_enabled_argument_still_counts_as_explicit() {
    let cli = cli(&["--router.enabled=", "-de", "a.dsn", "-do", "a.ses"]);
    assert_eq!(cli.get_settings().unwrap().enabled, Some(false));
}

#[test]
fn routable_layers_do_not_disable_the_router() {
    let merger = SettingsMerger::new(vec![
        Box::new(DefaultSettings::new(&host())),
        Box::new(cli(&[
            "-de",
            "a.dsn",
            "--router.layers.routable=false,true",
        ])),
    ]);
    let merged = merger.merge(&host());
    assert!(merged.get_run_router());
    assert_eq!(merged.get_layer_count(), 2);
}

#[test]
fn legacy_bridge_is_dead() {
    let args = [
        "-oit", "5", "-us", "global", "-is", "seq", "-hr", "2:3", "-inc", "GND, VCC",
    ];
    let dead = bridge(&args);
    assert_eq!(dead.optimization_improvement_threshold, Some(0.05f32));
    assert_eq!(
        dead.board_update_strategy,
        Some(BoardUpdateStrategy::GlobalOptimal)
    );
    assert_eq!(
        dead.item_selection_strategy,
        Some(ItemSelectionStrategy::Sequential)
    );
    assert_eq!(dead.hybrid_ratio.as_deref(), Some("2:3"));
    assert_eq!(
        dead.ignore_net_classes.as_deref(),
        Some(&["GND".to_string(), " VCC".to_string()][..])
    );

    let cli = cli(&args);
    let settings = cli.get_settings().unwrap();
    let optimizer = settings.optimizer.as_ref().unwrap();
    assert_eq!(optimizer.optimization_improvement_threshold, None);
    assert_eq!(optimizer.board_update_strategy, None);
    assert_eq!(optimizer.item_selection_strategy, None);
    assert_eq!(optimizer.hybrid_ratio, None);
    assert_eq!(settings.ignore_net_classes, None);
    assert!(cli.get_parsed_arguments().is_empty());
}

#[test]
fn a_router_long_option_writes_the_dead_bridge_too() {
    let dead = bridge(&["--router.enabled="]);
    assert_eq!(dead.router_enabled, Some(false));
    assert_eq!(
        cli(&["--router.enabled="]).get_settings().unwrap().enabled,
        Some(false)
    );

    let dead = bridge(&["--router.optimizer.max_threads=7"]);
    assert_eq!(dead.optimizer_max_threads, Some(7));
    let dead = bridge(&["--router.optimizer.max_threads=99999"]);
    assert_eq!(dead.optimizer_max_threads, Some(99999));
    assert_eq!(bridge(&["-mt", "99999"]).optimizer_max_threads, Some(1024));

    assert_eq!(bridge(&["--user_data_path=/tmp"]), LegacyBridge::default());
    assert_eq!(
        bridge(&["--api_server.endpoints=http://x"]),
        LegacyBridge::default()
    );
    assert_eq!(
        bridge(&["--router.no_such_field=1"]),
        LegacyBridge::default()
    );
    assert_eq!(bridge(&["--router.enabled"]), LegacyBridge::default());
}

#[test]
fn mt_feeds_two_fields_with_different_clamps() {
    let dead = bridge(&["-mt", "2000"]);
    assert_eq!(dead.optimizer_max_threads, Some(1024));

    let live = cli(&["-mt", "2000"]);
    let settings = live.get_settings().unwrap();
    assert_eq!(settings.max_threads, Some(2000));
    assert_eq!(settings.optimizer.as_ref().unwrap().max_threads, None);

    let merged = SettingsMerger::new(vec![
        Box::new(DefaultSettings::new(&host())),
        Box::new(cli(&["-mt", "2000"])),
    ])
    .merge(&host());
    assert_eq!(merged.max_threads, Some(4));
    assert_eq!(merged.optimizer.as_ref().unwrap().max_threads, Some(2));
}

#[test]
fn mt_clamps_are_asymmetrically_reachable() {
    assert_eq!(bridge(&["-mt", "-3"]).optimizer_max_threads, None);
    assert_eq!(bridge(&["-mt", "0"]).optimizer_max_threads, Some(0));
    assert_eq!(bridge(&["-mt", "1024"]).optimizer_max_threads, Some(1024));
}

#[test]
fn oit_divides_before_it_clamps() {
    assert_eq!(
        bridge(&["-oit", "0"]).optimization_improvement_threshold,
        Some(0.0f32)
    );
    assert_eq!(
        bridge(&["-oit", "-5"]).optimization_improvement_threshold,
        None
    );

    let one = bridge(&["-oit", "1"])
        .optimization_improvement_threshold
        .unwrap();
    assert_eq!(one.to_bits(), 1_008_981_770);
    let thirty_three = bridge(&["-oit", "33"])
        .optimization_improvement_threshold
        .unwrap();
    assert_eq!(thirty_three.to_bits(), 1_051_260_355);
}

#[test]
fn us_and_is_never_reject_a_value() {
    assert_eq!(
        bridge(&["-us", "HYBRID"]).board_update_strategy,
        Some(BoardUpdateStrategy::Hybrid)
    );
    assert_eq!(
        bridge(&["-us", " global "]).board_update_strategy,
        Some(BoardUpdateStrategy::GlobalOptimal)
    );
    assert_eq!(
        bridge(&["-us", "nonsense"]).board_update_strategy,
        Some(BoardUpdateStrategy::Greedy)
    );

    assert_eq!(
        bridge(&["-is", "sequestered"]).item_selection_strategy,
        Some(ItemSelectionStrategy::Sequential)
    );
    assert_eq!(
        bridge(&["-is", "RANDOMIZE"]).item_selection_strategy,
        Some(ItemSelectionStrategy::Random)
    );
    assert_eq!(
        bridge(&["-is", "nonsense"]).item_selection_strategy,
        Some(ItemSelectionStrategy::Prioritized)
    );
    assert_eq!(
        bridge(&["-hr", " 2:3 "]).hybrid_ratio.as_deref(),
        Some("2:3")
    );
}

#[test]
fn mp_decodes_on_the_bridge_and_parses_on_the_cli_path() {
    assert_eq!(bridge(&["-mp", "0x10"]).max_passes, Some(16));
    assert_eq!(bridge(&["-mp", "010"]).max_passes, Some(8));
    assert_eq!(
        cli(&["-mp", "0x10"]).get_settings().unwrap().max_passes,
        None
    );

    assert_eq!(bridge(&["-mp", "100000"]).max_passes, Some(9999));
    assert_eq!(bridge(&["-mp", "0"]).max_passes, Some(0));
    assert_eq!(bridge(&["-mp", "9999"]).max_passes, Some(9999));
}

#[test]
fn a_malformed_number_skips_the_flag_without_aborting_the_parse() {
    let bad = bridge(&["-mp", "notanumber", "-hr", "9:9"]);
    assert_eq!(bad.max_passes, None);
    assert_eq!(bad.hybrid_ratio.as_deref(), Some("9:9"));
    assert_eq!(bridge(&["-mp"]).max_passes, None);
}

#[test]
fn drc_switches_to_drc_only_mode_before_it_looks_for_a_value() {
    let drc = bridge(&["-drc"]);
    assert_eq!(drc.router_enabled, Some(false));
    assert_eq!(drc.drc_enabled, Some(true));

    let with_report = bridge(&["-drc", "report.json"]);
    assert_eq!(with_report.router_enabled, Some(false));
    assert_eq!(with_report.drc_enabled, Some(true));

    let dr = bridge(&["-dr", "x.rules"]);
    assert_eq!(dr.router_enabled, None);
    assert_eq!(dr.drc_enabled, None);
}

#[test]
fn legacy_bridge_matches_flags_by_prefix() {
    assert_eq!(bridge(&["-mpx", "5"]).max_passes, Some(5));
    assert_eq!(
        bridge(&["-incoming", "GND"]).ignore_net_classes.as_deref(),
        Some(&["GND".to_string()][..])
    );
    assert_eq!(cli(&["-mpx", "5"]).get_settings().unwrap().max_passes, None);
}

#[test]
fn inc_splits_without_trimming() {
    assert_eq!(
        bridge(&["-inc", "GND"]).ignore_net_classes.as_deref(),
        Some(&["GND".to_string()][..])
    );
    assert_eq!(
        bridge(&["-inc", "GND,"]).ignore_net_classes.as_deref(),
        Some(&["GND".to_string()][..])
    );
    assert_eq!(
        bridge(&["-inc", "GND, VCC"]).ignore_net_classes.as_deref(),
        Some(&["GND".to_string(), " VCC".to_string()][..])
    );
}

fn slots(input: Option<&str>, session: Option<&str>, rules: Option<&str>) -> DeSlots {
    DeSlots {
        initial_input_file: input.map(str::to_string),
        design_session_filename: session.map(str::to_string),
        initial_rules_file: rules.map(str::to_string),
    }
}

#[test]
fn de_classification_matrix() {
    assert_eq!(
        de(&["-de", "myboard.dsn"]),
        slots(Some("myboard.dsn"), None, None)
    );
    assert_eq!(
        de(&["-de", "myboard.dsn+myboard.ses"]),
        slots(Some("myboard.dsn"), Some("myboard.ses"), None)
    );
    assert_eq!(
        de(&["-de", "myboard.dsn+myboard.rules"]),
        slots(Some("myboard.dsn"), None, Some("myboard.rules"))
    );
    assert_eq!(
        de(&["-de", "myboard.dsn+myboard.ses+myboard.rules"]),
        slots(
            Some("myboard.dsn"),
            Some("myboard.ses"),
            Some("myboard.rules")
        )
    );
    assert_eq!(
        de(&["-de", "myboard.rules+myboard.dsn+myboard.ses"]),
        slots(
            Some("myboard.dsn"),
            Some("myboard.ses"),
            Some("myboard.rules")
        )
    );
    assert_eq!(
        de(&["-de", "myboard.dsn", "myboard.ses", "myboard.rules"]),
        slots(
            Some("myboard.dsn"),
            Some("myboard.ses"),
            Some("myboard.rules")
        )
    );
    assert_eq!(
        de(&["-de", "sonde xilinx.dsn"]),
        slots(Some("sonde xilinx.dsn"), None, None)
    );
    assert_eq!(
        de(&["-de", "myboard.dsn+myboard.ses", "myboard.rules"]),
        slots(
            Some("myboard.dsn"),
            Some("myboard.ses"),
            Some("myboard.rules")
        )
    );
    assert_eq!(
        de(&["-de", "/path/to/myboard.dsn+/path/to/myboard.ses"]),
        slots(
            Some("/path/to/myboard.dsn"),
            Some("/path/to/myboard.ses"),
            None
        )
    );
    assert_eq!(
        de(&["-de", "myboard.DSN+myboard.SES+myboard.RULES"]),
        slots(
            Some("myboard.DSN"),
            Some("myboard.SES"),
            Some("myboard.RULES")
        )
    );
    assert_eq!(
        de(&["-de", "myboard.Dsn+myboard.Ses"]),
        slots(Some("myboard.Dsn"), Some("myboard.Ses"), None)
    );
    assert_eq!(
        de(&["-de", "board1.dsn+board2.dsn"]),
        slots(Some("board2.dsn"), None, None)
    );
    assert_eq!(
        de(&["-de", "a.ses+b.ses"]),
        slots(None, Some("b.ses"), None)
    );
    assert_eq!(
        de(&["-de", "a.rules+b.rules"]),
        slots(None, None, Some("b.rules"))
    );
    assert_eq!(
        de(&["-de", "myboard.ses"]),
        slots(None, Some("myboard.ses"), None)
    );
    assert_eq!(
        de(&["-de", "myboard.rules"]),
        slots(None, None, Some("myboard.rules"))
    );
    assert_eq!(de(&["-de"]), slots(None, None, None));
    assert_eq!(
        de(&[
            "-de",
            "myboard.dsn+myboard.ses",
            "-do",
            "output.ses",
            "-mp",
            "10"
        ]),
        slots(Some("myboard.dsn"), Some("myboard.ses"), None)
    );
}

#[test]
fn de_entries_are_trimmed() {
    assert_eq!(
        de(&["-de", " a.dsn + b.ses "]),
        slots(Some("a.dsn"), Some("b.ses"), None)
    );
    assert_eq!(de(&["-de", "+"]), slots(None, None, None));
    assert_eq!(de(&["-de", "a.dsn+"]), slots(Some("a.dsn"), None, None));
}

#[test]
fn de_drops_an_unknown_extension_rather_than_guessing() {
    assert_eq!(de(&["-de", "a.txt"]), slots(None, None, None));
    assert_eq!(
        de(&["-de", "a.txt+b.dsn"]),
        slots(Some("b.dsn"), None, None)
    );
}

#[test]
fn de_json_is_design_input_until_a_dsn_appears() {
    assert_eq!(de(&["-de", "a.json"]), slots(Some("a.json"), None, None));
    assert_eq!(
        de(&["-de", "a.dsn+b.json"]),
        slots(Some("a.dsn"), Some("b.json"), None)
    );
    assert_eq!(
        de(&["-de", "a.json+b.json"]),
        slots(Some("a.json"), Some("b.json"), None)
    );
    assert_eq!(
        de(&["-de", "a.json+b.dsn"]),
        slots(Some("b.dsn"), None, None)
    );
}

#[test]
fn de_takes_an_existing_path_verbatim_even_with_a_plus_in_it() {
    struct TempDir(std::path::PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    let dir = TempDir(std::env::temp_dir().join(format!(
        "copper-settings-de-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    )));
    std::fs::create_dir_all(&dir.0).expect("temp dir");
    let path = dir.0.join("board+rev2.dsn");
    std::fs::write(&path, b"").expect("temp file");
    let name = path.to_str().expect("utf-8 temp path").to_string();

    assert_eq!(de(&["-de", &name]), slots(Some(&name), None, None));

    let ghost = format!("{name}.ghost");
    assert_eq!(de(&["-de", &ghost]), slots(None, None, None));
}

#[test]
fn de_is_prefix_matched_and_never_double_dashed() {
    assert_eq!(de(&["-dexyz", "a.dsn"]), slots(Some("a.dsn"), None, None));
    assert_eq!(de(&["--de", "a.dsn"]), slots(None, None, None));
    assert_eq!(
        de(&["-de", "a.dsn", "-de", "b.dsn"]),
        slots(Some("b.dsn"), None, None)
    );
    assert_eq!(de(&[]), slots(None, None, None));
}

fn cli_native(args: &[&str]) -> CliSettings {
    CliSettings::new_with_set_alias(&argv(args))
}

fn max_passes(source: &CliSettings) -> Option<i32> {
    source
        .get_settings()
        .expect("a source always answers")
        .max_passes
}

#[test]
fn the_java_exact_constructor_ignores_set_the_way_the_jar_does() {
    for args in [
        &["--set", "router.max_passes=7"][..],
        &["--set=router.max_passes=7"][..],
    ] {
        let source = cli(args);
        assert_eq!(
            max_passes(&source),
            None,
            "`CliSettings::new` must leave `--set` unread, exactly as the jar does: {args:?}"
        );
    }
}

#[test]
fn the_set_alias_is_exactly_the_dotted_spelling() {
    let dotted = cli(&["--router.max_passes=7"]);
    assert_eq!(max_passes(&dotted), Some(7), "the oracle itself");
    for args in [
        &["--set", "router.max_passes=7"][..],
        &["--set=router.max_passes=7"][..],
    ] {
        let source = cli_native(args);
        assert_eq!(
            max_passes(&source),
            max_passes(&dotted),
            "the alias must answer what the dotted spelling answers: {args:?}"
        );
    }

    let twice = cli_native(&[
        "--set",
        "router.max_passes=7",
        "--set",
        "router.max_passes=9",
    ]);
    assert_eq!(max_passes(&twice), Some(9));

    let ratio = cli_native(&["--set", "router.optimizer.hybrid_ratio=1:2"]);
    assert_eq!(
        ratio
            .get_settings()
            .expect("settings")
            .optimizer
            .as_ref()
            .and_then(|o| o.hybrid_ratio.clone()),
        Some("1:2".to_string())
    );

    let empty = cli_native(&[]);
    for args in [
        &["--set", "gui.theme=dark"][..],
        &["--set", "router.max_passes"][..],
        &["--settings", "s.json"][..],
    ] {
        let source = cli_native(args);
        assert_eq!(
            source.get_settings().expect("settings"),
            empty.get_settings().expect("settings"),
            "the alias must change nothing here: {args:?}"
        );
    }

    let forced = cli_native(&["-de", "a.dsn", "-do", "b.ses"]);
    assert_eq!(
        forced.get_settings().expect("settings").enabled,
        Some(true),
        "control: `-de` + `-do` with no explicit `router.enabled` forces it on (:77-83)"
    );
    let disarmed = cli_native(&[
        "-de",
        "a.dsn",
        "-do",
        "b.ses",
        "--set",
        "router.enabled=false",
    ]);
    assert_eq!(
        disarmed.get_settings().expect("settings").enabled,
        Some(false),
        "the alias must arm `hasExplicitRouterEnabledArgument` exactly as the dotted spelling does"
    );
}
