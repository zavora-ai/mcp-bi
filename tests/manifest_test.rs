//! Validate `mcp-server.toml` parses, passes SDK validation, declares every tool,
//! and claims no write surface — because this server has none.

use adk_mcp_sdk::manifest::ServerManifest;
use adk_mcp_sdk::risk::RiskClass;
use std::path::Path;

fn manifest() -> ServerManifest {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("mcp-server.toml");
    ServerManifest::from_file(&path).expect("manifest should parse")
}

#[test]
fn manifest_parses_and_validates() {
    let m = manifest();
    assert!(
        m.validate().is_empty(),
        "validation errors: {:?}",
        m.validate()
    );
    assert_eq!(m.server_id, "mcp_bi");
    assert_eq!(m.domain, "analytics");
    assert_eq!(m.tools.len(), 12, "expected 12 declared tools");
}

#[test]
fn every_tool_is_read_only_and_ungated() {
    // A dashboard is changed through the platform's own review, not by an agent, so
    // there is no write surface here and nothing to gate. If that ever changes, this
    // test should fail loudly rather than a write slipping in unannounced.
    let m = manifest();
    for tool in &m.tools {
        assert_eq!(
            tool.risk_class,
            RiskClass::ReadOnly,
            "{} should be read_only",
            tool.name
        );
        assert!(
            !tool.requires_approval,
            "{} should need no approval",
            tool.name
        );
        assert!(
            tool.credential_bindings.is_empty(),
            "{} binds no credential",
            tool.name
        );
    }
    assert_eq!(m.writes_allowed, adk_mcp_sdk::manifest::WritesAllowed::None);
}

#[test]
fn the_declared_tools_match_the_ones_the_server_exposes() {
    // The manifest is what the registry onboards from, so a tool added to the server
    // and forgotten here would be invisible to it.
    let m = manifest();
    let declared: std::collections::BTreeSet<&str> =
        m.tools.iter().map(|tool| tool.name.as_str()).collect();
    let expected: std::collections::BTreeSet<&str> = [
        "bi_backend_info",
        "bi_chart_data",
        "bi_dashboard_url",
        "bi_describe_dataset",
        "bi_drill_down",
        "bi_export_dashboard_image",
        "bi_get_dashboard",
        "bi_insights",
        "bi_list_dashboards",
        "bi_list_datasets",
        "bi_query",
        "bi_render_chart",
    ]
    .into_iter()
    .collect();
    assert_eq!(declared, expected);
}

#[test]
fn the_manifest_version_tracks_the_crate() {
    assert_eq!(manifest().version, env!("CARGO_PKG_VERSION"));
}
