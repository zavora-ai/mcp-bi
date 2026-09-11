// Licensed under the Apache License, Version 2.0.

//! One trait, seven platforms.
//!
//! Selected with `BI_BACKEND`, defaulting to `memory` — a deterministic seeded
//! platform that needs no credentials and no network. That default is what makes
//! the server useful in tests, in CI and in a demo on a plane, and it follows the
//! same convention as `mcp-market-data` in this workspace.
//!
//! | `BI_BACKEND` | Platform | Open source |
//! |---|---|---|
//! | `memory` (default) | Seeded fixture | — |
//! | `superset` | Apache Superset | yes |
//! | `metabase` | Metabase | yes |
//! | `powerbi` | Microsoft Power BI | no |
//! | `tableau` | Salesforce Tableau | no |
//! | `looker` | Google Looker | no |
//! | `qlik` | Qlik Sense | no |
//! | `quicksight` | Amazon QuickSight | no |

use crate::types::{Capabilities, Dashboard, Dataset, DrillResult, Filter, Table};
use anyhow::{Result, anyhow};
use async_trait::async_trait;

/// What every BI platform must answer.
///
/// Deliberately read-mostly. Creating and publishing dashboards is a governed
/// write that belongs with the platform's own review process; this server exists
/// so an agent can *understand* what a business already measures.
#[async_trait]
pub trait BiBackend: Send + Sync {
    fn capabilities(&self) -> Capabilities;

    async fn list_dashboards(&self) -> Result<Vec<Dashboard>>;

    async fn get_dashboard(&self, id: &str) -> Result<Dashboard>;

    async fn list_datasets(&self) -> Result<Vec<Dataset>>;

    async fn describe_dataset(&self, id: &str) -> Result<Dataset>;

    /// The rows behind one chart, with the dashboard's filters applied.
    async fn chart_data(&self, chart_id: &str, filters: &[Filter]) -> Result<Table>;

    /// Narrow a chart by filters, optionally breaking it down by a dimension.
    async fn drill_down(
        &self,
        chart_id: &str,
        dimension: Option<&str>,
        filters: &[Filter],
    ) -> Result<DrillResult>;

    /// Arbitrary SQL, where the platform allows it.
    async fn query(&self, dataset_id: &str, sql: &str, limit: usize) -> Result<Table> {
        let _ = (dataset_id, sql, limit);
        Err(unsupported(&self.capabilities().backend, "raw SQL queries"))
    }

    /// A server-rendered image of a dashboard, where the platform offers one.
    ///
    /// Not every platform does. When it does not, the answer is an error saying so
    /// — a screenshot taken by driving a browser is the caller's decision to make,
    /// not something to substitute in silently.
    async fn export_image(&self, dashboard_id: &str) -> Result<(Vec<u8>, String)> {
        let _ = dashboard_id;
        Err(unsupported(
            &self.capabilities().backend,
            "server-side dashboard rendering",
        ))
    }

    /// A URL that opens the same view, with filters applied where supported.
    async fn deep_link(&self, dashboard_id: &str, filters: &[Filter]) -> Result<String> {
        let _ = (dashboard_id, filters);
        Err(unsupported(&self.capabilities().backend, "deep links"))
    }
}

/// A missing capability, phrased so the caller knows what to do instead.
pub fn unsupported(backend: &str, what: &str) -> anyhow::Error {
    anyhow!(
        "the {backend} backend does not support {what}. \
         Check bi_backend_info for what it does support; \
         a dashboard with no server-side rendering can still be opened in a browser \
         with bi_dashboard_url and read with a desktop screenshot."
    )
}

/// Which platform to talk to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    Memory,
    Superset,
    Metabase,
    PowerBi,
    Tableau,
    Looker,
    Qlik,
    QuickSight,
}

impl Selection {
    /// Read `BI_BACKEND`, defaulting to the offline fixture.
    pub fn from_env() -> Result<Self> {
        let raw = std::env::var("BI_BACKEND").unwrap_or_else(|_| "memory".to_string());
        Self::parse(&raw)
    }

    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_lowercase().as_str() {
            "" | "memory" | "offline" | "fixture" => Ok(Self::Memory),
            "superset" | "apache-superset" => Ok(Self::Superset),
            "metabase" => Ok(Self::Metabase),
            "powerbi" | "power-bi" | "power_bi" => Ok(Self::PowerBi),
            "tableau" => Ok(Self::Tableau),
            "looker" => Ok(Self::Looker),
            "qlik" | "qliksense" | "qlik-sense" => Ok(Self::Qlik),
            "quicksight" | "aws-quicksight" => Ok(Self::QuickSight),
            other => Err(anyhow!(
                "unknown BI_BACKEND \"{other}\". Use memory, superset, metabase, \
                 powerbi, tableau, looker, qlik or quicksight."
            )),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Superset => "superset",
            Self::Metabase => "metabase",
            Self::PowerBi => "powerbi",
            Self::Tableau => "tableau",
            Self::Looker => "looker",
            Self::Qlik => "qlik",
            Self::QuickSight => "quicksight",
        }
    }

    pub fn open_source(self) -> bool {
        matches!(self, Self::Memory | Self::Superset | Self::Metabase)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_names_are_forgiving_about_spelling() {
        assert_eq!(Selection::parse("superset").unwrap(), Selection::Superset);
        assert_eq!(Selection::parse("Power-BI").unwrap(), Selection::PowerBi);
        assert_eq!(Selection::parse("  qlik-sense ").unwrap(), Selection::Qlik);
        // The default is the offline fixture, so the server starts with no setup.
        assert_eq!(Selection::parse("").unwrap(), Selection::Memory);
    }

    #[test]
    fn an_unknown_backend_lists_the_real_ones() {
        let error = Selection::parse("cognos").unwrap_err().to_string();
        assert!(
            error.contains("cognos"),
            "names what was asked for: {error}"
        );
        assert!(error.contains("superset"), "and what is available: {error}");
    }

    #[test]
    fn only_the_open_platforms_claim_to_be_open() {
        assert!(Selection::Superset.open_source());
        assert!(Selection::Metabase.open_source());
        for closed in [
            Selection::PowerBi,
            Selection::Tableau,
            Selection::Looker,
            Selection::Qlik,
            Selection::QuickSight,
        ] {
            assert!(
                !closed.open_source(),
                "{} is not open source",
                closed.name()
            );
        }
    }

    #[test]
    fn a_missing_capability_suggests_the_way_round_it() {
        let error = unsupported("quicksight", "raw SQL queries").to_string();
        assert!(error.contains("quicksight"));
        assert!(error.contains("bi_backend_info"));
        assert!(
            error.contains("bi_dashboard_url"),
            "points at the workaround"
        );
    }
}
