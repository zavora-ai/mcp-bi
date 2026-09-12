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
use mcp_bi::open_source::{Metabase, Superset};
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

// ---------------------------------------------------------------------------
// Token lifetime. A Superset access token is valid for 15 minutes — measured, not
// assumed — which is shorter than a real analysis session. These cover the seam
// that made a live run fail partway through with an opaque 401.
// ---------------------------------------------------------------------------

/// Build an unsigned JWT whose `exp` is `offset` seconds from now.
fn jwt_expiring_in(offset: i64) -> String {
    use base64::Engine as _;
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let claims = serde_json::json!({ "exp": chrono::Utc::now().timestamp() + offset });
    format!(
        "{}.{}.signature-is-not-checked",
        engine.encode(b"{\"alg\":\"HS256\"}"),
        engine.encode(serde_json::to_vec(&claims).unwrap()),
    )
}

#[tokio::test]
async fn a_token_with_life_left_is_used_as_given() {
    let http = Arc::new(Recorded::new(vec![(
        "/api/v1/dashboard/",
        json!({ "result": [], "count": 0 }),
    )]));
    let superset = Superset::new(http.clone(), "http://superset.test", &jwt_expiring_in(900));
    superset.list_dashboards().await.expect("should query");
    let token = jwt_expiring_in(900);
    assert!(
        http.seen.lock().unwrap()[0]
            .1
            .iter()
            .any(|(name, value)| name == "Authorization" && value.starts_with("Bearer ")),
        "the supplied token should be presented as a bearer token"
    );
    let _ = token;
}

#[tokio::test]
async fn an_expired_token_with_no_credentials_says_what_would_fix_it() {
    // The failure a live run actually hit. Superset's own answer is
    // `{"msg":"Token has expired"}`, which tells an operator nothing about the
    // remedy, so the server must not simply pass it along.
    let http = Arc::new(Recorded::new(vec![(
        "/api/v1/dashboard/",
        json!({ "result": [], "count": 0 }),
    )]));
    let superset = Superset::new(http, "http://superset.test", &jwt_expiring_in(-1));
    let error = superset
        .list_dashboards()
        .await
        .expect_err("an expired token must not be used")
        .to_string();
    assert!(
        error.contains("15 minutes"),
        "should state the lifetime: {error}"
    );
    assert!(
        error.contains("SUPERSET_USERNAME"),
        "should name the fix: {error}"
    );
    assert!(
        error.contains("SUPERSET_PASSWORD"),
        "should name the fix: {error}"
    );
}

#[tokio::test]
async fn credentials_let_the_server_mint_its_own_token() {
    let fresh = jwt_expiring_in(900);
    let http = Arc::new(Recorded::new(vec![
        ("/api/v1/security/login", json!({ "access_token": fresh })),
        ("/api/v1/dashboard/", json!({ "result": [], "count": 0 })),
    ]));
    let superset = Superset::with_login(http.clone(), "http://superset.test", "", "admin", "admin");
    superset
        .list_dashboards()
        .await
        .expect("should log in, then query");

    let seen = http.seen.lock().unwrap();
    assert!(
        seen[0].0.contains("/api/v1/security/login"),
        "the first call should obtain a token, not fail: {}",
        seen[0].0
    );
    assert!(
        seen[1].0.contains("/api/v1/dashboard/"),
        "then the query runs"
    );
    let presented = seen[1]
        .1
        .iter()
        .find(|(name, _)| name == "Authorization")
        .map(|(_, value)| value.clone())
        .expect("the query should carry a bearer token");
    assert_eq!(
        presented,
        format!("Bearer {fresh}"),
        "it should carry the minted token"
    );
}

#[tokio::test]
async fn an_expiring_token_is_replaced_before_it_is_rejected() {
    // Inside the refresh margin the token is still technically valid, but a request
    // issued now could be rejected on arrival, so it is replaced first.
    let fresh = jwt_expiring_in(900);
    let http = Arc::new(Recorded::new(vec![
        ("/api/v1/security/login", json!({ "access_token": fresh })),
        ("/api/v1/dashboard/", json!({ "result": [], "count": 0 })),
    ]));
    let superset = Superset::with_login(
        http.clone(),
        "http://superset.test",
        &jwt_expiring_in(30),
        "admin",
        "admin",
    );
    superset
        .list_dashboards()
        .await
        .expect("should refresh, then query");
    assert!(
        http.seen.lock().unwrap()[0]
            .0
            .contains("/api/v1/security/login"),
        "a token 30 seconds from expiry should be refreshed first"
    );
}

#[tokio::test]
async fn a_non_jwt_token_is_trusted_because_only_the_server_can_judge_it() {
    let http = Arc::new(Recorded::new(vec![(
        "/api/v1/dashboard/",
        json!({ "result": [], "count": 0 }),
    )]));
    let superset = Superset::new(http, "http://superset.test", "an-opaque-token");
    superset
        .list_dashboards()
        .await
        .expect("an unparseable token should still be presented");
}

// ---------------------------------------------------------------------------
// Metabase has two id spaces: a table id, which is what bi_list_datasets
// reports, and a database id, which is what a query runs against. Conflating
// them produced `HTTP 500 Assert failed: (keyword? driver)` from a live
// instance — Metabase failing to resolve a driver for a database that does not
// exist — which says nothing about the actual mistake.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_metabase_query_runs_against_the_table_s_database_not_its_own_id() {
    let http = Arc::new(Recorded::new(vec![
        // The table knows which database it belongs to.
        (
            "/api/table/2",
            json!({ "id": 2, "name": "PRODUCTS", "db_id": 1 }),
        ),
        (
            "/api/dataset",
            json!({
                "data": {
                    "cols": [{ "display_name": "CATEGORY" }, { "display_name": "n" }],
                    "rows": [["Widget", 54]],
                }
            }),
        ),
    ]));
    let metabase = Metabase::new(http.clone(), "http://metabase.test", "session-token");
    let table = metabase
        .query(
            "2",
            "SELECT CATEGORY, COUNT(*) AS n FROM PRODUCTS GROUP BY CATEGORY",
            100,
        )
        .await
        .expect("the query should run");
    assert_eq!(table.rows.len(), 1);

    let seen = http.seen.lock().unwrap();
    assert!(
        seen[0].0.contains("/api/table/2"),
        "the table is resolved first: {}",
        seen[0].0
    );
    let body = seen[1].2.as_ref().expect("the query carries a body");
    assert_eq!(
        body.get("database").and_then(|value| value.as_i64()),
        Some(1),
        "the database must come from the table's db_id, not from the dataset id"
    );
}

#[tokio::test]
async fn an_id_that_is_not_a_table_says_so_rather_than_guessing_a_database() {
    // The old code fell back to database 1 for anything unparseable, which turned a
    // caller's mistake into a query against a database they never named.
    let http = Arc::new(Recorded::new(vec![(
        "/api/table/not-a-table",
        json!({ "id": 99, "name": "X" }),
    )]));
    let metabase = Metabase::new(http, "http://metabase.test", "session-token");
    let error = metabase
        .query("not-a-table", "SELECT 1", 10)
        .await
        .expect_err("a table with no db_id cannot be queried")
        .to_string();
    assert!(
        error.contains("db_id"),
        "should name what is missing: {error}"
    );
}

// ---------------------------------------------------------------------------
// Memory. OpenAI measured the same question taking 22m41s without it and 1m22s
// with it; the reason is that a data platform is full of facts its schema does
// not contain. These cover what must be true for that to work: a correction
// survives, it is scoped to the platform it was learned on, and a wrong one can
// be removed.
// ---------------------------------------------------------------------------

use mcp_bi::recall::Recall;

fn at(when: &str) -> String {
    format!("2026-09-12T{when}:00Z")
}

#[test]
fn a_correction_is_recalled_by_a_differently_worded_question() {
    // The point is not exact matching. A note recorded as "product-line volume
    // chart" has to come back for "which chart shows volume by product line".
    let store = Recall::ephemeral();
    store
        .remember(
            "product-line volume chart",
            "Chart 37 is the one people mean. Chart 12 looks similar but excludes returns.",
            "superset",
            Some("3"),
            &at("10:00"),
        )
        .expect("should save");
    let hits = store.recall(
        "which chart shows volume by product line?",
        "superset",
        None,
        None,
    );
    assert_eq!(hits.len(), 1);
    assert!(hits[0].note.contains("Chart 37"));
}

#[test]
fn a_note_is_never_offered_for_a_different_platform() {
    // A Superset chart id means nothing to Metabase, and offering it would invite
    // a confident wrong answer.
    let store = Recall::ephemeral();
    store
        .remember(
            "chart ids",
            "Chart 37 is product-line volume",
            "superset",
            None,
            &at("10:00"),
        )
        .unwrap();
    assert_eq!(store.recall("chart ids", "superset", None, None).len(), 1);
    assert!(store.recall("chart ids", "metabase", None, None).is_empty());
}

#[test]
fn a_scoped_note_is_relevant_even_when_no_words_match() {
    let store = Recall::ephemeral();
    store
        .remember(
            "filter values",
            "The region filter wants ISO codes, not names",
            "superset",
            Some("dash-9"),
            &at("10:00"),
        )
        .unwrap();
    let hits = store.recall("anything at all", "superset", Some("dash-9"), None);
    assert_eq!(
        hits.len(),
        1,
        "a note scoped to what is being asked about is relevant"
    );
}

#[test]
fn correcting_a_correction_replaces_it_rather_than_stacking() {
    // Two contradictory notes on the same subject, both looking equally true, is
    // worse than either alone.
    let store = Recall::ephemeral();
    store
        .remember("chart", "use chart 12", "superset", None, &at("10:00"))
        .unwrap();
    store
        .remember(
            "chart",
            "use chart 37, 12 excludes returns",
            "superset",
            None,
            &at("11:00"),
        )
        .unwrap();
    let hits = store.recall("chart", "superset", None, None);
    assert_eq!(
        hits.len(),
        1,
        "the earlier note should be gone, not ranked below"
    );
    assert!(hits[0].note.contains("37"));
}

#[test]
fn a_wrong_correction_can_be_forgotten() {
    let store = Recall::ephemeral();
    store
        .remember("chart", "use chart 12", "superset", None, &at("10:00"))
        .unwrap();
    assert_eq!(store.forget("chart", "superset").unwrap(), 1);
    assert!(store.recall("chart", "superset", None, None).is_empty());
    assert_eq!(
        store.forget("chart", "superset").unwrap(),
        0,
        "forgetting twice is not an error"
    );
}

#[test]
fn a_note_records_how_often_it_earned_its_place() {
    // A note recalled constantly is load-bearing; one never recalled is a
    // candidate for removal. Without the count, neither is distinguishable.
    let store = Recall::ephemeral();
    store
        .remember("volume chart", "chart 37", "superset", None, &at("10:00"))
        .unwrap();
    store.recall("volume chart", "superset", None, None);
    store.recall("volume chart", "superset", None, None);
    assert_eq!(store.all(Some("superset"))[0].recalled, 2);
}

#[test]
fn an_empty_or_oversized_note_is_refused_with_the_reason() {
    let store = Recall::ephemeral();
    let empty = store.remember("subject", "   ", "superset", None, &at("10:00"));
    assert!(
        empty
            .unwrap_err()
            .to_string()
            .contains("subject and a note")
    );

    let long = "x".repeat(5_000);
    let error = store
        .remember("subject", &long, "superset", None, &at("10:00"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("at most"), "should state the limit: {error}");
    assert!(
        error.contains("finding"),
        "should say why: a long note is a finding, not a correction"
    );
}

#[tokio::test]
async fn memory_survives_a_restart() {
    // The whole value proposition. If it does not outlive the process it saves
    // nothing, because a session already has its own context.
    let dir = std::env::temp_dir().join(format!("mcp-bi-recall-{}", std::process::id()));
    let path = dir.join("memory.json");
    let _ = std::fs::remove_dir_all(&dir);
    unsafe { std::env::set_var("BI_MEMORY_PATH", &path) };

    let first = Recall::open_from_env();
    first
        .remember(
            "dataset ids",
            "A Metabase dataset id is a table id, not a database id",
            "metabase",
            None,
            &at("10:00"),
        )
        .expect("should save");
    assert!(path.exists(), "a note must reach disk");
    drop(first);

    let second = Recall::open_from_env();
    let hits = second.recall("what is a dataset id here?", "metabase", None, None);
    assert_eq!(hits.len(), 1, "a fresh store must find the earlier note");
    assert!(hits[0].note.contains("table id"));

    let _ = std::fs::remove_dir_all(&dir);
    unsafe { std::env::remove_var("BI_MEMORY_PATH") };
}

// ── Metabase session renewal ────────────────────────────────────────────────
//
// Every shape below was measured against a live Metabase instance, not read from the
// docs. Two facts drove the design: `POST /api/session` answers with exactly
// `{"id": "<uuid>"}`, and a rejected session answers `401` with the bare body
// `Unauthenticated` — not JSON, so nothing useful can be parsed out of it.

#[tokio::test]
async fn metabase_obtains_a_session_from_credentials() {
    let http = Arc::new(Recorded::new(vec![
        ("/api/session", json!({ "id": "fresh-session-uuid" })),
        ("/api/dashboard", json!([{ "id": 1, "name": "Sales" }])),
    ]));
    let backend =
        Metabase::with_login(http.clone(), "http://mb.invalid", "u@example.invalid", "pw");
    let dashboards = backend.list_dashboards().await.expect("list dashboards");

    assert_eq!(dashboards.len(), 1);
    assert!(
        http.called("/api/session"),
        "it must log in when given no token"
    );
    assert!(
        http.sent_header("X-Metabase-Session", "fresh-session-uuid"),
        "and use the session it was issued"
    );
}

#[tokio::test]
async fn metabase_renews_a_lapsed_session_and_retries() {
    // The failure this exists for: a run that outlives its session. The first request is
    // rejected, exactly as Metabase rejects an expired session id, and the run continues.
    let http = Arc::new(
        Recorded::new(vec![
            ("/api/session", json!({ "id": "second-session" })),
            ("/api/dashboard", json!([{ "id": 7, "name": "Revenue" }])),
        ])
        .lapsing_for("/api/dashboard", 1),
    );
    let backend =
        Metabase::with_login(http.clone(), "http://mb.invalid", "u@example.invalid", "pw");
    let dashboards = backend
        .list_dashboards()
        .await
        .expect("a lapsed session must be renewed, not surfaced as a failure");

    assert_eq!(dashboards.len(), 1, "the retry returns the real answer");
    assert!(
        http.sent_header("X-Metabase-Session", "second-session"),
        "the retry must use the renewed session"
    );
}

#[tokio::test]
async fn metabase_without_credentials_says_what_would_fix_it() {
    // A bare 401 tells an operator nothing. With no way to recover, the error has to
    // name the remedy rather than repeat the platform's one-word rejection.
    let http = Arc::new(
        Recorded::new(vec![("/api/dashboard", json!([]))]).lapsing_for("/api/dashboard", 1),
    );
    let backend = Metabase::new(http, "http://mb.invalid", "a-token-with-no-credentials");
    let error = backend
        .list_dashboards()
        .await
        .expect_err("a rejected session with no credentials cannot succeed");
    let text = format!("{error:#}");

    assert!(
        text.contains("METABASE_USERNAME"),
        "names the remedy: {text}"
    );
    assert!(
        text.contains("METABASE_PASSWORD"),
        "names both variables: {text}"
    );
}

#[tokio::test]
async fn a_failed_call_carries_its_status_and_keeps_its_message() {
    // Both halves matter. The status has to be readable as data so recovery does not
    // depend on matching a message, and the message has to stay the headline so a
    // permissions failure still says which permission.
    use mcp_bi::http::{is_unauthorized, status_of};

    let http = Arc::new(
        Recorded::new(vec![("/api/dashboard", json!([]))]).lapsing_for("/api/dashboard", 1),
    );
    let backend = Metabase::new(http, "http://mb.invalid", "token");
    let error = backend.list_dashboards().await.expect_err("must fail");

    assert_eq!(
        status_of(&error),
        Some(401),
        "the status is available as data"
    );
    assert!(
        is_unauthorized(&error),
        "and is recognised as an auth failure"
    );
}
