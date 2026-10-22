use copper_settings::prelude::*;
use copper_settings::{BoardUpdateStrategy, HostEnvironment};

#[test]
fn default_has_every_field_none() {
    let rs = RouterSettings::default();
    assert_eq!(rs.enabled, None);
    assert_eq!(rs.algorithm, None);
    assert_eq!(rs.fanout, None);
    assert_eq!(rs.copper_to_edge_clearance_um, None);
    assert_eq!(rs.hole_clearance_um, None);
    assert_eq!(rs.neck_width_um, None);
    assert_eq!(rs.strict_drc, None);
    assert_eq!(rs.job_timeout_string, None);
    assert_eq!(rs.max_passes, None);
    assert_eq!(rs.max_items, None);
    assert_eq!(rs.layers, None);
    assert_eq!(rs.save_intermediate_stages, None);
    assert_eq!(rs.ignore_net_classes, None);
    assert_eq!(rs.trace_pull_tight_accuracy, None);
    assert_eq!(rs.vias_allowed, None);
    assert_eq!(rs.automatic_neckdown, None);
    assert_eq!(rs.optimizer, None);
    assert_eq!(rs.scoring, None);
    assert_eq!(rs.max_threads, None);
    assert_eq!(rs.result_json_path, None);
}

#[test]
fn new_allocates_the_three_nested_objects_like_javas_no_arg_constructor() {
    let rs = RouterSettings::new();
    assert_eq!(rs.fanout, Some(FanoutSettings::default()));
    assert_eq!(rs.optimizer, Some(OptimizerSettings::default()));
    assert_eq!(rs.scoring, Some(ScoringSettings::default()));

    assert_eq!(rs.enabled, None);
    assert_eq!(rs.algorithm, None);
    assert_eq!(rs.copper_to_edge_clearance_um, None);
    assert_eq!(rs.hole_clearance_um, None);
    assert_eq!(rs.neck_width_um, None);
    assert_eq!(rs.strict_drc, None);
    assert_eq!(rs.job_timeout_string, None);
    assert_eq!(rs.max_passes, None);
    assert_eq!(rs.max_items, None);
    assert_eq!(rs.layers, None);
    assert_eq!(rs.save_intermediate_stages, None);
    assert_eq!(rs.ignore_net_classes, None);
    assert_eq!(rs.trace_pull_tight_accuracy, None);
    assert_eq!(rs.vias_allowed, None);
    assert_eq!(rs.automatic_neckdown, None);
    assert_eq!(rs.max_threads, None);
    assert_eq!(rs.result_json_path, None);
}

#[test]
fn new_and_default_disagree() {
    assert_ne!(RouterSettings::new(), RouterSettings::default());
}

#[test]
fn renamed_keys_appear_in_serialized_output() {
    let mut rs = RouterSettings::new();
    rs.vias_allowed = Some(true);
    rs.job_timeout_string = Some("5m".to_string());
    rs.result_json_path = Some("/tmp/x.json".to_string());
    rs.optimizer
        .as_mut()
        .unwrap()
        .optimization_improvement_threshold = Some(0.5);
    rs.optimizer.as_mut().unwrap().timeout_string = Some("10m".to_string());
    rs.fanout.as_mut().unwrap().timeout_string = Some("1m".to_string());

    let value = serde_json::to_value(&rs).expect("RouterSettings must serialize");
    let obj = value.as_object().expect("top level must be a JSON object");

    assert_eq!(obj.get("allowed_via_types"), Some(&serde_json::json!(true)));
    assert_eq!(obj.get("job_timeout"), Some(&serde_json::json!("5m")));
    assert_eq!(
        obj.get("result_json"),
        Some(&serde_json::json!("/tmp/x.json"))
    );
    assert!(!obj.contains_key("vias_allowed"));
    assert!(!obj.contains_key("jobTimeoutString"));
    assert!(!obj.contains_key("resultJsonPath"));

    let optimizer = obj
        .get("optimizer")
        .and_then(|v| v.as_object())
        .expect("optimizer must be present and an object");
    assert_eq!(
        optimizer.get("improvement_threshold"),
        Some(&serde_json::json!(0.5))
    );
    assert_eq!(optimizer.get("timeout"), Some(&serde_json::json!("10m")));

    let fanout = obj
        .get("fanout")
        .and_then(|v| v.as_object())
        .expect("fanout must be present and an object");
    assert_eq!(fanout.get("timeout"), Some(&serde_json::json!("1m")));
}

#[test]
fn trace_pull_tight_accuracy_accepts_both_the_current_key_and_its_alternate() {
    let a: RouterSettings = serde_json::from_str(r#"{"trace_pull_tight_accuracy":7}"#).unwrap();
    let b: RouterSettings = serde_json::from_str(r#"{"tracePullTightAccuracy":7}"#).unwrap();
    assert_eq!(a.trace_pull_tight_accuracy, Some(7));
    assert_eq!(b.trace_pull_tight_accuracy, Some(7));
}

#[test]
fn automatic_neckdown_accepts_both_the_current_key_and_its_alternate() {
    let a: RouterSettings = serde_json::from_str(r#"{"automatic_neckdown":true}"#).unwrap();
    let b: RouterSettings = serde_json::from_str(r#"{"automaticNeckdown":true}"#).unwrap();
    assert_eq!(a.automatic_neckdown, Some(true));
    assert_eq!(b.automatic_neckdown, Some(true));
}

#[test]
fn scoring_via_costs_accepts_both_the_current_key_and_its_alternate() {
    let a: ScoringSettings = serde_json::from_str(r#"{"via_costs":9}"#).unwrap();
    let b: ScoringSettings = serde_json::from_str(r#"{"viaCosts":9}"#).unwrap();
    assert_eq!(a.via_costs, Some(9));
    assert_eq!(b.via_costs, Some(9));
}

#[test]
fn scoring_start_ripup_costs_accepts_both_the_current_key_and_its_alternate() {
    let a: ScoringSettings = serde_json::from_str(r#"{"start_ripup_costs":11}"#).unwrap();
    let b: ScoringSettings = serde_json::from_str(r#"{"startRipupCosts":11}"#).unwrap();
    assert_eq!(a.start_ripup_costs, Some(11));
    assert_eq!(b.start_ripup_costs, Some(11));
}

#[test]
fn fanout_ripup_allowed_accepts_both_the_current_key_and_its_alternate() {
    let a: FanoutSettings = serde_json::from_str(r#"{"ripup_allowed":true}"#).unwrap();
    let b: FanoutSettings = serde_json::from_str(r#"{"ripupAllowed":true}"#).unwrap();
    assert_eq!(a.ripup_allowed, Some(true));
    assert_eq!(b.ripup_allowed, Some(true));
}

#[test]
fn transient_fields_never_appear_in_serialized_output() {
    let mut rs = RouterSettings::new();
    rs.max_items = Some(42);
    rs.layers = Some(vec![LayerSettings::with_bend_cost(
        Some(true),
        Some(false),
        Some(1.5),
    )]);
    rs.save_intermediate_stages = Some(true);
    rs.ignore_net_classes = Some(vec!["a".to_string(), "b".to_string()]);

    let value = serde_json::to_value(&rs).expect("RouterSettings must serialize");
    let obj = value.as_object().unwrap();
    assert!(!obj.contains_key("max_items"));
    assert!(!obj.contains_key("layers"));
    assert!(!obj.contains_key("save_intermediate_stages"));
    assert!(!obj.contains_key("ignore_net_classes"));
}

#[test]
fn layers_is_deserialized_even_though_its_never_serialized() {
    let input = serde_json::json!({
        "max_items": 99,
        "layers": [{"routable": true, "preferred_direction_horizontal": false, "bend_cost": 2.5}],
        "save_intermediate_stages": true,
        "ignore_net_classes": ["x", "y"],
    });
    let rs: RouterSettings = serde_json::from_value(input).unwrap();

    assert_eq!(rs.max_items, None);
    assert_eq!(rs.save_intermediate_stages, None);
    assert_eq!(rs.ignore_net_classes, None);
    assert_eq!(
        rs.layers,
        Some(vec![LayerSettings::with_bend_cost(
            Some(true),
            Some(false),
            Some(2.5)
        )])
    );
}

#[test]
fn optimizer_transient_fields_never_appear_in_serialized_output() {
    let mut rs = RouterSettings::new();
    rs.optimizer.as_mut().unwrap().board_update_strategy = Some(BoardUpdateStrategy::Hybrid);

    let value = serde_json::to_value(&rs).expect("RouterSettings must serialize");
    let optimizer = value
        .as_object()
        .unwrap()
        .get("optimizer")
        .and_then(|v| v.as_object())
        .unwrap();
    assert!(!optimizer.contains_key("board_update_strategy"));
}

#[test]
fn field_names_pin_javas_declaration_order() {
    assert_eq!(
        RouterSettings::FIELD_NAMES,
        &[
            "enabled",
            "algorithm",
            "fanout",
            "copper_to_edge_clearance_um",
            "hole_clearance_um",
            "neck_width_um",
            "strict_drc",
            "job_timeout_string",
            "max_passes",
            "max_items",
            "layers",
            "save_intermediate_stages",
            "ignore_net_classes",
            "trace_pull_tight_accuracy",
            "vias_allowed",
            "automatic_neckdown",
            "optimizer",
            "scoring",
            "max_threads",
            "result_json_path",
            "board_specific_trace_costs_applied",
            "opt_changed_area_ms",
            "smd_via_relaxation",
            "failure_give_up_threshold",
            "connection_search_steps",
        ]
    );
    assert_eq!(
        LayerSettings::FIELD_NAMES,
        &["routable", "preferred_direction_horizontal", "bend_cost"]
    );
    assert_eq!(
        ScoringSettings::FIELD_NAMES,
        &[
            "preferred_direction_trace_cost",
            "undesired_direction_trace_cost",
            "default_preferred_direction_trace_cost",
            "default_undesired_direction_trace_cost",
            "via_costs",
            "plane_via_costs",
            "start_ripup_costs",
            "unrouted_net_penalty",
            "clearance_violation_penalty",
            "bend_penalty",
            "default_bend_cost",
            "smd_via_cost_factor",
        ]
    );
    assert_eq!(
        OptimizerSettings::FIELD_NAMES,
        &[
            "enabled",
            "algorithm",
            "max_passes",
            "max_items",
            "max_threads",
            "optimization_improvement_threshold",
            "max_consecutive_failures",
            "additional_ripup_cost_factor_at_start",
            "trace_ripup_cost_factor",
            "max_autoroute_passes",
            "max_search_steps",
            "board_update_strategy",
            "hybrid_ratio",
            "item_selection_strategy",
            "timeout_string",
        ]
    );
    assert_eq!(
        FanoutSettings::FIELD_NAMES,
        &[
            "enabled",
            "max_passes",
            "max_items",
            "max_milliseconds_per_pin",
            "ripup_allowed",
            "min_escape_length_mm",
            "max_escape_length_mm",
            "start_via_diameter_mm",
            "end_via_diameter_mm",
            "pin_sorting_order",
            "fallback_to_board_vias",
            "timeout_string",
        ]
    );
    assert_eq!(
        DesignRulesCheckerSettings::FIELD_NAMES,
        &["enabled", "include_warnings", "include_errors"]
    );
    assert_eq!(
        DebugSettings::FIELD_NAMES,
        &[
            "enable_detailed_logging",
            "single_step_execution",
            "trace_insertion_delay",
            "filter_by_net",
            "operation_filters",
        ]
    );
}

#[test]
fn host_environment_default_max_threads_leaves_half_the_host_free() {
    assert_eq!(HostEnvironment::with_processors(1).default_max_threads(), 1);
    assert_eq!(HostEnvironment::with_processors(8).default_max_threads(), 4);
    assert_eq!(HostEnvironment::with_processors(0).default_max_threads(), 1);
}

#[test]
fn design_rules_checker_settings_default_matches_javas_field_initializers() {
    let d = DesignRulesCheckerSettings::default();
    assert!(!d.enabled);
    assert!(d.include_warnings);
    assert!(d.include_errors);
}

#[test]
fn debug_settings_default_matches_javas_field_initializers() {
    let d = DebugSettings::default();
    assert!(!d.enable_detailed_logging);
    assert!(!d.single_step_execution);
    assert_eq!(d.trace_insertion_delay, 0);
    assert!(d.filter_by_net.is_empty());
    assert_eq!(
        d.operation_filters,
        vec![
            "insert_trace_segment",
            "remove_trace_segment",
            "insert_trace_failure",
            "remove_tail",
            "insert_trace",
            "remove_trace",
            "insert_via",
            "remove_via",
        ]
    );
}

#[test]
fn debug_settings_is_net_permitted_matches_java() {
    let mut d = DebugSettings::default();
    assert!(d.is_net_permitted(1, None));

    d.filter_by_net.insert("1".to_string());
    assert!(d.is_net_permitted(1, None));
    assert!(!d.is_net_permitted(2, None));

    let mut d2 = DebugSettings::default();
    d2.filter_by_net.insert("Net #3".to_string());
    assert!(d2.is_net_permitted(3, None));

    let mut d3 = DebugSettings::default();
    d3.filter_by_net.insert("Net#4".to_string());
    assert!(d3.is_net_permitted(4, None));

    let mut d4 = DebugSettings::default();
    d4.filter_by_net.insert("mynet".to_string());
    assert!(d4.is_net_permitted(0, Some("MyNet")));
    assert!(!d4.is_net_permitted(0, Some("OtherNet")));
}
