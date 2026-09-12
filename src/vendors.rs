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
use anyhow::{Context, Result, anyhow, bail};
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
        // Both artefacts, because Power BI has two and a person means either.
        //
        // The comment this replaces asserted that "reports are what people mean by a
        // dashboard; the Dashboards API covers the older pinned-tile artefact", and
        // listed only reports. Against a real tenant that hid the thing the owner
        // called their dashboard: the report's pages were named "Page 1", "Page 2",
        // "Page 3", while the dashboard beside it held 14 tiles named
        // "No. of Houses with Water", "Satisfied with Water Services", "Responses".
        // An agent choosing what to look at needs the second set.
        let mut listed = Vec::new();

        // Dashboards first, so the named tiles lead.
        if let Ok(body) = self
            .http
            .get_json(&self.scope("dashboards"), &self.headers())
            .await
        {
            for dashboard in body
                .get("value")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                listed.push(Dashboard {
                    id: dashboard
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    title: dashboard
                        .get("displayName")
                        .and_then(Value::as_str)
                        .unwrap_or("(untitled)")
                        .to_string(),
                    // Power BI carries no description on a dashboard, so the kind goes
                    // here: an agent has to know a tile collection behaves differently
                    // from a report's pages.
                    description: Some("Power BI dashboard (pinned tiles)".to_string()),
                    charts: Vec::new(),
                    filters: Vec::new(),
                    url: dashboard
                        .get("webUrl")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    modified_at: None,
                });
            }
        }

        // Reports second. A failure here is worth surfacing, since a tenant with no
        // dashboards and no readable reports is a configuration problem rather than an
        // empty account.
        let body = self
            .http
            .get_json(&self.scope("reports"), &self.headers())
            .await?;
        for report in body
            .get("value")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            listed.push(Dashboard {
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
                    .map(str::to_string)
                    .or_else(|| Some("Power BI report (pages of visuals)".to_string())),
                charts: Vec::new(),
                filters: Vec::new(),
                url: report
                    .get("webUrl")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                modified_at: None,
            });
        }
        Ok(listed)
    }

    async fn get_dashboard(&self, id: &str) -> Result<Dashboard> {
        // A Power BI id could name either artefact, and the caller should not have to
        // know which. Tiles are asked for first because they are the more useful answer
        // when they exist: a real dashboard here returned 14 tiles named after what they
        // measure, while the report beside it had three pages named "Page 1" to "Page 3".
        if let Ok(tiles) = self
            .http
            .get_json(
                &self.scope(&format!("dashboards/{id}/tiles")),
                &self.headers(),
            )
            .await
        {
            let tiles = tiles
                .get("value")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if !tiles.is_empty() {
                let title = self
                    .http
                    .get_json(&self.scope(&format!("dashboards/{id}")), &self.headers())
                    .await
                    .ok()
                    .and_then(|body| {
                        body.get("displayName")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .unwrap_or_else(|| "(untitled)".to_string());
                return Ok(Dashboard {
                    id: id.to_string(),
                    title,
                    description: Some(format!(
                        "Power BI dashboard with {} pinned tiles. A tile's underlying query is \
                         not exposed by the REST API — use bi_query with DAX against the tile's \
                         dataset to get numbers.",
                        tiles.len()
                    )),
                    charts: tiles
                        .iter()
                        .map(|tile| ChartRef {
                            id: tile
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                            // A tile pinned from a whole report page carries no title.
                            // Saying so beats an empty string, which reads as a bug.
                            title: tile
                                .get("title")
                                .and_then(Value::as_str)
                                .unwrap_or("(untitled tile)")
                                .to_string(),
                            kind: "tile".into(),
                            dataset_id: tile
                                .get("datasetId")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            dimensions: Vec::new(),
                            metrics: Vec::new(),
                        })
                        .collect(),
                    filters: Vec::new(),
                    url: Some(format!("https://app.powerbi.com/dashboards/{id}")),
                    modified_at: None,
                });
            }
        }

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
        // The name comes from the dataset itself. It used to be filled with the id,
        // which meant every semantic model described itself as a GUID — visible the
        // moment this ran against a real tenant, where `bi_list_datasets` reported
        // "Water Survey" and `bi_describe_dataset` reported
        // "a-dataset-guid" for the same thing.
        let name = self
            .http
            .get_json(&self.scope(&format!("datasets/{id}")), &self.headers())
            .await
            .ok()
            .and_then(|body| body.get("name").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_else(|| id.to_string());

        // A semantic model's shape comes from DAX over the model's own metadata, which
        // is the only route the REST API offers without XMLA. Which metadata function
        // works is not a matter of documentation:
        //
        //   INFO.COLUMNS()      → HTTP 400 "Failed to execute the DAX query." on a live
        //                         tenant, with error code 3239575574 and no explanation
        //   INFO.VIEW.COLUMNS() → 28 rows, with friendly type names and IsHidden
        //
        // The first is what this adapter shipped with, so it returned nothing at all.
        // Tried in order of usefulness, because availability varies by model and by
        // engine version, and a caller wants columns rather than a lecture.
        let attempts = [
            "EVALUATE SELECTCOLUMNS(INFO.VIEW.COLUMNS(), \"name\", [Name], \"kind\", [DataType], \
             \"hidden\", [IsHidden])",
            "EVALUATE SELECTCOLUMNS(INFO.COLUMNS(), \"name\", [ExplicitName], \"kind\", [DataType])",
            "EVALUATE SELECTCOLUMNS(COLUMNSTATISTICS(), \"name\", [Column Name])",
        ];
        let mut table = None;
        let mut last_error = None;
        for dax in attempts {
            match self.query(id, dax, 500).await {
                Ok(found) if !found.rows.is_empty() => {
                    table = Some(found);
                    break;
                }
                Ok(_) => {}
                Err(error) => last_error = Some(error),
            }
        }
        // Say so rather than returning an empty column list, which reads as "this model
        // has no columns" and is the thing that hid the broken query in the first place.
        let table = match table {
            Some(table) => table,
            None => {
                let detail = last_error
                    .map(|error| format!("{error:#}"))
                    .unwrap_or_else(|| "every metadata query returned no rows".to_string());
                bail!(
                    "Could not read the columns of Power BI dataset {name} ({id}). Reading a \
                     semantic model's shape needs DAX over its metadata, and none of \
                     INFO.VIEW.COLUMNS, INFO.COLUMNS or COLUMNSTATISTICS answered. Last error: \
                     {detail}"
                )
            }
        };

        // Find each alias by name, never by position.
        //
        // This layer returns a DAX result's columns in alphabetical order rather than the
        // order the query asked for: `SELECTCOLUMNS(… "name" … "kind" … "hidden" …)` comes
        // back as `["[hidden]", "[kind]", "[name]"]`. Reading row[0] as the name therefore
        // reads the hidden flag, which is how the original code would have produced
        // nothing even if its metadata function had worked.
        let index_of = |alias: &str| {
            table
                .columns
                .iter()
                .position(|column| column.trim_matches(['[', ']']).eq_ignore_ascii_case(alias))
        };
        let (name_at, kind_at, hidden_at) =
            (index_of("name"), index_of("kind"), index_of("hidden"));
        let name_at = name_at.ok_or_else(|| {
            anyhow!(
                "The metadata query for dataset {name} ({id}) returned no column named `name`; \
                 got {:?}",
                table.columns
            )
        })?;

        Ok(Dataset {
            id: id.to_string(),
            name,
            schema: None,
            columns: table
                .rows
                .iter()
                .filter_map(|row| {
                    // Hidden columns are the model's own bookkeeping — a real model here
                    // carried `RowNumber-2662979B-…`, which is noise to an agent choosing
                    // something to group by.
                    if hidden_at
                        .and_then(|at| row.get(at))
                        .and_then(Value::as_bool)
                        == Some(true)
                    {
                        return None;
                    }
                    let name = row.get(name_at)?.as_str()?.to_string();
                    let kind = kind_at
                        .and_then(|at| row.get(at))
                        .and_then(Value::as_str)
                        .unwrap_or("string")
                        .to_lowercase();
                    // Friendly names from INFO.VIEW.COLUMNS ("Integer", "Text", "Date")
                    // and numeric codes from INFO.COLUMNS both land here, so both
                    // vocabularies are matched.
                    let numeric = ["int", "double", "decimal", "currency", "number"]
                        .iter()
                        .any(|needle| kind.contains(needle));
                    let temporal = ["date", "time"].iter().any(|needle| kind.contains(needle));
                    Some(Column {
                        groupable: !numeric,
                        kind: if numeric {
                            "number".into()
                        } else if temporal {
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
        let started = match self
            .http
            .post_json(
                &self.scope(&format!("reports/{dashboard_id}/ExportTo")),
                &self.headers(),
                json!({ "format": "PNG" }),
            )
            .await
        {
            Ok(started) => started,
            Err(error) => {
                // Measured on a real tenant: `403 InvalidRequest — Export report to image
                // is disabled on tenant level`. Nothing about the request is wrong, and no
                // retry or permission grant on this account will change it, so passing the
                // raw status through invites an agent to keep trying. Name the cause.
                let text = format!("{error:#}");
                if text.contains("disabled on tenant") {
                    bail!(
                        "Power BI export is switched off for this tenant, so no image can be \
                         produced through the API. This is an administrator setting — \
                         \"Export reports as image files\" in the Power BI admin portal — not a \
                         permission on this account, and not something a retry will change. \
                         Use bi_dashboard_url and capture the window instead."
                    )
                }
                return Err(error).context(
                    "Power BI refused to start an export. Export requires the report to be in a \
                     workspace on a Premium or Fabric capacity.",
                );
            }
        };
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
            // Which artefact this id names decides the URL, and getting it wrong produces
            // a link that loads nothing: `reportEmbed?reportId=` with a *dashboard* id is
            // a valid-looking URL for a report that does not exist.
            //
            // The list endpoint is asked rather than the item endpoint, because Power BI
            // gives `webUrl` two different meanings under the same name. Measured:
            //
            //   GET /dashboards        → webUrl = https://app.powerbi.com/groups/me/dashboards/{id}
            //   GET /dashboards/{id}   → webUrl = https://app.powerbi.com/dashboardEmbed?dashboardId={id}&config=…
            //
            // The first is the page a person opens; the second is for hosting inside
            // another application and shows chrome-less content. This tool exists to hand
            // a person a link, so the list's answer is the right one.
            //
            // A filtered link is always the report form, because Power BI's URL filter
            // syntax applies to reports; a dashboard's tiles cannot be filtered this way.
            let from_list = |collection: Value, id: &str| -> Option<String> {
                collection
                    .get("value")?
                    .as_array()?
                    .iter()
                    .find(|item| item.get("id").and_then(Value::as_str) == Some(id))?
                    .get("webUrl")?
                    .as_str()
                    .map(str::to_string)
            };
            let dashboards = self
                .http
                .get_json(&self.scope("dashboards"), &self.headers())
                .await
                .ok()
                .and_then(|body| from_list(body, dashboard_id));
            match dashboards {
                Some(url) => url,
                None => self
                    .http
                    .get_json(&self.scope("reports"), &self.headers())
                    .await
                    .ok()
                    .and_then(|body| from_list(body, dashboard_id))
                    .unwrap_or_else(|| {
                        format!("https://app.powerbi.com/reportEmbed?reportId={dashboard_id}")
                    }),
            }
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

    /// A Qlik app's data model: its tables, their row counts, and every field's type tags.
    ///
    /// The adapter used to refuse anything to do with a model, on the grounds that "Qlik
    /// apps are read through the Engine JSON API over a WebSocket, not REST". That is true
    /// of *sheets and their visuals* — `/apps/{id}/objects` and `/apps/{id}/sheets` both
    /// answer 404 — and not true of the data model, which this endpoint returns over plain
    /// REST. Measured on a live tenant: 6 tables and 35 fields, with row counts of 632,313
    /// for Sales, 245 for Products and 30 for Stores.
    async fn model(&self, app_id: &str) -> Result<Value> {
        self.http
            .get_json(
                &format!("{}/api/v1/apps/{app_id}/data/metadata", self.base),
                &self.headers(),
            )
            .await
            .with_context(|| format!("could not read the data model of Qlik app {app_id}"))
    }

    /// A dataset id names a table inside an app, because that is the pair a query needs.
    ///
    /// Qlik has no single "dataset" object corresponding to a queryable table: an app holds
    /// tables, and the tenant separately holds data *files* under
    /// `/items?resourceType=dataset` — which are `.qvd` and `.txt` artefacts with no schema
    /// endpoint of their own. The useful thing to describe is a table in an app, so the id
    /// carries both.
    fn split_dataset_id(id: &str) -> (&str, Option<&str>) {
        match id.split_once(':') {
            Some((app, table)) if !table.is_empty() => (app, Some(table)),
            _ => (id, None),
        }
    }

    /// Qlik states a field's type as tags rather than a type name.
    fn kind_of(tags: &[&str]) -> &'static str {
        if tags.iter().any(|t| *t == "$timestamp" || *t == "$date") {
            "time"
        } else if tags.iter().any(|t| *t == "$numeric" || *t == "$integer") {
            "number"
        } else {
            "string"
        }
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
                "An app's data model is available over REST: bi_list_datasets reports its \
                 tables with row counts, and bi_describe_dataset reports a table's fields \
                 and their types."
                    .into(),
                "Sheets and their visuals are not. `/apps/{id}/objects` and \
                 `/apps/{id}/sheets` answer 404 — they come from the Engine JSON API over a \
                 WebSocket, which this adapter does not open — so bi_get_dashboard lists no \
                 charts and bi_chart_data has nothing to read."
                    .into(),
                "For the numbers behind a visual, use the Engine API, or open the sheet with \
                 bi_dashboard_url and read it from the screen."
                    .into(),
                "Every field is reported as groupable. Qlik's associative model makes any \
                 field selectable as a dimension, and the metadata carries nothing to infer \
                 a measure from: on a live app `Date_year` has 2 distinct values and \
                 `Quantity` has 3, so cardinality cannot tell them apart."
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
        // The app's own name, not its id. Returning the id as the title made every app
        // present itself as `a bare GUID`, which is the same defect the Power BI
        // adapter had, and it is one REST call away from being right.
        let app = self
            .http
            .get_json(&format!("{}/api/v1/apps/{id}", self.base), &self.headers())
            .await
            .ok();
        let attributes = app.as_ref().and_then(|app| app.get("attributes"));
        let title = attributes
            .and_then(|attributes| attributes.get("name"))
            .and_then(Value::as_str)
            .unwrap_or(id)
            .to_string();

        // Say what *is* reachable, not only what is not. Sheets genuinely need the Engine
        // API — `/apps/{id}/objects` and `/apps/{id}/sheets` both answer 404 on a live
        // tenant, so that limit is measured rather than assumed — but the data model behind
        // them is plain REST, and naming the tools that reach it is more use to a caller
        // than an apology.
        let model = self.model(id).await.ok();
        let tables: Vec<String> = model
            .as_ref()
            .and_then(|model| model.get("tables"))
            .and_then(Value::as_array)
            .map(|tables| {
                tables
                    .iter()
                    .filter(|table| table.get("is_system").and_then(Value::as_bool) != Some(true))
                    .filter_map(|table| table.get("name").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let description = if tables.is_empty() {
            "A Qlik app's sheets and visuals come from the Engine JSON API over a WebSocket, \
             which this adapter does not open. This app reports no data model either, which \
             usually means it has never been reloaded."
                .to_string()
        } else {
            format!(
                "Sheets and their visuals come from the Engine JSON API over a WebSocket, which \
                 this adapter does not open, so no charts are listed. The data model is \
                 available over REST: {} table(s) — {}. Use bi_list_datasets and \
                 bi_describe_dataset, and bi_dashboard_url to open the app.",
                tables.len(),
                tables.join(", ")
            )
        };

        Ok(Dashboard {
            id: id.to_string(),
            title,
            description: Some(description),
            charts: Vec::new(),
            filters: Vec::new(),
            url: Some(format!("{}/sense/app/{id}", self.base)),
            modified_at: attributes
                .and_then(|attributes| attributes.get("modifiedDate"))
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }

    async fn list_datasets(&self) -> Result<Vec<Dataset>> {
        // Each app's tables, which is what a query is written against. This used to refuse
        // as "listing datasets over REST" being unsupported; the app metadata endpoint
        // returns exactly this, so the refusal was wrong rather than cautious.
        let apps = self
            .http
            .get_json(
                &format!("{}/api/v1/items?resourceType=app", self.base),
                &self.headers(),
            )
            .await?;
        let mut listed = Vec::new();
        for app in apps
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let Some(app_id) = app.get("resourceId").and_then(Value::as_str) else {
                continue;
            };
            let app_name = app.get("name").and_then(Value::as_str).unwrap_or(app_id);
            // One app failing should not hide the rest: a tenant can hold an app that has
            // never been reloaded, and it has no model to report.
            let Ok(model) = self.model(app_id).await else {
                continue;
            };
            for table in model
                .get("tables")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                // Qlik's own bookkeeping tables — `$$SysTable 3` and friends — describe the
                // model rather than the business, and are not something to analyse.
                if table.get("is_system").and_then(Value::as_bool) == Some(true) {
                    continue;
                }
                let Some(name) = table.get("name").and_then(Value::as_str) else {
                    continue;
                };
                listed.push(Dataset {
                    id: format!("{app_id}:{name}"),
                    name: format!("{app_name} › {name}"),
                    schema: None,
                    columns: Vec::new(),
                    row_count: table.get("no_of_rows").and_then(Value::as_u64),
                });
            }
        }
        Ok(listed)
    }

    async fn describe_dataset(&self, id: &str) -> Result<Dataset> {
        let (app_id, wanted_table) = Self::split_dataset_id(id);
        let model = self.model(app_id).await?;

        let tables = model
            .get("tables")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        // Name the table if one was asked for, and say so plainly when it is not there —
        // an empty column list would read as a table that has no fields.
        if let Some(wanted) = wanted_table {
            let known: Vec<&str> = tables
                .iter()
                .filter(|table| table.get("is_system").and_then(Value::as_bool) != Some(true))
                .filter_map(|table| table.get("name").and_then(Value::as_str))
                .collect();
            if !known.contains(&wanted) {
                bail!(
                    "Qlik app {app_id} has no table named `{wanted}`. It holds: {}",
                    known.join(", ")
                )
            }
        }

        let row_count = tables
            .iter()
            .find(|table| {
                wanted_table
                    .is_none_or(|wanted| table.get("name").and_then(Value::as_str) == Some(wanted))
            })
            .and_then(|table| table.get("no_of_rows").and_then(Value::as_u64));

        let columns = model
            .get("fields")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|field| {
                let tags: Vec<&str> = field
                    .get("tags")
                    .and_then(Value::as_array)
                    .map(|tags| tags.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                // `$system` and `$hidden` mark Qlik's internal model fields — `$Field`,
                // `$Table`, `$Rows` — which exist in every app and describe nothing about
                // the data. Six of this app's 35 fields were these.
                if tags
                    .iter()
                    .any(|tag| *tag == "$system" || *tag == "$hidden")
                {
                    return None;
                }
                let sources: Vec<&str> = field
                    .get("src_tables")
                    .and_then(Value::as_array)
                    .map(|tables| tables.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                // A key field belongs to several tables, so a request for one table keeps
                // the keys that reach it.
                if let Some(wanted) = wanted_table
                    && !sources.contains(&wanted)
                {
                    return None;
                }
                let kind = Self::kind_of(&tags);
                Some(Column {
                    name: field.get("name").and_then(Value::as_str)?.to_string(),
                    kind: kind.into(),
                    // Every field, including a numeric one.
                    //
                    // Other backends infer "numeric means measure", and that inference is
                    // wrong here for two reasons. Qlik's associative model makes any field
                    // selectable as a dimension — that is the product's central idea, and a
                    // visualisation decides what aggregates, not the model. And the
                    // metadata carries nothing to infer from even if it were appropriate:
                    // measured on a live app, `Date_year` has 2 distinct values and
                    // `Quantity` has 3, so cardinality cannot separate the dimension from
                    // the measure. There is no `SummarizeBy` equivalent to read.
                    //
                    // Claiming `groupable: false` for `Date_year` would stop an agent
                    // grouping by year on a sales model, which is the first thing anyone
                    // would ask for. `kind` still says which fields are numbers, so a
                    // caller can pick sensible measures without being told a falsehood
                    // about what may be grouped.
                    groupable: true,
                })
            })
            .collect();

        let app_name = self
            .http
            .get_json(
                &format!("{}/api/v1/apps/{app_id}", self.base),
                &self.headers(),
            )
            .await
            .ok()
            .and_then(|app| {
                app.get("attributes")?
                    .get("name")?
                    .as_str()
                    .map(str::to_string)
            })
            .unwrap_or_else(|| app_id.to_string());

        Ok(Dataset {
            id: id.to_string(),
            name: match wanted_table {
                Some(table) => format!("{app_name} › {table}"),
                None => app_name,
            },
            schema: None,
            columns,
            row_count,
        })
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
