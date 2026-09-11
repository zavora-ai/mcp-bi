// Licensed under the Apache License, Version 2.0.

//! The vendor-neutral shape of a BI platform.
//!
//! Every backend — Apache Superset, Metabase, Power BI, Tableau, Looker, Qlik,
//! QuickSight — is mapped onto these types, so an agent writes one set of calls
//! rather than learning seven APIs.
//!
//! Two conventions run through all of it:
//!
//! - **Numbers are authoritative, pixels are for people.** Every visual has a
//!   `data` route that returns the rows behind it. A model that reads a trend off
//!   an image will state it confidently and sometimes wrongly, so an image is for
//!   showing a person and for corroborating a number that was queried.
//! - **A capability that is missing is reported, never simulated.** Backends
//!   differ in what they expose; `Capabilities` says so, and a call into a gap
//!   returns an error naming the backend rather than plausible invented data.

use serde::{Deserialize, Serialize};

/// A saved dashboard: the thing an analyst opens.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Dashboard {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Charts on the dashboard, in reading order.
    pub charts: Vec<ChartRef>,
    /// Filters the dashboard exposes, and their current values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<Filter>,
    /// Where a person would open it, so a browser can be pointed at the same view.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
}

/// A chart as it appears on a dashboard, without its data.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChartRef {
    pub id: String,
    pub title: String,
    /// `line`, `bar`, `pie`, `table`, `number`, `candlestick`, or a vendor name
    /// passed through when it does not map onto one of those.
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dataset_id: Option<String>,
    /// Dimensions this chart can be broken down by — the drill-down surface.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dimensions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metrics: Vec<String>,
}

/// A dashboard or chart filter.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Filter {
    pub column: String,
    /// `=`, `!=`, `in`, `>`, `<`, `>=`, `<=`, `between`, `like`.
    pub op: String,
    pub value: serde_json::Value,
}

/// A queryable table or semantic model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Dataset {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub columns: Vec<Column>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_count: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Column {
    pub name: String,
    /// `number`, `string`, `time`, `bool`.
    pub kind: String,
    /// Whether it is usable as a grouping key. Measures are not.
    #[serde(default)]
    pub groupable: bool,
}

/// Tabular result: the numbers behind a visual, or the answer to a query.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
    /// Set when the backend truncated the result.
    #[serde(default)]
    pub truncated: bool,
}

impl Table {
    /// Column values as numbers, for charting and statistics.
    ///
    /// Non-numeric cells become `None` rather than zero: a gap in a series and a
    /// genuine zero mean different things, and averaging the difference away
    /// silently changes the answer.
    pub fn numeric_column(&self, name: &str) -> Option<Vec<Option<f64>>> {
        let index = self.columns.iter().position(|column| column == name)?;
        Some(
            self.rows
                .iter()
                .map(|row| row.get(index).and_then(|cell| cell.as_f64()))
                .collect(),
        )
    }

    /// Column values as display strings, for axis labels.
    pub fn text_column(&self, name: &str) -> Option<Vec<String>> {
        let index = self.columns.iter().position(|column| column == name)?;
        Some(
            self.rows
                .iter()
                .map(|row| match row.get(index) {
                    Some(serde_json::Value::String(text)) => text.clone(),
                    Some(other) => other.to_string(),
                    None => String::new(),
                })
                .collect(),
        )
    }
}

/// One drill step: what was narrowed, and what the numbers became.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrillResult {
    pub chart_id: String,
    /// The dimension drilled into, when the step was a breakdown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimension: Option<String>,
    pub filters: Vec<Filter>,
    pub table: Table,
    /// Rows before and after, so a step that changed nothing is visible as such.
    pub rows_before: usize,
    pub rows_after: usize,
    /// A deep link to the same narrowed view, for a person or a browser.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// What a backend can actually do.
///
/// Reported rather than assumed: Superset can render a thumbnail, Metabase can
/// run raw SQL, QuickSight will not hand back arbitrary query results. An agent
/// reads this instead of discovering a gap through a failure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Capabilities {
    pub backend: String,
    pub open_source: bool,
    pub list_dashboards: bool,
    pub chart_data: bool,
    /// Arbitrary SQL against a dataset or database.
    pub raw_query: bool,
    pub drill_down: bool,
    /// Server-rendered dashboard image, without driving a browser.
    pub export_image: bool,
    /// Deep links that carry filter state, so a browser can open the same view.
    pub deep_links: bool,
    /// What to do about the gaps.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// A rendered picture plus what produced it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Image {
    /// Base64. Returned to a model as an image block, not as this string.
    pub data: String,
    pub mime_type: String,
    pub bytes: usize,
    /// How it was produced: `charts-rs`, or the backend that rendered it.
    pub source: String,
}
