// Licensed under the Apache License, Version 2.0.

//! Entry point: pick a backend, then serve on stdio.
//!
//! The default backend is a seeded fixture, so `cargo run` works with no
//! credentials, no network and no BI platform. Point it at a real one with
//! `BI_BACKEND`.

use adk_mcp_sdk::HealthCheck;
use mcp_bi::backend::{BiBackend, Selection};
use mcp_bi::http::{Http, Reqwest};
use mcp_bi::memory::MemoryBackend;
use mcp_bi::open_source::{Metabase, Superset};
use mcp_bi::server::BiServer;
use mcp_bi::vendors::{Looker, PowerBi, Qlik, QuickSight, Tableau};
use rmcp::{ServiceExt, transport::stdio};
use std::sync::Arc;

/// Find `mcp-server.toml`: beside the executable first, then the working directory.
fn find_manifest() -> Option<adk_mcp_sdk::ServerManifest> {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("mcp-server.toml")));
    // A cargo build puts the binary three levels below the crate root, so check
    // there as well — that is where the file lives during development.
    let crate_root = std::env::current_exe().ok().and_then(|exe| {
        exe.ancestors()
            .nth(3)
            .map(|dir| dir.join("mcp-server.toml"))
    });
    [
        beside,
        crate_root,
        Some(std::path::PathBuf::from("mcp-server.toml")),
    ]
    .into_iter()
    .flatten()
    .find(|path| path.exists())
    .and_then(|path| adk_mcp_sdk::ServerManifest::from_file(&path).ok())
}

/// Build the selected backend, or explain what is missing.
fn connect(selection: Selection, http: Arc<dyn Http>) -> anyhow::Result<Arc<dyn BiBackend>> {
    Ok(match selection {
        Selection::Memory => Arc::new(MemoryBackend::new()),
        Selection::Superset => Arc::new(Superset::from_env(http)?),
        Selection::Metabase => Arc::new(Metabase::from_env(http)?),
        Selection::PowerBi => Arc::new(PowerBi::from_env(http)?),
        Selection::Tableau => Arc::new(Tableau::from_env(http)?),
        Selection::Looker => Arc::new(Looker::from_env(http)?),
        Selection::Qlik => Arc::new(Qlik::from_env(http)?),
        Selection::QuickSight => Arc::new(QuickSight::from_env(http)?),
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // stdout carries the JSON-RPC stream on a stdio server, so a single log line
    // written there corrupts the protocol. Logs go to stderr, always.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?),
        )
        .init();

    // An MCP server is normally spawned as a child process from somewhere else, so
    // the manifest cannot be assumed to sit in the working directory. Look beside
    // the executable too, and treat a missing one as "not registry-managed" rather
    // than fatal: refusing to start would make the server unusable as a child.
    let manifest = find_manifest();
    let (display_name, version) = match &manifest {
        Some(manifest) => {
            let errors = manifest.validate();
            if !errors.is_empty() {
                for error in &errors {
                    tracing::error!("manifest: {error}");
                }
                anyhow::bail!("invalid mcp-server.toml ({} error(s))", errors.len());
            }
            (manifest.display_name.clone(), manifest.version.clone())
        }
        None => {
            tracing::debug!("no mcp-server.toml found; starting without registry metadata");
            (
                "Business Intelligence".to_string(),
                env!("CARGO_PKG_VERSION").to_string(),
            )
        }
    };

    let selection = Selection::from_env()?;
    let backend = connect(selection, Arc::new(Reqwest::new()))?;
    let capabilities = backend.capabilities();
    tracing::info!(
        backend = selection.name(),
        open_source = selection.open_source(),
        "{} v{} starting on stdio",
        display_name,
        version
    );
    if selection == Selection::Memory {
        tracing::info!(
            "using the seeded fixture backend — set BI_BACKEND=superset for a real platform"
        );
    }
    for note in &capabilities.notes {
        tracing::info!("backend note: {note}");
    }

    let server = BiServer::new(backend, selection);
    let health = server.check_health().await;
    if !health.healthy {
        tracing::error!(message = ?health.message, "health check failed");
        std::process::exit(1);
    }

    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
