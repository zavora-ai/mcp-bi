// Licensed under the Apache License, Version 2.0.

//! The open-source backends, and the ones this server is built around.
//!
//! **Apache Superset** is the primary. It is ASF-licensed, self-hostable, has a
//! documented REST API, and pairs with the rest of an open stack. Everything the
//! trait needs it can do: list dashboards and charts, return the data behind a
//! chart, run SQL, render a thumbnail, and accept filter state in a URL.
//!
//! **Metabase** is the second, for the same reasons and a simpler model.
//!
//! Both authenticate with a bearer token obtained from a username and password,
//! which is the shape a self-hosted instance actually has — no OAuth app
//! registration, no tenant, no cloud account.

use crate::backend::{BiBackend, unsupported};
use crate::http::Http;
use crate::types::{
    Capabilities, ChartRef, Column, Dashboard, Dataset, DrillResult, Filter, Table,
};
use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::{Arc, RwLock};

/// Read a bearer token from the environment, or say which variable is missing.
fn token(variable: &str) -> Result<String> {
    std::env::var(variable)
        .map_err(|_| anyhow!("{variable} is not set. This backend needs it to authenticate."))
}

/// Refresh this many seconds before the token's stated expiry, so a request that
/// is issued just under the wire is not rejected on arrival.
const REFRESH_MARGIN_SECS: i64 = 60;

/// Read the `exp` claim from a JWT without verifying it.
///
/// The signature is Superset's business, not ours — all this needs is to know when
/// the token stops being useful so it can be replaced before a call fails. A token
/// that is not a JWT, or carries no `exp`, returns `None` and is treated as good
/// until the server says otherwise.
fn expiry_of(token: &str) -> Option<i64> {
    use base64::Engine as _;
    let claims = token.split('.').nth(1)?;
    let padded = match claims.len() % 4 {
        0 => claims.to_string(),
        remainder => format!("{claims}{}", "=".repeat(4 - remainder)),
    };
    let decoded = base64::engine::general_purpose::URL_SAFE
        .decode(padded)
        .ok()?;
    serde_json::from_slice::<Value>(&decoded)
        .ok()?
        .get("exp")?
        .as_i64()
}

fn base(variable: &str, fallback: &str) -> String {
    std::env::var(variable)
        .unwrap_or_else(|_| fallback.to_string())
        .trim_end_matches('/')
        .to_string()
}

/// Rows out of a `{ "columns": [...], "data": [ { ... } ] }` shaped payload.
///
/// Superset and Metabase both return records rather than positional rows, so the
/// column order has to be pinned down before the rows are flattened, or two calls
/// can disagree about which value is which.
fn table_from_records(records: &[Value], preferred: Option<Vec<String>>) -> Table {
    let columns = preferred.unwrap_or_else(|| {
        records
            .first()
            .and_then(Value::as_object)
            .map(|object| object.keys().cloned().collect())
            .unwrap_or_default()
    });
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

// ── Apache Superset ─────────────────────────────────────────────────────────

pub struct Superset {
    http: Arc<dyn Http>,
    base: String,
    /// The bearer token in use. Behind a lock because a refresh replaces it while
    /// the backend is shared immutably across concurrent tool calls.
    token: RwLock<String>,
    /// When the current token stops being accepted, read from its own `exp` claim.
    expires_at: RwLock<Option<i64>>,
    /// Set when the server may mint its own tokens, which is what lets a session
    /// outlive the 15 minutes one token is good for.
    login: Option<Login>,
}

/// What the server needs to obtain a token itself, rather than being handed one.
struct Login {
    username: String,
    password: String,
    provider: String,
}

impl Superset {
    pub fn from_env(http: Arc<dyn Http>) -> Result<Self> {
        let login = match (
            std::env::var("SUPERSET_USERNAME"),
            std::env::var("SUPERSET_PASSWORD"),
        ) {
            (Ok(username), Ok(password)) => Some(Login {
                username,
                password,
                provider: std::env::var("SUPERSET_AUTH_PROVIDER")
                    .unwrap_or_else(|_| "db".to_string()),
            }),
            _ => None,
        };
        // Either is enough on its own: a token to start with, or credentials to mint
        // one. Neither is a configuration error worth naming both remedies for.
        let token = std::env::var("SUPERSET_TOKEN").unwrap_or_default();
        if token.is_empty() && login.is_none() {
            bail!(
                "Superset needs credentials. Set SUPERSET_USERNAME and SUPERSET_PASSWORD so the \
                 server can obtain and refresh tokens itself, or SUPERSET_TOKEN to supply one \
                 directly — though a Superset access token is only valid for 15 minutes, so a \
                 session longer than that will fail partway through."
            )
        }
        Ok(Self {
            http,
            base: base("SUPERSET_URL", "http://localhost:8088"),
            expires_at: RwLock::new(expiry_of(&token)),
            token: RwLock::new(token),
            login,
        })
    }

    /// For tests, and for a host that already holds a token.
    pub fn new(http: Arc<dyn Http>, base_url: &str, token: &str) -> Self {
        Self {
            http,
            base: base_url.trim_end_matches('/').to_string(),
            expires_at: RwLock::new(expiry_of(token)),
            token: RwLock::new(token.to_string()),
            login: None,
        }
    }

    /// A token to start with *and* the credentials to replace it when it expires.
    ///
    /// This is the durable configuration: no login on startup if a token is already
    /// in hand, and no failure partway through once that token runs out. Pass an
    /// empty token to log in on the first request.
    pub fn with_login(
        http: Arc<dyn Http>,
        base_url: &str,
        token: &str,
        username: &str,
        password: &str,
    ) -> Self {
        Self {
            http,
            base: base_url.trim_end_matches('/').to_string(),
            expires_at: RwLock::new(expiry_of(token)),
            token: RwLock::new(token.to_string()),
            login: Some(Login {
                username: username.to_string(),
                password: password.to_string(),
                provider: "db".to_string(),
            }),
        }
    }

    /// True when the current token is missing, or close enough to expiry that a
    /// request started now could be rejected by the time it arrives.
    fn stale(&self) -> bool {
        if self.token.read().expect("token lock").is_empty() {
            return true;
        }
        match *self.expires_at.read().expect("expiry lock") {
            // A token whose expiry we could not read is assumed good; the caller
            // supplied it deliberately and only the server can judge it.
            None => false,
            Some(exp) => Utc::now().timestamp() + REFRESH_MARGIN_SECS >= exp,
        }
    }

    /// Exchange the configured credentials for a fresh access token.
    async fn refresh(&self, login: &Login) -> Result<()> {
        let body = self
            .http
            .post_json(
                &format!("{}/api/v1/security/login", self.base),
                &[("Content-Type".into(), "application/json".into())],
                json!({
                    "username": login.username,
                    "password": login.password,
                    "provider": login.provider,
                    "refresh": true,
                }),
            )
            .await
            .context("Superset login failed")?;
        let fresh = body
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                anyhow!("Superset login returned no access_token. Check the username and password.")
            })?;
        *self.expires_at.write().expect("expiry lock") = expiry_of(fresh);
        *self.token.write().expect("token lock") = fresh.to_string();
        tracing::info!("refreshed the Superset access token");
        Ok(())
    }

    /// Headers for a request, refreshing the token first when it is about to expire.
    ///
    /// Every request goes through here, so a long analysis session survives a token
    /// lifetime instead of failing partway through with an opaque 401.
    async fn auth(&self) -> Result<Vec<(String, String)>> {
        if self.stale() {
            match &self.login {
                Some(login) => self.refresh(login).await?,
                // Nothing can be done about it here, so say what would fix it rather
                // than letting Superset answer with `{"msg":"Token has expired"}`.
                None => bail!(
                    "The Superset token has expired. A Superset access token is valid for 15 \
                     minutes, which is shorter than a typical analysis session. Set \
                     SUPERSET_USERNAME and SUPERSET_PASSWORD so this server can refresh it \
                     itself, or supply a newer SUPERSET_TOKEN."
                ),
            }
        }
        Ok(self.headers())
    }

    fn headers(&self) -> Vec<(String, String)> {
        vec![
            (
                "Authorization".into(),
                format!("Bearer {}", self.token.read().expect("token lock")),
            ),
            ("Accept".into(), "application/json".into()),
        ]
    }

    /// Superset's simple-filter operators, which are not the ones a dashboard URL uses.
    fn query_operator(op: &str) -> &str {
        match op {
            "=" => "==",
            "in" => "IN",
            "not in" => "NOT IN",
            other => other,
        }
    }

    /// Run a saved chart's own query, optionally regrouped and filtered.
    ///
    /// `GET /api/v1/chart/{id}/data/` only works when a chart has a *saved* query
    /// context, and charts created through the UI usually have none — Superset
    /// answers `400 Chart has no query context saved`. So the query is rebuilt from
    /// the chart's stored `params` and posted to `/api/v1/chart/data`, which is what
    /// the UI itself does and works for any chart.
    async fn run_chart_query(
        &self,
        chart_id: &str,
        dimension: Option<&str>,
        filters: &[Filter],
        row_limit: usize,
    ) -> Result<Table> {
        let chart = self
            .http
            .get_json(
                &format!("{}/api/v1/chart/{chart_id}", self.base),
                &self.auth().await?,
            )
            .await?;
        let result = chart
            .get("result")
            .ok_or_else(|| anyhow!("Superset returned no chart {chart_id}"))?;
        // `params` is a JSON *string* holding the chart's form data.
        let form: Value = result
            .get("params")
            .and_then(Value::as_str)
            .map(|raw| serde_json::from_str(raw).unwrap_or(Value::Null))
            .unwrap_or(Value::Null);
        let datasource_id = result
            .get("datasource_id")
            .cloned()
            .ok_or_else(|| anyhow!("chart {chart_id} has no datasource"))?;
        let datasource_type = result
            .get("datasource_type")
            .and_then(Value::as_str)
            .unwrap_or("table");

        // A drill replaces the grouping; otherwise keep the chart's own.
        let columns: Vec<Value> = match dimension {
            Some(dimension) => vec![json!(dimension)],
            None => form
                .get("groupby")
                .and_then(Value::as_array)
                .cloned()
                .or_else(|| form.get("x_axis").map(|axis| vec![axis.clone()]))
                .unwrap_or_default(),
        };
        let simple_filters: Vec<Value> = filters
            .iter()
            .map(|filter| {
                json!({
                    "col": filter.column,
                    "op": Self::query_operator(&filter.op),
                    "val": filter.value,
                })
            })
            .collect();

        let body = json!({
            "datasource": { "id": datasource_id, "type": datasource_type },
            "force": false,
            "queries": [{
                "columns": columns,
                "metrics": form.get("metrics").cloned().unwrap_or(json!([])),
                "row_limit": row_limit,
                "orderby": [],
                "filters": simple_filters,
                "extras": {},
            }],
            "result_format": "json",
            "result_type": "results",
        });
        let response = self
            .http
            .post_json(
                &format!("{}/api/v1/chart/data", self.base),
                &self.auth().await?,
                body,
            )
            .await?;
        let first = response
            .get("result")
            .and_then(Value::as_array)
            .and_then(|results| results.first().cloned())
            .ok_or_else(|| anyhow!("Superset returned no result block for chart {chart_id}"))?;
        let records = first
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let preferred = first
            .get("colnames")
            .and_then(Value::as_array)
            .map(|names| {
                names
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            });
        Ok(table_from_records(&records, preferred))
    }

    /// Superset wraps filters in `native_filters` / `form_data`; the documented and
    /// stable way to carry them in a link is `form_data` as a JSON blob.
    fn filter_query(filters: &[Filter]) -> String {
        if filters.is_empty() {
            return String::new();
        }
        let adhoc: Vec<Value> = filters
            .iter()
            .map(|filter| {
                json!({
                    "expressionType": "SIMPLE",
                    "subject": filter.column,
                    "operator": filter.op.to_uppercase(),
                    "comparator": filter.value,
                    "clause": "WHERE",
                })
            })
            .collect();
        let form_data = json!({ "adhoc_filters": adhoc }).to_string();
        format!("?form_data={}", urlencoding::encode(&form_data))
    }
}

#[async_trait]
impl BiBackend for Superset {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            backend: "superset".into(),
            open_source: true,
            list_dashboards: true,
            chart_data: true,
            raw_query: true,
            drill_down: true,
            export_image: true,
            deep_links: true,
            notes: vec![
                "Thumbnails require Superset's THUMBNAILS feature flag and a Celery worker; \
                 without them export_image returns the platform's error rather than a blank."
                    .into(),
            ],
        }
    }

    async fn list_dashboards(&self) -> Result<Vec<Dashboard>> {
        let url = format!("{}/api/v1/dashboard/?q=(page_size:100)", self.base);
        let body = self.http.get_json(&url, &self.auth().await?).await?;
        let result = body
            .get("result")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(result
            .iter()
            .map(|item| Dashboard {
                id: item.get("id").map(|id| id.to_string()).unwrap_or_default(),
                title: item
                    .get("dashboard_title")
                    .and_then(Value::as_str)
                    .unwrap_or("(untitled)")
                    .to_string(),
                description: None,
                charts: Vec::new(),
                filters: Vec::new(),
                url: item
                    .get("url")
                    .and_then(Value::as_str)
                    .map(|path| format!("{}{path}", self.base)),
                modified_at: item
                    .get("changed_on_utc")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
            .collect())
    }

    async fn get_dashboard(&self, id: &str) -> Result<Dashboard> {
        let detail = self
            .http
            .get_json(
                &format!("{}/api/v1/dashboard/{id}", self.base),
                &self.auth().await?,
            )
            .await?;
        let result = detail.get("result").unwrap_or(&detail);
        // Charts come from a separate endpoint, which is also where the dataset and
        // the groupable columns live.
        let charts_body = self
            .http
            .get_json(
                &format!("{}/api/v1/dashboard/{id}/charts", self.base),
                &self.auth().await?,
            )
            .await
            .unwrap_or(Value::Null);
        let charts = charts_body
            .get("result")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|chart| {
                let form = chart.get("form_data").unwrap_or(&Value::Null);
                ChartRef {
                    id: chart
                        .get("id")
                        .or_else(|| form.get("slice_id"))
                        .map(|id| id.to_string())
                        .unwrap_or_default(),
                    title: chart
                        .get("slice_name")
                        .and_then(Value::as_str)
                        .unwrap_or("(untitled)")
                        .to_string(),
                    kind: form
                        .get("viz_type")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_string(),
                    dataset_id: form
                        .get("datasource")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    dimensions: form
                        .get("groupby")
                        .and_then(Value::as_array)
                        .map(|values| {
                            values
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default(),
                    metrics: form
                        .get("metrics")
                        .and_then(Value::as_array)
                        .map(|values| {
                            values
                                .iter()
                                .map(|metric| {
                                    metric
                                        .as_str()
                                        .map(str::to_string)
                                        .or_else(|| {
                                            metric
                                                .get("label")
                                                .and_then(Value::as_str)
                                                .map(str::to_string)
                                        })
                                        .unwrap_or_else(|| metric.to_string())
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                }
            })
            .collect();
        Ok(Dashboard {
            id: id.to_string(),
            title: result
                .get("dashboard_title")
                .and_then(Value::as_str)
                .unwrap_or("(untitled)")
                .to_string(),
            description: result
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string),
            charts,
            filters: Vec::new(),
            url: Some(format!("{}/superset/dashboard/{id}/", self.base)),
            modified_at: result
                .get("changed_on")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }

    async fn list_datasets(&self) -> Result<Vec<Dataset>> {
        let body = self
            .http
            .get_json(
                &format!("{}/api/v1/dataset/?q=(page_size:100)", self.base),
                &self.auth().await?,
            )
            .await?;
        Ok(body
            .get("result")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|item| Dataset {
                id: item.get("id").map(|id| id.to_string()).unwrap_or_default(),
                name: item
                    .get("table_name")
                    .and_then(Value::as_str)
                    .unwrap_or("(unnamed)")
                    .to_string(),
                schema: item
                    .get("schema")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                columns: Vec::new(),
                row_count: None,
            })
            .collect())
    }

    async fn describe_dataset(&self, id: &str) -> Result<Dataset> {
        let body = self
            .http
            .get_json(
                &format!("{}/api/v1/dataset/{id}", self.base),
                &self.auth().await?,
            )
            .await?;
        let result = body.get("result").unwrap_or(&body);
        Ok(Dataset {
            id: id.to_string(),
            name: result
                .get("table_name")
                .and_then(Value::as_str)
                .unwrap_or("(unnamed)")
                .to_string(),
            schema: result
                .get("schema")
                .and_then(Value::as_str)
                .map(str::to_string),
            columns: result
                .get("columns")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|column| {
                    let kind = column
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("string")
                        .to_lowercase();
                    Column {
                        name: column
                            .get("column_name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        groupable: column
                            .get("groupby")
                            .and_then(Value::as_bool)
                            .unwrap_or(!kind.contains("int") && !kind.contains("float")),
                        kind: if kind.contains("int")
                            || kind.contains("float")
                            || kind.contains("numeric")
                        {
                            "number".into()
                        } else if kind.contains("time") || kind.contains("date") {
                            "time".into()
                        } else if kind.contains("bool") {
                            "bool".into()
                        } else {
                            "string".into()
                        },
                    }
                })
                .collect(),
            row_count: None,
        })
    }

    async fn chart_data(&self, chart_id: &str, filters: &[Filter]) -> Result<Table> {
        let table = self.run_chart_query(chart_id, None, filters, 1000).await?;
        Ok(table)
    }

    async fn drill_down(
        &self,
        chart_id: &str,
        dimension: Option<&str>,
        filters: &[Filter],
    ) -> Result<DrillResult> {
        let before = self
            .run_chart_query(chart_id, None, &[], 1000)
            .await
            .map(|table| table.rows.len())
            .unwrap_or(0);
        let table = self
            .run_chart_query(chart_id, dimension, filters, 1000)
            .await?;
        Ok(DrillResult {
            chart_id: chart_id.to_string(),
            dimension: dimension.map(str::to_string),
            filters: filters.to_vec(),
            rows_before: before,
            rows_after: table.rows.len(),
            table,
            url: Some(format!("{}/explore/?slice_id={chart_id}", self.base)),
        })
    }

    async fn query(&self, dataset_id: &str, sql: &str, limit: usize) -> Result<Table> {
        // Superset's SQL Lab endpoint wants a database id, and a dataset knows its
        // own, so resolve it rather than making the caller supply two ids.
        let dataset = self
            .http
            .get_json(
                &format!("{}/api/v1/dataset/{dataset_id}", self.base),
                &self.auth().await?,
            )
            .await?;
        let database_id = dataset
            .get("result")
            .and_then(|result| result.get("database"))
            .and_then(|database| database.get("id"))
            .cloned()
            .ok_or_else(|| anyhow!("could not find the database behind dataset {dataset_id}"))?;
        // SQL Lab is CSRF-protected, unlike the chart data endpoint. Fetching the
        // token also sets the session cookie the check needs, which is why the HTTP
        // client keeps a cookie store.
        let mut headers = self.headers();
        let csrf = self
            .http
            .get_json(
                &format!("{}/api/v1/security/csrf_token/", self.base),
                &self.auth().await?,
            )
            .await
            .ok();
        if let Some(token) = csrf
            .as_ref()
            .and_then(|body| body.get("result"))
            .and_then(Value::as_str)
        {
            headers.push(("X-CSRFToken".into(), token.to_string()));
            headers.push(("Referer".into(), self.base.clone()));
        }
        let body = self
            .http
            .post_json(
                &format!("{}/api/v1/sqllab/execute/", self.base),
                &headers,
                // The schema names this `queryLimit`; `limit` is rejected outright.
                json!({ "database_id": database_id, "sql": sql, "runAsync": false, "queryLimit": limit }),
            )
            .await?;
        let records = body
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let preferred = body
            .get("columns")
            .and_then(Value::as_array)
            .map(|columns| {
                columns
                    .iter()
                    .filter_map(|column| column.get("name").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            });
        let mut table = table_from_records(&records, preferred);
        table.truncated = table.rows.len() >= limit;
        Ok(table)
    }

    async fn export_image(&self, dashboard_id: &str) -> Result<(Vec<u8>, String)> {
        // The digest is required by the route; Superset accepts a placeholder and
        // recomputes, and a wrong digest is answered with a redirect to the right one.
        let url = format!(
            "{}/api/v1/dashboard/{dashboard_id}/thumbnail/current/",
            self.base
        );
        self.http.get_bytes(&url, &self.auth().await?).await
    }

    async fn deep_link(&self, dashboard_id: &str, filters: &[Filter]) -> Result<String> {
        Ok(format!(
            "{}/superset/dashboard/{dashboard_id}/{}",
            self.base,
            Self::filter_query(filters)
        ))
    }
}

// ── Metabase ────────────────────────────────────────────────────────────────

pub struct Metabase {
    http: Arc<dyn Http>,
    base: String,
    token: String,
}

impl Metabase {
    pub fn from_env(http: Arc<dyn Http>) -> Result<Self> {
        Ok(Self {
            http,
            base: base("METABASE_URL", "http://localhost:3000"),
            token: token("METABASE_TOKEN")?,
        })
    }

    pub fn new(http: Arc<dyn Http>, base_url: &str, token: &str) -> Self {
        Self {
            http,
            base: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
        }
    }

    /// Metabase uses its own header rather than `Authorization`.
    fn headers(&self) -> Vec<(String, String)> {
        vec![
            ("X-Metabase-Session".into(), self.token.clone()),
            ("Accept".into(), "application/json".into()),
        ]
    }
}

#[async_trait]
impl BiBackend for Metabase {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            backend: "metabase".into(),
            open_source: true,
            list_dashboards: true,
            chart_data: true,
            raw_query: true,
            drill_down: true,
            export_image: false,
            deep_links: true,
            notes: vec![
                "Metabase renders dashboards to PDF rather than to an image, so export_image \
                 is not offered; open the deep link and capture it instead."
                    .into(),
            ],
        }
    }

    async fn list_dashboards(&self) -> Result<Vec<Dashboard>> {
        let body = self
            .http
            .get_json(&format!("{}/api/dashboard", self.base), &self.headers())
            .await?;
        let items = body.as_array().cloned().unwrap_or_default();
        Ok(items
            .iter()
            .map(|item| Dashboard {
                id: item.get("id").map(|id| id.to_string()).unwrap_or_default(),
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
                    .get("id")
                    .map(|id| format!("{}/dashboard/{id}", self.base)),
                modified_at: item
                    .get("updated_at")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
            .collect())
    }

    async fn get_dashboard(&self, id: &str) -> Result<Dashboard> {
        let body = self
            .http
            .get_json(
                &format!("{}/api/dashboard/{id}", self.base),
                &self.headers(),
            )
            .await?;
        let charts = body
            .get("dashcards")
            .or_else(|| body.get("ordered_cards"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|dashcard| {
                let card = dashcard.get("card")?;
                Some(ChartRef {
                    id: card.get("id").map(|id| id.to_string()).unwrap_or_default(),
                    title: card
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("(untitled)")
                        .to_string(),
                    kind: card
                        .get("display")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .to_string(),
                    dataset_id: card.get("table_id").map(|id| id.to_string()),
                    dimensions: Vec::new(),
                    metrics: Vec::new(),
                })
            })
            .collect();
        Ok(Dashboard {
            id: id.to_string(),
            title: body
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("(untitled)")
                .to_string(),
            description: body
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string),
            charts,
            filters: Vec::new(),
            url: Some(format!("{}/dashboard/{id}", self.base)),
            modified_at: body
                .get("updated_at")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }

    async fn list_datasets(&self) -> Result<Vec<Dataset>> {
        let body = self
            .http
            .get_json(&format!("{}/api/table", self.base), &self.headers())
            .await?;
        Ok(body
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|table| Dataset {
                id: table.get("id").map(|id| id.to_string()).unwrap_or_default(),
                name: table
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("(unnamed)")
                    .to_string(),
                schema: table
                    .get("schema")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                columns: Vec::new(),
                row_count: None,
            })
            .collect())
    }

    async fn describe_dataset(&self, id: &str) -> Result<Dataset> {
        let body = self
            .http
            .get_json(
                &format!("{}/api/table/{id}/query_metadata", self.base),
                &self.headers(),
            )
            .await?;
        Ok(Dataset {
            id: id.to_string(),
            name: body
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("(unnamed)")
                .to_string(),
            schema: body
                .get("schema")
                .and_then(Value::as_str)
                .map(str::to_string),
            columns: body
                .get("fields")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|field| {
                    let base_type = field
                        .get("base_type")
                        .and_then(Value::as_str)
                        .unwrap_or("type/Text");
                    Column {
                        name: field
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        kind: if base_type.contains("Integer")
                            || base_type.contains("Float")
                            || base_type.contains("Decimal")
                        {
                            "number".into()
                        } else if base_type.contains("Date") || base_type.contains("Time") {
                            "time".into()
                        } else if base_type.contains("Boolean") {
                            "bool".into()
                        } else {
                            "string".into()
                        },
                        groupable: !base_type.contains("Float") && !base_type.contains("Decimal"),
                    }
                })
                .collect(),
            row_count: None,
        })
    }

    async fn chart_data(&self, chart_id: &str, _filters: &[Filter]) -> Result<Table> {
        let body = self
            .http
            .post_json(
                &format!("{}/api/card/{chart_id}/query", self.base),
                &self.headers(),
                json!({}),
            )
            .await?;
        let data = body.get("data").unwrap_or(&body);
        let columns: Vec<String> = data
            .get("cols")
            .and_then(Value::as_array)
            .map(|cols| {
                cols.iter()
                    .map(|column| {
                        column
                            .get("display_name")
                            .or_else(|| column.get("name"))
                            .and_then(Value::as_str)
                            .unwrap_or("?")
                            .to_string()
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Metabase returns positional rows, so they map straight across.
        Ok(Table {
            columns,
            rows: data
                .get("rows")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|row| row.as_array().cloned())
                .collect(),
            truncated: false,
        })
    }

    async fn drill_down(
        &self,
        chart_id: &str,
        dimension: Option<&str>,
        filters: &[Filter],
    ) -> Result<DrillResult> {
        // Metabase drill-through needs the card's own query rewritten, which the
        // REST surface does not expose generically. Report that rather than
        // returning the unfiltered table as though it were filtered.
        if dimension.is_some() || !filters.is_empty() {
            return Err(unsupported(
                "metabase",
                "server-side drill-down on a saved card; query the underlying table with bi_query instead",
            ));
        }
        let table = self.chart_data(chart_id, &[]).await?;
        Ok(DrillResult {
            chart_id: chart_id.to_string(),
            dimension: None,
            filters: Vec::new(),
            rows_before: table.rows.len(),
            rows_after: table.rows.len(),
            table,
            url: Some(format!("{}/question/{chart_id}", self.base)),
        })
    }

    async fn query(&self, dataset_id: &str, sql: &str, limit: usize) -> Result<Table> {
        let body = self
            .http
            .post_json(
                &format!("{}/api/dataset", self.base),
                &self.headers(),
                json!({
                    "type": "native",
                    "native": { "query": sql },
                    "database": dataset_id.parse::<i64>().unwrap_or(1),
                }),
            )
            .await?;
        let data = body.get("data").unwrap_or(&body);
        let columns: Vec<String> = data
            .get("cols")
            .and_then(Value::as_array)
            .map(|cols| {
                cols.iter()
                    .map(|column| {
                        column
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("?")
                            .to_string()
                    })
                    .collect()
            })
            .unwrap_or_default();
        let rows: Vec<Vec<Value>> = data
            .get("rows")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|row| row.as_array().cloned())
            .take(limit)
            .collect();
        Ok(Table {
            truncated: rows.len() >= limit,
            columns,
            rows,
        })
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
                format!("{}={}", filter.column, urlencoding::encode(&value))
            })
            .collect::<Vec<_>>()
            .join("&");
        Ok(if query.is_empty() {
            format!("{}/dashboard/{dashboard_id}", self.base)
        } else {
            format!("{}/dashboard/{dashboard_id}?{query}", self.base)
        })
    }
}
