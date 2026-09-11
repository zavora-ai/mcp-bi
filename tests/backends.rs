// Licensed under the Apache License, Version 2.0.

//! What this server promises, checked without a BI platform.
//!
//! The seeded backend is exercised for real, and Superset against recorded
//! responses — so these tests assert that the adapter asks the right endpoint and
//! sends its token as a header, with no Superset to hand.
//!
//! The Superset fixtures are not invented. Every shape here was confirmed against a
//! live Apache Superset instance, including two that a plausible reading of the docs
//! gets wrong: a saved chart usually has **no** query context, so its data has to be
//! re-queried through `POST /api/v1/chart/data`; and SQL Lab is CSRF-protected while
//! the chart data endpoint is not.

use mcp_bi::backend::{BiBackend, Selection};
use mcp_bi::http::Recorded;
use mcp_bi::memory::MemoryBackend;
use mcp_bi::open_source::Superset;
use mcp_bi::render;
use mcp_bi::types::Filter;
use serde_json::json;
use std::sync::Arc;

fn filter(column: &str, op: &str, value: serde_json::Value) -> Filter {
    Filter {
        column: column.into(),
        op: op.into(),
        value,
    }
}

// ── The offline backend ─────────────────────────────────────────────────────

#[tokio::test]
async fn the_default_backend_needs_no_credentials_and_has_dashboards_to_open() {
    let backend = MemoryBackend::new();
    let dashboards = backend.list_dashboards().await.unwrap();
    assert_eq!(
        dashboards.len(),
        3,
        "three saved dashboards to navigate between"
    );
    let ids: Vec<&str> = dashboards.iter().map(|d| d.id.as_str()).collect();
    assert!(ids.contains(&"market-overview"));
    assert!(ids.contains(&"portfolio-risk"));
    assert!(ids.contains(&"execution-quality"));

    // Every dashboard names the dimensions its charts can be broken down by, which
    // is what makes an unattended drill-down possible rather than guesswork.
    let overview = backend.get_dashboard("market-overview").await.unwrap();
    let price = overview
        .charts
        .iter()
        .find(|c| c.id == "price-by-day")
        .unwrap();
    assert!(price.dimensions.contains(&"ticker".to_string()));
    assert!(price.dimensions.contains(&"sector".to_string()));
}

#[tokio::test]
async fn an_unknown_dashboard_lists_the_real_ones() {
    let error = MemoryBackend::new()
        .get_dashboard("revenue")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("revenue"), "says what was asked for");
    assert!(
        error.contains("market-overview"),
        "and what exists: {error}"
    );
}

#[tokio::test]
async fn the_fixture_is_identical_on_every_run() {
    // The property that makes a finance demo checkable: a number reached today can
    // be re-derived tomorrow and compared.
    let first = MemoryBackend::new()
        .chart_data("price-by-day", &[])
        .await
        .unwrap();
    let second = MemoryBackend::new()
        .chart_data("price-by-day", &[])
        .await
        .unwrap();
    assert_eq!(
        first, second,
        "seeded data must not drift between constructions"
    );
    assert_eq!(first.rows.len(), 30, "one row per day in the window");
}

#[tokio::test]
async fn a_filter_narrows_the_numbers_and_the_change_is_visible() {
    let backend = MemoryBackend::new();
    let all = backend.chart_data("volume-by-venue", &[]).await.unwrap();
    assert_eq!(all.rows.len(), 3, "three venues before filtering");

    let drill = backend
        .drill_down(
            "volume-by-venue",
            None,
            &[filter("region", "=", json!("EU"))],
        )
        .await
        .unwrap();
    assert_eq!(drill.rows_before, 3);
    assert!(
        drill.rows_after <= drill.rows_before,
        "a filter cannot add venues"
    );
    assert_eq!(drill.filters.len(), 1);
}

#[tokio::test]
async fn breaking_a_chart_down_by_a_dimension_regroups_it() {
    let backend = MemoryBackend::new();
    let by_venue = backend.chart_data("volume-by-venue", &[]).await.unwrap();
    assert_eq!(by_venue.columns[0], "venue");

    let by_region = backend
        .drill_down("volume-by-venue", Some("region"), &[])
        .await
        .unwrap();
    assert_eq!(
        by_region.table.columns[0], "region",
        "the grouping key changed"
    );
    assert_eq!(by_region.dimension.as_deref(), Some("region"));
    // Three regions in the fixture, so the breakdown has to produce three buckets.
    assert_eq!(by_region.table.rows.len(), 3);
}

#[tokio::test]
async fn filters_and_a_breakdown_compose() {
    let drill = MemoryBackend::new()
        .drill_down(
            "price-by-day",
            Some("ticker"),
            &[filter("sector", "=", json!("Technology"))],
        )
        .await
        .unwrap();
    // Three of the six tickers are Technology, so a sector filter plus a ticker
    // breakdown must land on exactly those.
    assert_eq!(drill.table.rows.len(), 3, "AAPL, MSFT and TSM");
    let names: Vec<String> = drill.table.text_column("ticker").unwrap();
    assert!(names.contains(&"AAPL".to_string()));
    assert!(
        !names.contains(&"JPM".to_string()),
        "a Financials ticker was excluded"
    );
}

#[tokio::test]
async fn the_offline_backend_says_what_it_cannot_do() {
    let backend = MemoryBackend::new();
    let capabilities = backend.capabilities();
    assert!(!capabilities.raw_query, "no SQL engine behind a fixture");
    assert!(!capabilities.export_image, "and no renderer");
    assert!(capabilities.drill_down);
    assert!(
        capabilities
            .notes
            .iter()
            .any(|note| note.contains("BI_BACKEND=superset")),
        "and points at how to reach a real platform"
    );

    // A gap is an error naming the workaround, not an empty table.
    let error = backend
        .query("trades", "select 1", 10)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("memory"));
    assert!(error.contains("bi_backend_info"));
}

#[tokio::test]
async fn a_dashboard_link_carries_the_filters() {
    let url = MemoryBackend::new()
        .deep_link("market-overview", &[filter("ticker", "=", json!("AAPL"))])
        .await
        .unwrap();
    assert!(url.contains("market-overview"));
    assert!(
        url.contains("ticker=AAPL"),
        "so a browser opens the same view: {url}"
    );
}

#[tokio::test]
async fn chart_numbers_can_be_drawn_and_described_together() {
    let table = MemoryBackend::new()
        .chart_data("price-by-day", &[])
        .await
        .unwrap();
    let image = render::render(&table, "line", "Close price", "grafana", None, None).unwrap();
    assert_eq!(image.mime_type, "image/png");
    assert!(image.bytes > 2_000);

    let insights = render::insights(&table);
    assert_eq!(insights.len(), 1, "one numeric column, one description");
    let insight = &insights[0];
    assert_eq!(insight.column, "price");
    assert_eq!(insight.points, 30);
    assert!(["up", "down", "flat"].contains(&insight.direction.as_str()));
    // A statistic an agent can cite instead of reading the slope off the picture.
    assert!(
        !insight.max_at.is_empty(),
        "the peak is labelled with its day"
    );
}

// ── Apache Superset, against responses recorded from a live instance ────────

/// The chart record Superset actually returns.
///
/// `params` is a JSON *string*, and `query_context` is null for a chart built in the
/// UI — which is exactly why the data has to be re-queried rather than read back.
fn chart_record(id: u32, groupby: serde_json::Value) -> serde_json::Value {
    json!({ "result": {
        "slice_name": "Games per Genre",
        "datasource_id": 20,
        "datasource_type": "table",
        "query_context": serde_json::Value::Null,
        "params": json!({
            "viz_type": "echarts_timeseries_bar",
            "groupby": groupby,
            "metrics": [{ "aggregate": "SUM", "label": "SUM(sales)" }],
        })
        .to_string(),
        "id": id,
    }})
}

#[tokio::test]
async fn superset_reads_dashboards_and_sends_its_token_as_a_header() {
    let http = Arc::new(Recorded::new(vec![(
        "/api/v1/dashboard/?q=",
        json!({ "result": [
            { "id": 7, "dashboard_title": "Trading Desk", "url": "/superset/dashboard/7/",
              "changed_on_utc": "2026-09-10T09:00:00Z" },
            { "id": 8, "dashboard_title": "Risk", "url": "/superset/dashboard/8/" },
        ]}),
    )]));
    let superset = Superset::new(http.clone(), "https://bi.example.invalid", "secret-token");

    let dashboards = superset.list_dashboards().await.unwrap();
    assert_eq!(dashboards.len(), 2);
    assert_eq!(dashboards[0].title, "Trading Desk");
    assert_eq!(dashboards[0].id, "7");
    // The URL is absolute, so it can be opened without knowing the base.
    assert_eq!(
        dashboards[0].url.as_deref(),
        Some("https://bi.example.invalid/superset/dashboard/7/")
    );

    // The credential travels as a bearer header, never in the query string.
    assert!(http.sent_header("Authorization", "Bearer secret-token"));
    assert!(!http.called("secret-token"), "and not in any URL");
}

#[tokio::test]
async fn superset_maps_a_charts_form_data_onto_dimensions_and_metrics() {
    let http = Arc::new(Recorded::new(vec![
        (
            "/api/v1/dashboard/7/charts",
            json!({ "result": [{
                "id": 41, "slice_name": "Volume by venue",
                "form_data": {
                    "viz_type": "echarts_timeseries_bar",
                    "datasource": "12__table",
                    "groupby": ["venue", "region"],
                    "metrics": [{ "label": "SUM(volume)" }],
                }
            }]}),
        ),
        (
            "/api/v1/dashboard/7",
            json!({ "result": { "dashboard_title": "Trading Desk", "description": "Desk overview" }}),
        ),
    ]));
    let dashboard = Superset::new(http, "https://bi.example.invalid", "t")
        .get_dashboard("7")
        .await
        .unwrap();
    assert_eq!(dashboard.title, "Trading Desk");
    let chart = &dashboard.charts[0];
    assert_eq!(chart.id, "41");
    assert_eq!(chart.kind, "echarts_timeseries_bar");
    assert_eq!(chart.dimensions, vec!["venue", "region"]);
    // A metric can be an object or a string; the label is what a person reads.
    assert_eq!(chart.metrics, vec!["SUM(volume)"]);
}

#[tokio::test]
async fn superset_chart_data_rebuilds_the_query_because_saved_context_is_usually_absent() {
    // Confirmed against a live instance: `GET /chart/{id}/data/` answers
    // `400 Chart has no query context saved` for a chart created in the UI. So the
    // query is rebuilt from `params` and posted to `/chart/data`, as the UI does.
    let http = Arc::new(Recorded::new(vec![
        ("/api/v1/chart/41", chart_record(41, json!(["genre"]))),
        (
            "/api/v1/chart/data",
            json!({ "result": [{
                "colnames": ["genre", "SUM(sales)"],
                "data": [
                    { "SUM(sales)": 91_000, "genre": "Action" },
                    { "SUM(sales)": 64_500, "genre": "Sports" },
                ],
            }]}),
        ),
    ]));
    let table = Superset::new(http.clone(), "https://bi.example.invalid", "t")
        .chart_data("41", &[])
        .await
        .unwrap();

    // Records are unordered maps, so column order comes from `colnames` — otherwise
    // two calls could disagree about which value is which.
    assert_eq!(table.columns, vec!["genre", "SUM(sales)"]);
    assert_eq!(table.rows[0][0], json!("Action"));
    assert_eq!(table.rows[0][1], json!(91_000));

    assert!(http.called("/api/v1/chart/41"), "the chart was read first");
    let seen = http.seen.lock().unwrap();
    let posted = seen
        .iter()
        .filter_map(|(url, _, body)| body.clone().map(|body| (url.clone(), body)))
        .find(|(url, _)| url.ends_with("/chart/data"))
        .expect("the data endpoint was posted to");
    // The datasource and metrics come from the chart, not from the caller.
    assert_eq!(posted.1["datasource"], json!({ "id": 20, "type": "table" }));
    assert_eq!(posted.1["queries"][0]["columns"], json!(["genre"]));
    assert_eq!(posted.1["result_type"], json!("results"));

    let insight = &render::insights(&table)[0];
    assert_eq!(insight.max, 91_000.0);
    assert_eq!(insight.max_at, "Action");
}

#[tokio::test]
async fn superset_drill_down_replaces_the_grouping_and_sends_simple_filters() {
    let http = Arc::new(Recorded::new(vec![
        ("/api/v1/chart/41", chart_record(41, json!(["genre"]))),
        (
            "/api/v1/chart/data",
            json!({ "result": [{
                "colnames": ["platform", "SUM(sales)"],
                "data": [{ "platform": "PS4", "SUM(sales)": 12_000 }],
            }]}),
        ),
    ]));
    let drill = Superset::new(http.clone(), "https://bi.example.invalid", "t")
        .drill_down(
            "41",
            Some("platform"),
            &[filter("genre", "=", json!("Action"))],
        )
        .await
        .unwrap();
    assert_eq!(drill.dimension.as_deref(), Some("platform"));
    assert_eq!(drill.table.columns[0], "platform");
    assert_eq!(drill.rows_after, 1);

    let seen = http.seen.lock().unwrap();
    let posted = seen
        .iter()
        .filter_map(|(url, _, body)| body.clone().map(|body| (url.clone(), body)))
        .rfind(|(url, _)| url.ends_with("/chart/data"))
        .expect("the data endpoint was posted to");
    let query = &posted.1["queries"][0];
    // The drill dimension replaces the chart's own grouping.
    assert_eq!(query["columns"], json!(["platform"]));
    // Confirmed against a live instance: the chart data API takes {col, op, val}
    // with `==` for equality — not the `=` a dashboard URL uses.
    assert_eq!(query["filters"][0]["col"], json!("genre"));
    assert_eq!(query["filters"][0]["op"], json!("=="));
    assert_eq!(query["filters"][0]["val"], json!("Action"));
}

#[tokio::test]
async fn superset_resolves_a_datasets_database_and_satisfies_csrf_before_running_sql() {
    let http = Arc::new(Recorded::new(vec![
        (
            "/api/v1/dataset/12",
            json!({ "result": { "database": { "id": 3 }, "table_name": "trades" }}),
        ),
        // SQL Lab is CSRF-protected, unlike the chart data endpoint.
        (
            "/api/v1/security/csrf_token/",
            json!({ "result": "csrf-abc" }),
        ),
        (
            "/api/v1/sqllab/execute/",
            json!({
                "columns": [{ "name": "ticker" }, { "name": "pnl" }],
                "data": [{ "ticker": "AAPL", "pnl": 1234.5 }],
            }),
        ),
    ]));
    let table = Superset::new(http.clone(), "https://bi.example.invalid", "t")
        .query("12", "select ticker, pnl from trades", 100)
        .await
        .unwrap();
    assert_eq!(table.columns, vec!["ticker", "pnl"]);
    assert_eq!(table.rows[0][1], json!(1234.5));

    // The caller supplied a dataset, not a database, so the adapter looked it up.
    assert!(http.called("/api/v1/dataset/12"));
    assert!(
        http.sent_header("X-CSRFToken", "csrf-abc"),
        "the CSRF token was sent"
    );
    let seen = http.seen.lock().unwrap();
    let executed = seen
        .iter()
        .filter_map(|(url, _, body)| body.clone().map(|body| (url.clone(), body)))
        .find(|(url, _)| url.contains("sqllab"))
        .expect("SQL was executed");
    assert_eq!(
        executed.1["database_id"],
        json!(3),
        "the resolved database id"
    );
    // Confirmed against a live instance: the schema rejects `limit` outright.
    assert_eq!(executed.1["queryLimit"], json!(100));
}

#[tokio::test]
async fn superset_reports_a_platform_error_rather_than_an_empty_table() {
    // Nothing recorded, which stands in for a 403 or a missing chart.
    let http = Arc::new(Recorded::new(vec![]));
    let error = Superset::new(http, "https://bi.example.invalid", "t")
        .chart_data("99", &[])
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("/api/v1/chart/99"),
        "names the call that failed: {error}"
    );
}

#[tokio::test]
async fn superset_is_the_open_source_primary_and_can_do_everything_the_trait_needs() {
    let http = Arc::new(Recorded::new(vec![]));
    let capabilities = Superset::new(http, "https://bi.example.invalid", "t").capabilities();
    assert_eq!(capabilities.backend, "superset");
    assert!(capabilities.open_source);
    for (name, supported) in [
        ("list_dashboards", capabilities.list_dashboards),
        ("chart_data", capabilities.chart_data),
        ("raw_query", capabilities.raw_query),
        ("drill_down", capabilities.drill_down),
        ("export_image", capabilities.export_image),
        ("deep_links", capabilities.deep_links),
    ] {
        assert!(supported, "the primary backend supports {name}");
    }
    assert!(Selection::Superset.open_source());
}

#[tokio::test]
async fn a_superset_link_carries_filter_state_for_a_browser() {
    let http = Arc::new(Recorded::new(vec![]));
    let url = Superset::new(http, "https://bi.example.invalid", "t")
        .deep_link("7", &[filter("ticker", "in", json!(["AAPL", "MSFT"]))])
        .await
        .unwrap();
    assert!(url.starts_with("https://bi.example.invalid/superset/dashboard/7/"));
    assert!(
        url.contains("form_data="),
        "filters ride in form_data: {url}"
    );
    // URL-encoded, so a browser receives one parameter rather than a broken query.
    assert!(!url.contains(' '), "no raw spaces in a URL");
}
