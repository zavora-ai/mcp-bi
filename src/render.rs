// Licensed under the Apache License, Version 2.0.

//! Drawing charts, and computing the things worth saying about them.
//!
//! Two halves of the same idea. `render` turns numbers into a picture with
//! `charts-rs` — server-side, no browser, no JavaScript, no headless Chrome — so
//! an agent can show a person what it is talking about. `insight` computes the
//! statistics *about* those numbers, so what the agent says can cite a figure
//! instead of describing a slope it thinks it saw in an image.
//!
//! The division matters. A model handed only a chart image will report a trend
//! confidently and occasionally invent one. Handed the image *and* the statistics,
//! with instructions to cite, it stops guessing.

use crate::types::{Image, Table};
use anyhow::{Result, anyhow};
use base64::Engine;
use charts_rs::{
    BarChart, HorizontalBarChart, LineChart, PieChart, Series, TableChart, svg_to_png,
};
use serde::{Deserialize, Serialize};

/// Themes `charts-rs` ships. `grafana` and `dark` suit a trading desk.
const THEMES: [&str; 10] = [
    "light", "dark", "grafana", "ant", "vintage", "walden", "westeros", "chalk", "shine", "shadcn",
];

/// Draw a table as a chart.
///
/// The first column is the axis, every numeric column after it a series, which is
/// the shape `chart_data` already returns — so a chart is one call away from the
/// numbers behind a dashboard tile.
pub fn render(
    table: &Table,
    kind: &str,
    title: &str,
    theme: &str,
    width: Option<f32>,
    height: Option<f32>,
) -> Result<Image> {
    if table.columns.len() < 2 {
        return Err(anyhow!(
            "a chart needs a label column and at least one numeric column; this table has {}",
            table.columns.len()
        ));
    }
    if table.rows.is_empty() {
        return Err(anyhow!("nothing to draw: the table has no rows"));
    }
    let theme = if THEMES.contains(&theme) {
        theme
    } else {
        return Err(anyhow!(
            "unknown theme \"{theme}\". Available: {}",
            THEMES.join(", ")
        ));
    };

    let labels = table
        .text_column(&table.columns[0])
        .ok_or_else(|| anyhow!("could not read the label column"))?;
    let mut series_list = Vec::new();
    for name in table.columns.iter().skip(1) {
        if let Some(values) = table.numeric_column(name) {
            // A gap stays a gap: charts-rs draws nullable series with a break
            // rather than dropping to zero, which would invent a crash.
            let data: Vec<Option<f32>> = values
                .into_iter()
                .map(|value| value.map(|number| number as f32))
                .collect();
            if data.iter().any(Option::is_some) {
                series_list.push(Series::new_nullable(name.clone(), data));
            }
        }
    }
    if series_list.is_empty() {
        return Err(anyhow!(
            "no numeric column to plot; columns were: {}",
            table.columns.join(", ")
        ));
    }

    let svg = match kind {
        "line" | "area" => {
            let mut chart = LineChart::new_with_theme(series_list, labels, theme);
            chart.title_text = title.to_string();
            if let Some(width) = width {
                chart.width = width;
            }
            if let Some(height) = height {
                chart.height = height;
            }
            chart.svg()?
        }
        "bar" | "column" => {
            let mut chart = BarChart::new_with_theme(series_list, labels, theme);
            chart.title_text = title.to_string();
            if let Some(width) = width {
                chart.width = width;
            }
            if let Some(height) = height {
                chart.height = height;
            }
            chart.svg()?
        }
        "hbar" | "horizontal_bar" => {
            let mut chart = HorizontalBarChart::new_with_theme(series_list, labels, theme);
            chart.title_text = title.to_string();
            if let Some(width) = width {
                chart.width = width;
            }
            chart.svg()?
        }
        "pie" => {
            // A pie is one slice per label, so the first series carries the values.
            let mut chart = PieChart::new_with_theme(series_list, theme);
            chart.title_text = title.to_string();
            chart.svg()?
        }
        "table" => {
            let mut header = vec![table.columns.clone()];
            header.extend(table.rows.iter().map(|row| {
                row.iter()
                    .map(|cell| match cell {
                        serde_json::Value::String(text) => text.clone(),
                        other => other.to_string(),
                    })
                    .collect::<Vec<_>>()
            }));
            let mut chart = TableChart::new_with_theme(header, theme);
            chart.title_text = title.to_string();
            chart.svg()?
        }
        other => {
            return Err(anyhow!(
                "unknown chart kind \"{other}\". Use line, bar, hbar, pie or table."
            ));
        }
    };

    let png = svg_to_png(&svg)?;
    Ok(Image {
        bytes: png.len(),
        data: base64::engine::general_purpose::STANDARD.encode(&png),
        mime_type: "image/png".into(),
        source: "charts-rs".into(),
    })
}

/// What is true about a series, computed rather than eyeballed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Insight {
    pub column: String,
    pub points: usize,
    pub first: f64,
    pub last: f64,
    /// Change from first to last, as a percentage of the first.
    pub change_pct: f64,
    pub min: f64,
    pub min_at: String,
    pub max: f64,
    pub max_at: String,
    pub mean: f64,
    /// Sample standard deviation, so a single-point series has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub std_dev: Option<f64>,
    /// Sign of the least-squares slope: `up`, `down`, or `flat`.
    pub direction: String,
    /// Points more than three standard deviations from the mean, with labels.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outliers: Vec<Outlier>,
    /// Gaps, which a chart hides but an average should not.
    #[serde(default)]
    pub missing: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outlier {
    pub at: String,
    pub value: f64,
    /// How many standard deviations from the mean.
    pub z: f64,
}

/// Compute statistics for every numeric column in a table.
pub fn insights(table: &Table) -> Vec<Insight> {
    let labels = table
        .text_column(&table.columns[0])
        .unwrap_or_else(|| (0..table.rows.len()).map(|i| i.to_string()).collect());
    table
        .columns
        .iter()
        .skip(1)
        .filter_map(|name| {
            let raw = table.numeric_column(name)?;
            let missing = raw.iter().filter(|value| value.is_none()).count();
            let present: Vec<(usize, f64)> = raw
                .iter()
                .enumerate()
                .filter_map(|(index, value)| value.map(|number| (index, number)))
                .collect();
            if present.is_empty() {
                return None;
            }
            let values: Vec<f64> = present.iter().map(|(_, value)| *value).collect();
            let count = values.len() as f64;
            let mean = values.iter().sum::<f64>() / count;
            let std_dev = if values.len() > 1 {
                let variance = values
                    .iter()
                    .map(|value| (value - mean).powi(2))
                    .sum::<f64>()
                    / (count - 1.0);
                Some(variance.sqrt())
            } else {
                None
            };
            let (min_index, min) =
                present
                    .iter()
                    .fold((0usize, f64::MAX), |acc, (index, value)| {
                        if *value < acc.1 {
                            (*index, *value)
                        } else {
                            acc
                        }
                    });
            let (max_index, max) =
                present
                    .iter()
                    .fold((0usize, f64::MIN), |acc, (index, value)| {
                        if *value > acc.1 {
                            (*index, *value)
                        } else {
                            acc
                        }
                    });
            let first = values[0];
            let last = *values.last().expect("non-empty");
            // Least squares over the index, which is enough to name a direction.
            let x_mean = present.iter().map(|(index, _)| *index as f64).sum::<f64>() / count;
            let numerator: f64 = present
                .iter()
                .map(|(index, value)| (*index as f64 - x_mean) * (value - mean))
                .sum();
            let denominator: f64 = present
                .iter()
                .map(|(index, _)| (*index as f64 - x_mean).powi(2))
                .sum();
            let slope = if denominator == 0.0 {
                0.0
            } else {
                numerator / denominator
            };
            // "Flat" is relative to the series' own scale, not an absolute epsilon.
            let flat_threshold = mean.abs() * 0.0005;
            let direction = if slope > flat_threshold {
                "up"
            } else if slope < -flat_threshold {
                "down"
            } else {
                "flat"
            };
            let outliers = match std_dev {
                Some(deviation) if deviation > 0.0 => present
                    .iter()
                    .filter_map(|(index, value)| {
                        let z = (value - mean) / deviation;
                        (z.abs() > 3.0).then(|| Outlier {
                            at: labels.get(*index).cloned().unwrap_or_default(),
                            value: *value,
                            z: round(z, 2),
                        })
                    })
                    .collect(),
                _ => Vec::new(),
            };
            Some(Insight {
                column: name.clone(),
                points: values.len(),
                first: round(first, 4),
                last: round(last, 4),
                change_pct: if first == 0.0 {
                    0.0
                } else {
                    round((last - first) / first.abs() * 100.0, 2)
                },
                min: round(min, 4),
                min_at: labels.get(min_index).cloned().unwrap_or_default(),
                max: round(max, 4),
                max_at: labels.get(max_index).cloned().unwrap_or_default(),
                mean: round(mean, 4),
                std_dev: std_dev.map(|value| round(value, 4)),
                direction: direction.into(),
                outliers,
                missing,
            })
        })
        .collect()
}

fn round(value: f64, places: u32) -> f64 {
    let factor = 10f64.powi(places as i32);
    (value * factor).round() / factor
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn table(labels: &[&str], name: &str, values: &[Option<f64>]) -> Table {
        Table {
            columns: vec!["day".into(), name.into()],
            rows: labels
                .iter()
                .zip(values)
                .map(|(label, value)| {
                    vec![json!(label), value.map(|v| json!(v)).unwrap_or(json!(null))]
                })
                .collect(),
            truncated: false,
        }
    }

    #[test]
    fn a_rising_series_is_described_as_rising() {
        let computed = insights(&table(
            &["d1", "d2", "d3", "d4"],
            "price",
            &[Some(100.0), Some(104.0), Some(108.0), Some(112.0)],
        ));
        let insight = &computed[0];
        assert_eq!(insight.direction, "up");
        assert_eq!(insight.change_pct, 12.0);
        assert_eq!(insight.min_at, "d1");
        assert_eq!(insight.max_at, "d4");
        assert_eq!(insight.points, 4);
    }

    #[test]
    fn a_flat_series_is_not_called_a_trend() {
        // Tiny wobble around a large mean must not read as direction.
        let computed = insights(&table(
            &["a", "b", "c"],
            "volume",
            &[Some(10_000.0), Some(10_001.0), Some(10_000.5)],
        ));
        assert_eq!(computed[0].direction, "flat");
    }

    #[test]
    fn gaps_are_counted_rather_than_averaged_away() {
        let computed = insights(&table(
            &["a", "b", "c", "d"],
            "pnl",
            &[Some(10.0), None, Some(20.0), None],
        ));
        let insight = &computed[0];
        assert_eq!(insight.missing, 2, "the gaps are reported");
        assert_eq!(insight.points, 2, "and excluded from the statistics");
        assert_eq!(insight.mean, 15.0, "not treated as zero");
    }

    #[test]
    fn an_outlier_is_named_with_its_label() {
        let mut values: Vec<Option<f64>> = (0..30).map(|_| Some(100.0)).collect();
        values.push(Some(400.0));
        let labels: Vec<String> = (0..31).map(|index| format!("d{index}")).collect();
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let computed = insights(&table(&refs, "price", &values));
        let outliers = &computed[0].outliers;
        assert_eq!(outliers.len(), 1);
        assert_eq!(outliers[0].at, "d30");
        assert_eq!(outliers[0].value, 400.0);
    }

    #[test]
    fn rendering_produces_a_real_png() {
        let data = table(
            &["a", "b", "c"],
            "price",
            &[Some(1.0), Some(3.0), Some(2.0)],
        );
        let image = render(&data, "line", "Price", "grafana", None, None).unwrap();
        assert_eq!(image.mime_type, "image/png");
        assert_eq!(image.source, "charts-rs");
        assert!(image.bytes > 1_000, "a real chart, not an empty canvas");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&image.data)
            .unwrap();
        // PNG magic, so this is genuinely an image and not text that claims to be.
        assert_eq!(
            &decoded[..8],
            &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]
        );
    }

    #[test]
    fn rendering_refuses_what_it_cannot_draw() {
        let empty = Table {
            columns: vec!["day".into(), "x".into()],
            rows: vec![],
            truncated: false,
        };
        assert!(
            render(&empty, "line", "t", "light", None, None)
                .unwrap_err()
                .to_string()
                .contains("no rows")
        );

        let data = table(&["a"], "price", &[Some(1.0)]);
        assert!(
            render(&data, "sankey", "t", "light", None, None)
                .unwrap_err()
                .to_string()
                .contains("unknown chart kind")
        );
        assert!(
            render(&data, "line", "t", "neon", None, None)
                .unwrap_err()
                .to_string()
                .contains("unknown theme")
        );
    }
}
