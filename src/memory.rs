// Licensed under the Apache License, Version 2.0.

//! A seeded BI platform, for when there is no BI platform.
//!
//! Three saved dashboards over one trading dataset, generated from a fixed seed
//! so every run produces identical numbers. That matters for more than tests: a
//! decision an agent reaches here can be re-derived tomorrow and compared, which
//! is the property that makes a finance demo checkable rather than anecdotal.
//!
//! Nothing here pretends to be market truth. It is shaped like a trading desk's
//! data so the drill-down paths are realistic, and `bi_backend_info` says plainly
//! that it is a fixture.

use crate::backend::BiBackend;
use crate::types::{
    Capabilities, ChartRef, Column, Dashboard, Dataset, DrillResult, Filter, Table,
};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};

/// Running totals per bucket: price, volume, pnl, slippage, and the row count
/// that turns the first and last into averages.
type Sums = (f64, f64, f64, f64, usize);

const TICKERS: [(&str, &str, &str); 6] = [
    ("AAPL", "Technology", "US"),
    ("MSFT", "Technology", "US"),
    ("JPM", "Financials", "US"),
    ("SHEL", "Energy", "EU"),
    ("TSM", "Technology", "APAC"),
    ("NESN", "Consumer", "EU"),
];
const VENUES: [&str; 3] = ["NYSE", "NASDAQ", "LSE"];
const DAYS: usize = 30;

/// A deterministic generator. Small on purpose: the point is reproducibility, not
/// statistical quality, and a named algorithm beats an opaque dependency here.
fn seeded(seed: u64) -> impl FnMut() -> f64 {
    let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    move || {
        // SplitMix64: short, well-known, and identical on every platform.
        state = state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^= z >> 31;
        // Into [0, 1).
        (z >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// One row per ticker per day, plus a venue rotation.
struct Row {
    day: usize,
    ticker: &'static str,
    sector: &'static str,
    region: &'static str,
    venue: &'static str,
    price: f64,
    volume: f64,
    pnl: f64,
    slippage_bps: f64,
}

fn rows() -> Vec<Row> {
    let mut next = seeded(20260911);
    let mut out = Vec::with_capacity(DAYS * TICKERS.len());
    // A base price per ticker, then a random walk, so a chart has a trend to read
    // rather than noise around a constant.
    let mut prices: Vec<f64> = TICKERS.iter().map(|_| 90.0 + next() * 120.0).collect();
    for day in 0..DAYS {
        for (index, (ticker, sector, region)) in TICKERS.iter().enumerate() {
            let drift = (next() - 0.48) * 0.035;
            prices[index] = (prices[index] * (1.0 + drift)).max(1.0);
            out.push(Row {
                day,
                ticker,
                sector,
                region,
                venue: VENUES[(day + index) % VENUES.len()],
                price: (prices[index] * 100.0).round() / 100.0,
                volume: (1_000.0 + next() * 9_000.0).round(),
                pnl: ((next() - 0.45) * 25_000.0 * 100.0).round() / 100.0,
                slippage_bps: ((next() * 6.0) * 100.0).round() / 100.0,
            });
        }
    }
    out
}

fn day_label(day: usize) -> String {
    // A fixed window, so labels do not drift with the wall clock.
    let start = chrono::NaiveDate::from_ymd_opt(2026, 8, 13).expect("valid date");
    (start + chrono::Duration::days(day as i64))
        .format("%Y-%m-%d")
        .to_string()
}

pub struct MemoryBackend {
    rows: Vec<Row>,
}

impl Default for MemoryBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryBackend {
    pub fn new() -> Self {
        Self { rows: rows() }
    }

    fn dashboards(&self) -> Vec<Dashboard> {
        vec![
            Dashboard {
                id: "market-overview".into(),
                title: "Market Overview".into(),
                description: Some("Prices, traded volume and the day's movers.".into()),
                charts: vec![
                    ChartRef {
                        id: "price-by-day".into(),
                        title: "Close price by day".into(),
                        kind: "line".into(),
                        dataset_id: Some("trades".into()),
                        dimensions: vec!["ticker".into(), "sector".into(), "region".into()],
                        metrics: vec!["price".into()],
                    },
                    ChartRef {
                        id: "volume-by-venue".into(),
                        title: "Volume by venue".into(),
                        kind: "bar".into(),
                        dataset_id: Some("trades".into()),
                        dimensions: vec!["venue".into(), "region".into()],
                        metrics: vec!["volume".into()],
                    },
                    ChartRef {
                        id: "top-movers".into(),
                        title: "Top movers".into(),
                        kind: "table".into(),
                        dataset_id: Some("trades".into()),
                        dimensions: vec!["ticker".into()],
                        metrics: vec!["change_pct".into()],
                    },
                ],
                filters: vec![],
                url: Some("bi://memory/dashboard/market-overview".into()),
                modified_at: Some("2026-09-11T06:00:00Z".into()),
            },
            Dashboard {
                id: "portfolio-risk".into(),
                title: "Portfolio Risk".into(),
                description: Some("Exposure and drawdown by sector and region.".into()),
                charts: vec![
                    ChartRef {
                        id: "exposure-by-sector".into(),
                        title: "Exposure by sector".into(),
                        kind: "pie".into(),
                        dataset_id: Some("trades".into()),
                        dimensions: vec!["sector".into(), "region".into()],
                        metrics: vec!["exposure".into()],
                    },
                    ChartRef {
                        id: "pnl-by-day".into(),
                        title: "Daily P&L".into(),
                        kind: "line".into(),
                        dataset_id: Some("trades".into()),
                        dimensions: vec!["sector".into(), "ticker".into()],
                        metrics: vec!["pnl".into()],
                    },
                ],
                filters: vec![],
                url: Some("bi://memory/dashboard/portfolio-risk".into()),
                modified_at: Some("2026-09-11T06:00:00Z".into()),
            },
            Dashboard {
                id: "execution-quality".into(),
                title: "Execution Quality".into(),
                description: Some("Slippage by venue, which is where cost hides.".into()),
                charts: vec![ChartRef {
                    id: "slippage-by-venue".into(),
                    title: "Average slippage (bps) by venue".into(),
                    kind: "bar".into(),
                    dataset_id: Some("trades".into()),
                    dimensions: vec!["venue".into(), "ticker".into(), "region".into()],
                    metrics: vec!["slippage_bps".into()],
                }],
                filters: vec![],
                url: Some("bi://memory/dashboard/execution-quality".into()),
                modified_at: Some("2026-09-11T06:00:00Z".into()),
            },
        ]
    }

    /// Does a row survive the filters? Only the operators a dashboard exposes.
    fn keeps(&self, row: &Row, filters: &[Filter]) -> bool {
        filters.iter().all(|filter| {
            let actual: Value = match filter.column.as_str() {
                "ticker" => json!(row.ticker),
                "sector" => json!(row.sector),
                "region" => json!(row.region),
                "venue" => json!(row.venue),
                "day" => json!(row.day),
                _ => return true, // an unknown column filters nothing, and says so via notes
            };
            match filter.op.as_str() {
                "=" => actual == filter.value,
                "!=" => actual != filter.value,
                "in" => filter
                    .value
                    .as_array()
                    .map(|values| values.contains(&actual))
                    .unwrap_or(false),
                ">" => compare(&actual, &filter.value)
                    .map(|o| o.is_gt())
                    .unwrap_or(false),
                "<" => compare(&actual, &filter.value)
                    .map(|o| o.is_lt())
                    .unwrap_or(false),
                ">=" => compare(&actual, &filter.value)
                    .map(|o| o.is_ge())
                    .unwrap_or(false),
                "<=" => compare(&actual, &filter.value)
                    .map(|o| o.is_le())
                    .unwrap_or(false),
                _ => true,
            }
        })
    }

    /// Aggregate the fixture the way the named chart does.
    fn aggregate(&self, chart_id: &str, group_by: &str, filters: &[Filter]) -> Result<Table> {
        let kept: Vec<&Row> = self
            .rows
            .iter()
            .filter(|row| self.keeps(row, filters))
            .collect();
        let key = |row: &Row| -> String {
            match group_by {
                "ticker" => row.ticker.to_string(),
                "sector" => row.sector.to_string(),
                "region" => row.region.to_string(),
                "venue" => row.venue.to_string(),
                _ => day_label(row.day),
            }
        };
        // Ordered accumulation, so output order is stable run to run.
        let mut order: Vec<String> = Vec::new();
        let mut sums: std::collections::HashMap<String, Sums> = std::collections::HashMap::new();
        for row in &kept {
            let bucket = key(row);
            if !sums.contains_key(&bucket) {
                order.push(bucket.clone());
            }
            let entry = sums.entry(bucket).or_insert((0.0, 0.0, 0.0, 0.0, 0));
            entry.0 += row.price;
            entry.1 += row.volume;
            entry.2 += row.pnl;
            entry.3 += row.slippage_bps;
            entry.4 += 1;
        }
        let (value_column, pick): (&str, fn(&Sums) -> f64) = match chart_id {
            "price-by-day" => ("price", |s| round2(s.0 / s.4 as f64)),
            "volume-by-venue" => ("volume", |s| s.1.round()),
            "pnl-by-day" => ("pnl", |s| round2(s.2)),
            "slippage-by-venue" => ("slippage_bps", |s| round2(s.3 / s.4 as f64)),
            "exposure-by-sector" => ("exposure", |s| round2(s.0 * s.1 / 1_000.0)),
            "top-movers" => ("change_pct", |s| round2((s.2 / 1_000.0) / s.4 as f64)),
            other => return Err(anyhow!("unknown chart \"{other}\" on the memory backend")),
        };
        let label = if group_by.is_empty() { "day" } else { group_by };
        Ok(Table {
            columns: vec![label.to_string(), value_column.to_string()],
            rows: order
                .into_iter()
                .map(|bucket| {
                    let stats = &sums[&bucket];
                    vec![json!(bucket), json!(pick(stats))]
                })
                .collect(),
            truncated: false,
        })
    }

    /// A chart's natural grouping, before any drill-down.
    fn default_group(chart_id: &str) -> &'static str {
        match chart_id {
            "volume-by-venue" | "slippage-by-venue" => "venue",
            "exposure-by-sector" => "sector",
            "top-movers" => "ticker",
            _ => "", // by day
        }
    }
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn compare(left: &Value, right: &Value) -> Option<std::cmp::Ordering> {
    match (left.as_f64(), right.as_f64()) {
        (Some(a), Some(b)) => a.partial_cmp(&b),
        _ => left.as_str()?.partial_cmp(right.as_str()?),
    }
}

#[async_trait]
impl BiBackend for MemoryBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            backend: "memory".into(),
            open_source: false,
            list_dashboards: true,
            chart_data: true,
            raw_query: false,
            drill_down: true,
            export_image: false,
            deep_links: true,
            notes: vec![
                "Seeded fixture, not a real platform: three dashboards over one generated \
                 trading dataset, identical on every run so a result can be re-derived."
                    .into(),
                "No server-side rendering. Use bi_render_chart to draw the numbers, which is \
                 what a real dashboard image would show anyway."
                    .into(),
                "Set BI_BACKEND=superset (or metabase, powerbi, tableau, looker, qlik, \
                 quicksight) to read a real platform."
                    .into(),
            ],
        }
    }

    async fn list_dashboards(&self) -> Result<Vec<Dashboard>> {
        Ok(self.dashboards())
    }

    async fn get_dashboard(&self, id: &str) -> Result<Dashboard> {
        self.dashboards()
            .into_iter()
            .find(|dashboard| dashboard.id == id)
            .ok_or_else(|| {
                anyhow!(
                    "no dashboard \"{id}\". This backend has market-overview, \
                     portfolio-risk and execution-quality."
                )
            })
    }

    async fn list_datasets(&self) -> Result<Vec<Dataset>> {
        Ok(vec![self.describe_dataset("trades").await?])
    }

    async fn describe_dataset(&self, id: &str) -> Result<Dataset> {
        if id != "trades" {
            return Err(anyhow!(
                "no dataset \"{id}\". This backend has one: trades."
            ));
        }
        let column = |name: &str, kind: &str, groupable: bool| Column {
            name: name.into(),
            kind: kind.into(),
            groupable,
        };
        Ok(Dataset {
            id: "trades".into(),
            name: "trades".into(),
            schema: Some("public".into()),
            columns: vec![
                column("day", "time", true),
                column("ticker", "string", true),
                column("sector", "string", true),
                column("region", "string", true),
                column("venue", "string", true),
                column("price", "number", false),
                column("volume", "number", false),
                column("pnl", "number", false),
                column("slippage_bps", "number", false),
            ],
            row_count: Some(self.rows.len() as u64),
        })
    }

    async fn chart_data(&self, chart_id: &str, filters: &[Filter]) -> Result<Table> {
        self.aggregate(chart_id, Self::default_group(chart_id), filters)
    }

    async fn drill_down(
        &self,
        chart_id: &str,
        dimension: Option<&str>,
        filters: &[Filter],
    ) -> Result<DrillResult> {
        let before = self.chart_data(chart_id, &[]).await?.rows.len();
        let group = dimension.unwrap_or_else(|| Self::default_group(chart_id));
        let table = self.aggregate(chart_id, group, filters)?;
        Ok(DrillResult {
            chart_id: chart_id.to_string(),
            dimension: dimension.map(str::to_string),
            filters: filters.to_vec(),
            rows_before: before,
            rows_after: table.rows.len(),
            table,
            url: Some(format!("bi://memory/chart/{chart_id}")),
        })
    }

    async fn deep_link(&self, dashboard_id: &str, filters: &[Filter]) -> Result<String> {
        let query = filters
            .iter()
            .map(|filter| {
                format!(
                    "{}{}{}",
                    filter.column,
                    filter.op,
                    filter
                        .value
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| filter.value.to_string())
                )
            })
            .collect::<Vec<_>>()
            .join("&");
        Ok(if query.is_empty() {
            format!("bi://memory/dashboard/{dashboard_id}")
        } else {
            format!("bi://memory/dashboard/{dashboard_id}?{query}")
        })
    }
}
