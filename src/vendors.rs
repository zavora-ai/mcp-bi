// Licensed under the Apache License, Version 2.0.

//! The five commercial platforms, as optional backends.
//!
//! Power BI, Tableau, Looker, Qlik Sense and Amazon QuickSight — the platforms an
//! enterprise is most likely to already own. They are optional on purpose: the
//! server's primary target is Apache Superset, and none of these is needed to run
//! it. Each is mapped onto the same trait so an agent's calls do not change when a
//! company switches vendor.
//!
//! Each adapter reports its real limits rather than papering over them. Two are
//! worth knowing before you plan around them:
//!
//! - **Power BI's real-time path is closing.** Push and streaming semantic models
//!   can no longer be created, and existing ones retire on 31 October 2027. What
//!   remains stable is reading a model with DAX and exporting a report to an
//!   image, which is what this adapter uses. For streaming, Microsoft points at
//!   Fabric Real-Time Intelligence.
//! - **QuickSight does not return arbitrary query results.** It will describe a
//!   dashboard and generate a snapshot, but the data behind a visual is not an
//!   API. `chart_data` therefore fails honestly instead of inventing rows.

use crate::backend::{BiBackend, unsupported};
use crate::http::Http;
use crate::types::{
    Capabilities, ChartRef, Column, Dashboard, Dataset, DrillResult, Filter, Table,
};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;

fn require(variable: &str) -> Result<String> {
    std::env::var(variable)
        .map_err(|_| anyhow!("{variable} is not set. This backend needs it to authenticate."))
}

fn optional(variable: &str, fallback: &str) -> String {
    std::env::var(variable)
        .unwrap_or_else(|_| fallback.to_string())
        .trim_end_matches('/')
        .to_string()
}

/// Columns and rows out of records, with a pinned column order.
fn table_from_records(records: &[Value]) -> Table {
    let columns: Vec<String> = records
        .first()
        .and_then(Value::as_object)
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_default();
    Table {
        rows: records
            .iter()
            .map(|record| {
                columns
                    .iter()
                    .map(|column| record.get(column).cloned().unwrap_or(Value::Null))
                    .collect()
            })
            .collect(),
        columns,
        truncated: false,
    }
}

// ── Microsoft Power BI ──────────────────────────────────────────────────────

pub struct PowerBi {
    http: Arc<dyn Http>,
    base: String,
    token: String,
    /// Optional workspace. Without one, the APIs address "My workspace".
    group: Option<String>,
}

impl PowerBi {
    pub fn from_env(http: Arc<dyn Http>) -> Result<Self> {
        Ok(Self {
            http,
            base: optional("POWERBI_API", "https://api.powerbi.com/v1.0/myorg"),
            token: require("POWERBI_TOKEN")?,
            group: std::env::var("POWERBI_GROUP_ID").ok(),
        })
    }

    pub fn new(http: Arc<dyn Http>, base: &str, token: &str, group: Option<&str>) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
            group: group.map(str::to_string),
        }
    }

    fn headers(&self) -> Vec<(String, String)> {
        vec![
            ("Authorization".into(), format!("Bearer {}", self.token)),
            ("Accept".into(), "application/json".into()),
        ]
    }

    /// Workspace-scoped path, or the personal workspace when no group is set.
    fn scope(&self, tail: &str) -> String {
        match &self.group {
            Some(group) => format!("{}/groups/{group}/{tail}", self.base),
            None => format!("{}/{tail}", self.base),
        }
    }
}

#[async_trait]
impl BiBackend for PowerBi {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            backend: "powerbi".into(),
            open_source: false,
            list_dashboards: true,
            chart_data: false,
            raw_query: true,
            drill_down: true,
            export_image: true,
            deep_links: true,
            notes: vec![
                "Reads with DAX through the Execute Queries endpoint, capped by Microsoft at \
                 100,000 rows or 1,000,000 values per query."
                    .into(),
                "There is no API for the data behind a single visual, so chart_data is not \
                 offered; query the model with bi_query and DAX instead."
                    .into(),
                "Push and streaming semantic models can no longer be created and retire on \
                 2027-10-31. For streaming, Microsoft points at Fabric Real-Time Intelligence."
                    .into(),
            ],
        }
    }

    async fn list_dashboards(&self) -> Result<Vec<Dashboard>> {
        // Reports are what people mean by a dashboard in Power BI; the Dashboards
        // API covers the older pinned-tile artefact.
        let body = self
            .http
            .get_json(&self.scope("reports"), &self.headers())
            .await?;
        Ok(body
            .get("value")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|report| Dashboard {
                id: report
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                title: report
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("(untitled)")
                    .to_string(),
                description: report
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                charts: Vec::new(),
                filters: Vec::new(),
                url: report
                    .get("webUrl")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                modified_at: None,
            })
            .collect())
    }

    async fn get_dashboard(&self, id: &str) -> Result<Dashboard> {
        let report = self
            .http
            .get_json(&self.scope(&format!("reports/{id}")), &self.headers())
            .await?;
        // Pages are the closest thing to charts that the REST surface exposes.
        let pages = self
            .http
            .get_json(&self.scope(&format!("reports/{id}/pages")), &self.headers())
            .await
            .unwrap_or(Value::Null);
        Ok(Dashboard {
            id: id.to_string(),
            title: report
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("(untitled)")
                .to_string(),
            description: report
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string),
            charts: pages
                .get("value")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|page| ChartRef {
                    id: page
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    title: page
                        .get("displayName")
                        .and_then(Value::as_str)
                        .unwrap_or("(untitled page)")
                        .to_string(),
                    kind: "page".into(),
                    dataset_id: report
                        .get("datasetId")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    dimensions: Vec::new(),
                    metrics: Vec::new(),
                })
                .collect(),
            filters: Vec::new(),
            url: report
                .get("webUrl")
                .and_then(Value::as_str)
                .map(str::to_string),
            modified_at: None,
        })
    }

    async fn list_datasets(&self) -> Result<Vec<Dataset>> {
        let body = self
            .http
            .get_json(&self.scope("datasets"), &self.headers())
            .await?;
        Ok(body
            .get("value")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|dataset| Dataset {
                id: dataset
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                name: dataset
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("(unnamed)")
                    .to_string(),
                schema: None,
                columns: Vec::new(),
                row_count: None,
            })
            .collect())
    }

    async fn describe_dataset(&self, id: &str) -> Result<Dataset> {
        // A semantic model's shape comes from DAX over the model's own metadata,
        // which is the only route the REST API offers without XMLA.
        let table = self
            .query(id, "EVALUATE SELECTCOLUMNS(INFO.COLUMNS(), \"name\", [ExplicitName], \"kind\", [DataType])", 500)
            .await
            .unwrap_or(Table { columns: vec![], rows: vec![], truncated: false });
        Ok(Dataset {
            id: id.to_string(),
            name: id.to_string(),
            schema: None,
            columns: table
                .rows
                .iter()
                .filter_map(|row| {
                    let name = row.first()?.as_str()?.to_string();
                    let kind = row
                        .get(1)
                        .and_then(Value::as_str)
                        .unwrap_or("string")
                        .to_lowercase();
                    Some(Column {
                        groupable: !kind.contains("double") && !kind.contains("decimal"),
                        kind: if kind.contains("int")
                            || kind.contains("double")
                            || kind.contains("decimal")
                        {
                            "number".into()
                        } else if kind.contains("date") {
                            "time".into()
                        } else {
                            "string".into()
                        },
                        name,
                    })
                })
                .collect(),
            row_count: None,
        })
    }

    async fn chart_data(&self, _chart_id: &str, _filters: &[Filter]) -> Result<Table> {
        Err(unsupported(
            "powerbi",
            "reading the data behind one visual; query the semantic model with bi_query and DAX",
        ))
    }

    async fn drill_down(
        &self,
        chart_id: &str,
        dimension: Option<&str>,
        filters: &[Filter],
    ) -> Result<DrillResult> {
        let _ = (chart_id, dimension, filters);
        Err(unsupported(
            "powerbi",
            "drill-down on a visual; express the breakdown as DAX with bi_query, \
             or open the report with a filter using bi_dashboard_url",
        ))
    }

    async fn query(&self, dataset_id: &str, dax: &str, limit: usize) -> Result<Table> {
        let body = self
            .http
            .post_json(
                &self.scope(&format!("datasets/{dataset_id}/executeQueries")),
                &self.headers(),
                json!({ "queries": [{ "query": dax }], "serializerSettings": { "includeNulls": true } }),
            )
            .await?;
        let records = body
            .get("results")
            .and_then(Value::as_array)
            .and_then(|results| results.first())
            .and_then(|result| result.get("tables"))
            .and_then(Value::as_array)
            .and_then(|tables| tables.first())
            .and_then(|table| table.get("rows"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut table = table_from_records(&records);
        if table.rows.len() > limit {
            table.rows.truncate(limit);
            table.truncated = true;
        }
        Ok(table)
    }

    async fn export_image(&self, dashboard_id: &str) -> Result<(Vec<u8>, String)> {
        // Export is asynchronous: this starts it and reports what to poll, rather
        // than blocking a tool call for a minute or pretending it finished.
        let started = self
            .http
            .post_json(
                &self.scope(&format!("reports/{dashboard_id}/ExportTo")),
                &self.headers(),
                json!({ "format": "PNG" }),
            )
            .await?;
        Err(anyhow!(
            "Power BI renders exports asynchronously. Export {} was accepted; poll \
             reports/{dashboard_id}/exports/{} until its status is Succeeded, then fetch /file. \
             For an immediate picture, open bi_dashboard_url and capture the window.",
            started
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("(no id returned)"),
            started
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("{exportId}")
        ))
    }

    async fn deep_link(&self, dashboard_id: &str, filters: &[Filter]) -> Result<String> {
        // Power BI takes report filters in a URL as `filter=Table/Column eq 'value'`.
        let expression = filters
            .iter()
            .map(|filter| {
                let value = filter
                    .value
                    .as_str()
                    .map(|text| format!("'{text}'"))
                    .unwrap_or_else(|| filter.value.to_string());
                let operator = match filter.op.as_str() {
                    "=" => "eq",
                    "!=" => "ne",
                    ">" => "gt",
                    "<" => "lt",
                    ">=" => "ge",
                    "<=" => "le",
                    "in" => "in",
                    other => other,
                };
                format!("{} {operator} {value}", filter.column)
            })
            .collect::<Vec<_>>()
            .join(" and ");
        Ok(if expression.is_empty() {
            format!("https://app.powerbi.com/reportEmbed?reportId={dashboard_id}")
        } else {
            format!(
                "https://app.powerbi.com/reportEmbed?reportId={dashboard_id}&$filter={}",
                urlencoding::encode(&expression)
            )
        })
    }
}

// ── Salesforce Tableau ──────────────────────────────────────────────────────

pub struct Tableau {
    http: Arc<dyn Http>,
    base: String,
    token: String,
    site_id: String,
}

impl Tableau {
    pub fn from_env(http: Arc<dyn Http>) -> Result<Self> {
        Ok(Self {
            http,
            base: optional("TABLEAU_URL", "https://10ax.online.tableau.com"),
            token: require("TABLEAU_TOKEN")?,
            site_id: require("TABLEAU_SITE_ID")?,
        })
    }

    pub fn new(http: Arc<dyn Http>, base: &str, token: &str, site_id: &str) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
            site_id: site_id.to_string(),
        }
    }

    fn headers(&self) -> Vec<(String, String)> {
        vec![
            ("X-Tableau-Auth".into(), self.token.clone()),
            ("Accept".into(), "application/json".into()),
        ]
    }

    fn api(&self, tail: &str) -> String {
        format!("{}/api/3.22/sites/{}/{tail}", self.base, self.site_id)
    }
}

#[async_trait]
impl BiBackend for Tableau {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            backend: "tableau".into(),
            open_source: false,
            list_dashboards: true,
            chart_data: true,
            raw_query: false,
            drill_down: false,
            export_image: true,
            deep_links: true,
            notes: vec![
                "Workbooks are dashboards and views are their sheets.".into(),
                "chart_data reads a view's summary CSV, which is the numbers Tableau itself \
                 renders. It is not arbitrary SQL: use the VizQL Data Service for that."
                    .into(),
            ],
        }
    }

    async fn list_dashboards(&self) -> Result<Vec<Dashboard>> {
        let body = self
            .http
            .get_json(&self.api("workbooks"), &self.headers())
            .await?;
        let workbooks = body
            .get("workbooks")
            .and_then(|wrapper| wrapper.get("workbook"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(workbooks
            .iter()
            .map(|workbook| Dashboard {
                id: workbook
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                title: workbook
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("(untitled)")
                    .to_string(),
                description: workbook
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                charts: Vec::new(),
                filters: Vec::new(),
                url: workbook
                    .get("webpageUrl")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                modified_at: workbook
                    .get("updatedAt")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
            .collect())
    }

    async fn get_dashboard(&self, id: &str) -> Result<Dashboard> {
        let body = self
            .http
            .get_json(&self.api(&format!("workbooks/{id}/views")), &self.headers())
            .await?;
        let views = body
            .get("views")
            .and_then(|wrapper| wrapper.get("view"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(Dashboard {
            id: id.to_string(),
            title: id.to_string(),
            description: None,
            charts: views
                .iter()
                .map(|view| ChartRef {
                    id: view
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    title: view
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("(untitled)")
                        .to_string(),
                    kind: "view".into(),
                    dataset_id: None,
                    dimensions: Vec::new(),
                    metrics: Vec::new(),
                })
                .collect(),
            filters: Vec::new(),
            url: Some(format!("{}/#/workbooks/{id}", self.base)),
            modified_at: None,
        })
    }

    async fn list_datasets(&self) -> Result<Vec<Dataset>> {
        let body = self
            .http
            .get_json(&self.api("datasources"), &self.headers())
            .await?;
        Ok(body
            .get("datasources")
            .and_then(|wrapper| wrapper.get("datasource"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|source| Dataset {
                id: source
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                name: source
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("(unnamed)")
                    .to_string(),
                schema: None,
                columns: Vec::new(),
                row_count: None,
            })
            .collect())
    }

    async fn describe_dataset(&self, id: &str) -> Result<Dataset> {
        Ok(Dataset {
            id: id.to_string(),
            name: id.to_string(),
            schema: None,
            columns: Vec::new(),
            row_count: None,
        })
    }

    async fn chart_data(&self, chart_id: &str, _filters: &[Filter]) -> Result<Table> {
        // Views expose summary data as CSV, so this parses rather than deserialises.
        let (bytes, _) = self
            .http
            .get_bytes(
                &self.api(&format!("views/{chart_id}/data")),
                &self.headers(),
            )
            .await?;
        let text = String::from_utf8_lossy(&bytes);
        let mut lines = text.lines();
        let columns: Vec<String> = lines
            .next()
            .map(|header| {
                header
                    .split(',')
                    .map(|cell| cell.trim_matches('"').to_string())
                    .collect()
            })
            .unwrap_or_default();
        Ok(Table {
            rows: lines
                .map(|line| {
                    line.split(',')
                        .map(|cell| {
                            let cell = cell.trim().trim_matches('"');
                            cell.parse::<f64>()
                                .map(|number| json!(number))
                                .unwrap_or_else(|_| json!(cell))
                        })
                        .collect()
                })
                .collect(),
            columns,
            truncated: false,
        })
    }

    async fn drill_down(
        &self,
        chart_id: &str,
        dimension: Option<&str>,
        filters: &[Filter],
    ) -> Result<DrillResult> {
        let _ = (chart_id, dimension, filters);
        Err(unsupported(
            "tableau",
            "server-side drill-down; open the view with a filter using bi_dashboard_url",
        ))
    }

    async fn export_image(&self, dashboard_id: &str) -> Result<(Vec<u8>, String)> {
        self.http
            .get_bytes(
                &self.api(&format!("views/{dashboard_id}/image")),
                &self.headers(),
            )
            .await
    }

    async fn deep_link(&self, dashboard_id: &str, filters: &[Filter]) -> Result<String> {
        // Tableau takes filters as `?Field=value` on the view URL.
        let query = filters
            .iter()
            .map(|filter| {
                let value = filter
                    .value
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| filter.value.to_string());
                format!(
                    "{}={}",
                    urlencoding::encode(&filter.column),
                    urlencoding::encode(&value)
                )
            })
            .collect::<Vec<_>>()
            .join("&");
        Ok(if query.is_empty() {
            format!("{}/#/views/{dashboard_id}", self.base)
        } else {
            format!("{}/#/views/{dashboard_id}?{query}", self.base)
        })
    }
}

// ── Google Looker ───────────────────────────────────────────────────────────

pub struct Looker {
    http: Arc<dyn Http>,
    base: String,
    token: String,
}

impl Looker {
    pub fn from_env(http: Arc<dyn Http>) -> Result<Self> {
        Ok(Self {
            http,
            base: optional("LOOKER_URL", "https://example.cloud.looker.com"),
            token: require("LOOKER_TOKEN")?,
        })
    }

    pub fn new(http: Arc<dyn Http>, base: &str, token: &str) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
        }
    }

    fn headers(&self) -> Vec<(String, String)> {
        vec![
            ("Authorization".into(), format!("Bearer {}", self.token)),
            ("Accept".into(), "application/json".into()),
        ]
    }

    fn api(&self, tail: &str) -> String {
        format!("{}/api/4.0/{tail}", self.base)
    }
}

#[async_trait]
impl BiBackend for Looker {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            backend: "looker".into(),
            open_source: false,
            list_dashboards: true,
            chart_data: true,
            raw_query: true,
            drill_down: true,
            export_image: false,
            deep_links: true,
            notes: vec![
                "Queries run against LookML explores rather than raw tables, so bi_query takes \
                 an explore reference and returns modelled results."
                    .into(),
                "Rendering a dashboard to an image is an async render task; not wired here.".into(),
            ],
        }
    }

    async fn list_dashboards(&self) -> Result<Vec<Dashboard>> {
        let body = self
            .http
            .get_json(&self.api("dashboards"), &self.headers())
            .await?;
        Ok(body
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|dashboard| Dashboard {
                id: dashboard
                    .get("id")
                    .map(|id| {
                        id.as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| id.to_string())
                    })
                    .unwrap_or_default(),
                title: dashboard
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("(untitled)")
                    .to_string(),
                description: dashboard
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                charts: Vec::new(),
                filters: Vec::new(),
                url: dashboard.get("id").map(|id| {
                    format!(
                        "{}/dashboards/{}",
                        self.base,
                        id.as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| id.to_string())
                    )
                }),
                modified_at: dashboard
                    .get("updated_at")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
            .collect())
    }

    async fn get_dashboard(&self, id: &str) -> Result<Dashboard> {
        let body = self
            .http
            .get_json(&self.api(&format!("dashboards/{id}")), &self.headers())
            .await?;
        Ok(Dashboard {
            id: id.to_string(),
            title: body
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("(untitled)")
                .to_string(),
            description: body
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string),
            charts: body
                .get("dashboard_elements")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|element| ChartRef {
                    id: element
                        .get("id")
                        .map(|id| {
                            id.as_str()
                                .map(str::to_string)
                                .unwrap_or_else(|| id.to_string())
                        })
                        .unwrap_or_default(),
                    title: element
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or("(untitled tile)")
                        .to_string(),
                    kind: element
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("vis")
                        .to_string(),
                    dataset_id: element
                        .get("query")
                        .and_then(|query| query.get("model"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    dimensions: Vec::new(),
                    metrics: Vec::new(),
                })
                .collect(),
            filters: Vec::new(),
            url: Some(format!("{}/dashboards/{id}", self.base)),
            modified_at: None,
        })
    }

    async fn list_datasets(&self) -> Result<Vec<Dataset>> {
        let body = self
            .http
            .get_json(&self.api("lookml_models"), &self.headers())
            .await?;
        Ok(body
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|model| Dataset {
                id: model
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                name: model
                    .get("label")
                    .and_then(Value::as_str)
                    .unwrap_or("(unnamed)")
                    .to_string(),
                schema: None,
                columns: Vec::new(),
                row_count: None,
            })
            .collect())
    }

    async fn describe_dataset(&self, id: &str) -> Result<Dataset> {
        Ok(Dataset {
            id: id.to_string(),
            name: id.to_string(),
            schema: None,
            columns: Vec::new(),
            row_count: None,
        })
    }

    async fn chart_data(&self, chart_id: &str, _filters: &[Filter]) -> Result<Table> {
        let body = self
            .http
            .get_json(
                &self.api(&format!("dashboard_elements/{chart_id}/query/run/json")),
                &self.headers(),
            )
            .await?;
        Ok(table_from_records(
            &body.as_array().cloned().unwrap_or_default(),
        ))
    }

    async fn drill_down(
        &self,
        chart_id: &str,
        dimension: Option<&str>,
        filters: &[Filter],
    ) -> Result<DrillResult> {
        let _ = (dimension, filters);
        let table = self.chart_data(chart_id, &[]).await?;
        Err(unsupported(
            "looker",
            &format!(
                "drill-down through a dashboard element (the tile returned {} rows unfiltered); \
                 run an inline query against the explore with bi_query instead",
                table.rows.len()
            ),
        ))
    }

    async fn query(&self, model: &str, query: &str, limit: usize) -> Result<Table> {
        // Looker's inline query takes a JSON body describing the explore.
        let body: Value = serde_json::from_str(query).map_err(|error| {
            anyhow!(
                "Looker queries are JSON describing an explore, not SQL ({error}). \
                 Send something like {{\"view\":\"orders\",\"fields\":[\"orders.count\"]}}."
            )
        })?;
        let mut request = body;
        request["model"] = json!(model);
        request["limit"] = json!(limit.to_string());
        let response = self
            .http
            .post_json(&self.api("queries/run/json"), &self.headers(), request)
            .await?;
        Ok(table_from_records(
            &response.as_array().cloned().unwrap_or_default(),
        ))
    }

    async fn deep_link(&self, dashboard_id: &str, filters: &[Filter]) -> Result<String> {
        let query = filters
            .iter()
            .map(|filter| {
                let value = filter
                    .value
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| filter.value.to_string());
                format!(
                    "{}={}",
                    urlencoding::encode(&filter.column),
                    urlencoding::encode(&value)
                )
            })
            .collect::<Vec<_>>()
            .join("&");
        Ok(if query.is_empty() {
            format!("{}/dashboards/{dashboard_id}", self.base)
        } else {
            format!("{}/dashboards/{dashboard_id}?{query}", self.base)
        })
    }
}

// ── Qlik Sense ──────────────────────────────────────────────────────────────

pub struct Qlik {
    http: Arc<dyn Http>,
    base: String,
    token: String,
}

impl Qlik {
    pub fn from_env(http: Arc<dyn Http>) -> Result<Self> {
        Ok(Self {
            http,
            base: optional("QLIK_URL", "https://example.us.qlikcloud.com"),
            token: require("QLIK_TOKEN")?,
        })
    }

    pub fn new(http: Arc<dyn Http>, base: &str, token: &str) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
        }
    }

    fn headers(&self) -> Vec<(String, String)> {
        vec![
            ("Authorization".into(), format!("Bearer {}", self.token)),
            ("Accept".into(), "application/json".into()),
        ]
    }
}

#[async_trait]
impl BiBackend for Qlik {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            backend: "qlik".into(),
            open_source: false,
            list_dashboards: true,
            chart_data: false,
            raw_query: false,
            drill_down: false,
            export_image: false,
            deep_links: true,
            notes: vec![
                "Qlik apps are read through the Engine JSON API over a WebSocket, not REST, so \
                 only discovery and deep links are wired here."
                    .into(),
                "For the numbers behind a visual, use the Engine API or open the sheet with \
                 bi_dashboard_url and read it from the screen."
                    .into(),
            ],
        }
    }

    async fn list_dashboards(&self) -> Result<Vec<Dashboard>> {
        let body = self
            .http
            .get_json(
                &format!("{}/api/v1/items?resourceType=app", self.base),
                &self.headers(),
            )
            .await?;
        Ok(body
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|item| Dashboard {
                id: item
                    .get("resourceId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                title: item
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("(untitled)")
                    .to_string(),
                description: item
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                charts: Vec::new(),
                filters: Vec::new(),
                url: item
                    .get("resourceId")
                    .and_then(Value::as_str)
                    .map(|id| format!("{}/sense/app/{id}", self.base)),
                modified_at: item
                    .get("updatedAt")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
            .collect())
    }

    async fn get_dashboard(&self, id: &str) -> Result<Dashboard> {
        Ok(Dashboard {
            id: id.to_string(),
            title: id.to_string(),
            description: Some(
                "Qlik sheets and their objects come from the Engine JSON API over a WebSocket, \
                 which this adapter does not open."
                    .into(),
            ),
            charts: Vec::new(),
            filters: Vec::new(),
            url: Some(format!("{}/sense/app/{id}", self.base)),
            modified_at: None,
        })
    }

    async fn list_datasets(&self) -> Result<Vec<Dataset>> {
        Err(unsupported("qlik", "listing datasets over REST"))
    }

    async fn describe_dataset(&self, _id: &str) -> Result<Dataset> {
        Err(unsupported("qlik", "describing a dataset over REST"))
    }

    async fn chart_data(&self, _chart_id: &str, _filters: &[Filter]) -> Result<Table> {
        Err(unsupported("qlik", "reading a visual's data over REST"))
    }

    async fn drill_down(
        &self,
        _chart_id: &str,
        _dimension: Option<&str>,
        _filters: &[Filter],
    ) -> Result<DrillResult> {
        Err(unsupported("qlik", "drill-down over REST"))
    }

    async fn deep_link(&self, dashboard_id: &str, filters: &[Filter]) -> Result<String> {
        // Qlik selections travel as a `select` bookmark expression in the URL.
        let selection = filters
            .iter()
            .map(|filter| {
                let value = filter
                    .value
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| filter.value.to_string());
                format!(
                    "select/{}/{}",
                    urlencoding::encode(&filter.column),
                    urlencoding::encode(&value)
                )
            })
            .collect::<Vec<_>>()
            .join("/");
        Ok(if selection.is_empty() {
            format!("{}/sense/app/{dashboard_id}", self.base)
        } else {
            format!("{}/sense/app/{dashboard_id}/{selection}", self.base)
        })
    }
}

// ── Amazon QuickSight ───────────────────────────────────────────────────────

pub struct QuickSight {
    http: Arc<dyn Http>,
    base: String,
    token: String,
    account_id: String,
}

impl QuickSight {
    pub fn from_env(http: Arc<dyn Http>) -> Result<Self> {
        let region = optional("AWS_REGION", "us-east-1");
        Ok(Self {
            http,
            base: format!("https://quicksight.{region}.amazonaws.com"),
            token: require("QUICKSIGHT_TOKEN")?,
            account_id: require("AWS_ACCOUNT_ID")?,
        })
    }

    pub fn new(http: Arc<dyn Http>, base: &str, token: &str, account_id: &str) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
            account_id: account_id.to_string(),
        }
    }

    fn headers(&self) -> Vec<(String, String)> {
        vec![
            ("Authorization".into(), self.token.clone()),
            ("Accept".into(), "application/json".into()),
        ]
    }
}

#[async_trait]
impl BiBackend for QuickSight {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            backend: "quicksight".into(),
            open_source: false,
            list_dashboards: true,
            chart_data: false,
            raw_query: false,
            drill_down: false,
            export_image: false,
            deep_links: true,
            notes: vec![
                "QuickSight exposes no API for the data behind a visual, so chart_data and \
                 drill_down are not offered rather than being faked."
                    .into(),
                "Snapshot export is an async job and needs SigV4-signed requests; supply a \
                 pre-signed Authorization header if you wire it up."
                    .into(),
                "Embed URLs are generated per user and expire, so bi_dashboard_url returns the \
                 console link instead."
                    .into(),
            ],
        }
    }

    async fn list_dashboards(&self) -> Result<Vec<Dashboard>> {
        let body = self
            .http
            .get_json(
                &format!("{}/accounts/{}/dashboards", self.base, self.account_id),
                &self.headers(),
            )
            .await?;
        Ok(body
            .get("DashboardSummaryList")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|dashboard| Dashboard {
                id: dashboard
                    .get("DashboardId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                title: dashboard
                    .get("Name")
                    .and_then(Value::as_str)
                    .unwrap_or("(untitled)")
                    .to_string(),
                description: None,
                charts: Vec::new(),
                filters: Vec::new(),
                url: dashboard
                    .get("DashboardId")
                    .and_then(Value::as_str)
                    .map(|id| format!("https://quicksight.aws.amazon.com/sn/dashboards/{id}")),
                modified_at: dashboard
                    .get("LastUpdatedTime")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
            .collect())
    }

    async fn get_dashboard(&self, id: &str) -> Result<Dashboard> {
        let body = self
            .http
            .get_json(
                &format!("{}/accounts/{}/dashboards/{id}", self.base, self.account_id),
                &self.headers(),
            )
            .await?;
        let dashboard = body.get("Dashboard").unwrap_or(&body);
        Ok(Dashboard {
            id: id.to_string(),
            title: dashboard
                .get("Name")
                .and_then(Value::as_str)
                .unwrap_or("(untitled)")
                .to_string(),
            description: None,
            charts: dashboard
                .get("Version")
                .and_then(|version| version.get("Sheets"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|sheet| ChartRef {
                    id: sheet
                        .get("SheetId")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    title: sheet
                        .get("Name")
                        .and_then(Value::as_str)
                        .unwrap_or("(untitled sheet)")
                        .to_string(),
                    kind: "sheet".into(),
                    dataset_id: None,
                    dimensions: Vec::new(),
                    metrics: Vec::new(),
                })
                .collect(),
            filters: Vec::new(),
            url: Some(format!(
                "https://quicksight.aws.amazon.com/sn/dashboards/{id}"
            )),
            modified_at: None,
        })
    }

    async fn list_datasets(&self) -> Result<Vec<Dataset>> {
        let body = self
            .http
            .get_json(
                &format!("{}/accounts/{}/data-sets", self.base, self.account_id),
                &self.headers(),
            )
            .await?;
        Ok(body
            .get("DataSetSummaries")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|set| Dataset {
                id: set
                    .get("DataSetId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                name: set
                    .get("Name")
                    .and_then(Value::as_str)
                    .unwrap_or("(unnamed)")
                    .to_string(),
                schema: None,
                columns: Vec::new(),
                row_count: None,
            })
            .collect())
    }

    async fn describe_dataset(&self, id: &str) -> Result<Dataset> {
        Ok(Dataset {
            id: id.to_string(),
            name: id.to_string(),
            schema: None,
            columns: Vec::new(),
            row_count: None,
        })
    }

    async fn chart_data(&self, _chart_id: &str, _filters: &[Filter]) -> Result<Table> {
        Err(unsupported("quicksight", "reading a visual's data"))
    }

    async fn drill_down(
        &self,
        _chart_id: &str,
        _dimension: Option<&str>,
        _filters: &[Filter],
    ) -> Result<DrillResult> {
        Err(unsupported("quicksight", "drill-down"))
    }

    async fn deep_link(&self, dashboard_id: &str, _filters: &[Filter]) -> Result<String> {
        Ok(format!(
            "https://quicksight.aws.amazon.com/sn/dashboards/{dashboard_id}"
        ))
    }
}
