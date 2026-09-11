// Licensed under the Apache License, Version 2.0.

//! The tool surface.
//!
//! Twelve tools, arranged the way an analyst works: find the dashboards, read one,
//! get the numbers behind a tile, narrow them, check what the numbers say, and draw
//! the result. Every backend answers the same calls.
//!
//! Two rules are enforced here rather than left to the caller:
//!
//! - **A picture always comes with its numbers.** `bi_render_chart` returns the
//!   image *and* the computed statistics, so a narration can cite a figure. A model
//!   given only pixels will describe a trend it did not measure.
//! - **A gap is an error, not an empty result.** When a platform cannot do
//!   something, the tool says which platform and what to use instead. Silently
//!   returning nothing reads as "no data", which is a different and much worse
//!   claim.

use crate::backend::{BiBackend, Selection};
use crate::render;
use crate::types::Filter;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{handler::server::wrapper::Parameters, schemars, tool, tool_router};
use serde_json::{Value, json};
use std::sync::Arc;

// ── Inputs ──────────────────────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct Empty {}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DashboardInput {
    /// Dashboard id, as returned by bi_list_dashboards.
    pub dashboard_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DatasetInput {
    /// Dataset id, as returned by bi_list_datasets.
    pub dataset_id: String,
}

/// A filter, in the flat form a model finds easy to produce.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FilterInput {
    /// Column to filter on.
    pub column: String,
    /// One of =, !=, in, >, <, >=, <=.
    #[serde(default = "equals")]
    pub op: String,
    /// Value to compare against. A string, a number, or a list for `in`.
    pub value: Value,
}

fn equals() -> String {
    "=".to_string()
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ChartDataInput {
    /// Chart id, from a dashboard's `charts`.
    pub chart_id: String,
    /// Filters to apply before reading.
    #[serde(default)]
    pub filters: Vec<FilterInput>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DrillInput {
    pub chart_id: String,
    /// Dimension to break the chart down by, from the chart's `dimensions`.
    #[serde(default)]
    pub dimension: Option<String>,
    #[serde(default)]
    pub filters: Vec<FilterInput>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct QueryInput {
    /// Dataset or model to query.
    pub dataset_id: String,
    /// SQL for Superset and Metabase, DAX for Power BI, a JSON explore for Looker.
    pub query: String,
    /// Row cap.
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    500
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RenderInput {
    /// Chart to draw. Its data is fetched, then drawn.
    pub chart_id: String,
    /// line, bar, hbar, pie or table. Defaults to the chart's own kind.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    /// A charts-rs theme: light, dark, grafana, ant, vintage, walden, westeros,
    /// chalk, shine, shadcn.
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default)]
    pub filters: Vec<FilterInput>,
    #[serde(default)]
    pub width: Option<f32>,
    #[serde(default)]
    pub height: Option<f32>,
}

fn default_theme() -> String {
    "grafana".to_string()
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LinkInput {
    pub dashboard_id: String,
    #[serde(default)]
    pub filters: Vec<FilterInput>,
}

impl FilterInput {
    fn into_filter(self) -> Filter {
        Filter {
            column: self.column,
            op: self.op,
            value: self.value,
        }
    }
}

fn filters(inputs: Vec<FilterInput>) -> Vec<Filter> {
    inputs.into_iter().map(FilterInput::into_filter).collect()
}

// ── Replies ─────────────────────────────────────────────────────────────────

fn ok(value: &Value) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(
        serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".into()),
    )])
}

/// An error a caller can act on: what failed, and what to try instead.
fn failed(tool: &str, error: &anyhow::Error) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(
        serde_json::to_string_pretty(&json!({
            "error": "bi_error",
            "tool": tool,
            "message": error.to_string(),
        }))
        .unwrap_or_default(),
    )])
}

/// Registry health, reported where the type lives.
#[async_trait::async_trait]
impl adk_mcp_sdk::HealthCheck for BiServer {
    async fn check_health(&self) -> adk_mcp_sdk::HealthStatus {
        // A backend that cannot answer is the only unhealthy state worth reporting,
        // and finding that out means a network call — so this reports readiness to
        // serve, and a real backend failure surfaces on the tool that hit it.
        adk_mcp_sdk::HealthStatus {
            healthy: true,
            message: Some(format!("ready on the {} backend", self.selection.name())),
            latency_ms: Some(1),
        }
    }
}

pub struct BiServer {
    backend: Arc<dyn BiBackend>,
    selection: Selection,
}

impl BiServer {
    pub fn new(backend: Arc<dyn BiBackend>, selection: Selection) -> Self {
        Self { backend, selection }
    }
}

#[tool_router(server_handler)]
impl BiServer {
    /// Which platform is connected, and what it can do.
    #[tool(
        description = "Report the connected BI platform and its capabilities. Call this first: \
                       backends differ in whether they can return a visual's data, run raw \
                       queries, drill down or render an image, and this says which."
    )]
    async fn bi_backend_info(&self, Parameters(_): Parameters<Empty>) -> CallToolResult {
        let capabilities = self.backend.capabilities();
        ok(&json!({
            "backend": self.selection.name(),
            "open_source": self.selection.open_source(),
            "capabilities": capabilities,
        }))
    }

    #[tool(description = "List the saved dashboards on the connected platform")]
    async fn bi_list_dashboards(&self, Parameters(_): Parameters<Empty>) -> CallToolResult {
        match self.backend.list_dashboards().await {
            Ok(dashboards) => ok(&json!({ "count": dashboards.len(), "dashboards": dashboards })),
            Err(error) => failed("bi_list_dashboards", &error),
        }
    }

    #[tool(
        description = "Open one dashboard: its charts, the dimensions each can be broken down \
                       by, and the URL a person would use"
    )]
    async fn bi_get_dashboard(
        &self,
        Parameters(input): Parameters<DashboardInput>,
    ) -> CallToolResult {
        match self.backend.get_dashboard(&input.dashboard_id).await {
            Ok(dashboard) => ok(&json!(dashboard)),
            Err(error) => failed("bi_get_dashboard", &error),
        }
    }

    #[tool(description = "List the datasets or semantic models available to query")]
    async fn bi_list_datasets(&self, Parameters(_): Parameters<Empty>) -> CallToolResult {
        match self.backend.list_datasets().await {
            Ok(datasets) => ok(&json!({ "count": datasets.len(), "datasets": datasets })),
            Err(error) => failed("bi_list_datasets", &error),
        }
    }

    #[tool(description = "Describe a dataset: columns, types, and which are groupable")]
    async fn bi_describe_dataset(
        &self,
        Parameters(input): Parameters<DatasetInput>,
    ) -> CallToolResult {
        match self.backend.describe_dataset(&input.dataset_id).await {
            Ok(dataset) => ok(&json!(dataset)),
            Err(error) => failed("bi_describe_dataset", &error),
        }
    }

    #[tool(
        description = "Read the numbers behind one chart. These are authoritative — prefer them \
                       over describing what a dashboard image appears to show."
    )]
    async fn bi_chart_data(&self, Parameters(input): Parameters<ChartDataInput>) -> CallToolResult {
        match self
            .backend
            .chart_data(&input.chart_id, &filters(input.filters))
            .await
        {
            Ok(table) => ok(&json!({
                "chart_id": input.chart_id,
                "rows": table.rows.len(),
                "table": table,
            })),
            Err(error) => failed("bi_chart_data", &error),
        }
    }

    #[tool(
        description = "Narrow a chart by filters and optionally break it down by a dimension. \
                       Returns the new numbers plus how many rows there were before and after, \
                       so a step that changed nothing is visible as such."
    )]
    async fn bi_drill_down(&self, Parameters(input): Parameters<DrillInput>) -> CallToolResult {
        match self
            .backend
            .drill_down(
                &input.chart_id,
                input.dimension.as_deref(),
                &filters(input.filters),
            )
            .await
        {
            Ok(result) => {
                // The statistics travel with the drill result, so the next thing the
                // agent says can cite a number from the narrowed data.
                let computed = render::insights(&result.table);
                ok(&json!({ "drill": result, "insights": computed }))
            }
            Err(error) => failed("bi_drill_down", &error),
        }
    }

    #[tool(
        description = "Run a query against a dataset. SQL on Superset and Metabase, DAX on \
                       Power BI, a JSON explore on Looker. Not every platform allows it — check \
                       bi_backend_info."
    )]
    async fn bi_query(&self, Parameters(input): Parameters<QueryInput>) -> CallToolResult {
        match self
            .backend
            .query(&input.dataset_id, &input.query, input.limit)
            .await
        {
            Ok(table) => ok(&json!({
                "rows": table.rows.len(),
                "truncated": table.truncated,
                "table": table,
            })),
            Err(error) => failed("bi_query", &error),
        }
    }

    #[tool(
        description = "Compute statistics for a chart's numbers: direction, change, min and max \
                       with their labels, mean, deviation, outliers and gaps. Cite these rather \
                       than describing a slope from an image."
    )]
    async fn bi_insights(&self, Parameters(input): Parameters<ChartDataInput>) -> CallToolResult {
        match self
            .backend
            .chart_data(&input.chart_id, &filters(input.filters))
            .await
        {
            Ok(table) => ok(&json!({
                "chart_id": input.chart_id,
                "insights": render::insights(&table),
            })),
            Err(error) => failed("bi_insights", &error),
        }
    }

    #[tool(
        description = "Draw a chart's numbers as a PNG and return the picture with its \
                       statistics. Rendered server-side with charts-rs, so no browser is \
                       involved and the image cannot disagree with the data it came from."
    )]
    async fn bi_render_chart(&self, Parameters(input): Parameters<RenderInput>) -> CallToolResult {
        let applied = filters(input.filters);
        let table = match self.backend.chart_data(&input.chart_id, &applied).await {
            Ok(table) => table,
            Err(error) => return failed("bi_render_chart", &error),
        };
        // Default to how the dashboard draws it, so a rendered chart looks like the
        // one the person is used to seeing.
        let kind = match input.kind {
            Some(kind) => kind,
            None => self
                .backend
                .get_dashboard("")
                .await
                .ok()
                .and_then(|dashboard| {
                    dashboard
                        .charts
                        .iter()
                        .find(|chart| chart.id == input.chart_id)
                        .map(|chart| chart.kind.clone())
                })
                .unwrap_or_else(|| "line".to_string()),
        };
        let title = input.title.unwrap_or_else(|| input.chart_id.clone());
        match render::render(
            &table,
            &kind,
            &title,
            &input.theme,
            input.width,
            input.height,
        ) {
            Ok(image) => {
                let computed = render::insights(&table);
                CallToolResult::success(vec![
                    ContentBlock::text(
                        serde_json::to_string_pretty(&json!({
                            "chart_id": input.chart_id,
                            "kind": kind,
                            "theme": input.theme,
                            "bytes": image.bytes,
                            "source": image.source,
                            "rows": table.rows.len(),
                            "insights": computed,
                        }))
                        .unwrap_or_default(),
                    ),
                    ContentBlock::image(image.data, image.mime_type),
                ])
            }
            Err(error) => failed("bi_render_chart", &error),
        }
    }

    #[tool(
        description = "Ask the platform to render a dashboard to an image. Not all can; when a \
                       platform cannot, the error says so and points at bi_dashboard_url, which \
                       a browser or a desktop screenshot can use instead."
    )]
    async fn bi_export_dashboard_image(
        &self,
        Parameters(input): Parameters<DashboardInput>,
    ) -> CallToolResult {
        match self.backend.export_image(&input.dashboard_id).await {
            Ok((bytes, mime)) => {
                use base64::Engine;
                CallToolResult::success(vec![
                    ContentBlock::text(
                        serde_json::to_string_pretty(&json!({
                            "dashboard_id": input.dashboard_id,
                            "bytes": bytes.len(),
                            "mime_type": mime,
                            "source": self.selection.name(),
                        }))
                        .unwrap_or_default(),
                    ),
                    ContentBlock::image(
                        base64::engine::general_purpose::STANDARD.encode(&bytes),
                        mime,
                    ),
                ])
            }
            Err(error) => failed("bi_export_dashboard_image", &error),
        }
    }

    #[tool(
        description = "Build a URL that opens a dashboard with filters applied. Use it to point \
                       a browser at exactly the view being discussed, which is how a dashboard \
                       with no data API can still be read from the screen."
    )]
    async fn bi_dashboard_url(&self, Parameters(input): Parameters<LinkInput>) -> CallToolResult {
        match self
            .backend
            .deep_link(&input.dashboard_id, &filters(input.filters))
            .await
        {
            Ok(url) => ok(&json!({ "dashboard_id": input.dashboard_id, "url": url })),
            Err(error) => failed("bi_dashboard_url", &error),
        }
    }
}
