mod chunker;
mod embed;
mod error;
mod indexer;
mod server;
mod store;

use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::BaseDirs;
use rmcp::ServiceExt;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    // Logging goes to stderr; stdout is the MCP JSON-RPC transport.
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .with_writer(std::io::stderr)
        .with_target(false)
        .compact()
        .init();

    let root = match std::env::args().nth(1) {
        Some(path) => PathBuf::from(path),
        None => std::env::current_dir().context("failed to get current directory")?,
    };
    let root = root
        .canonicalize()
        .with_context(|| format!("directory not found: {}", root.display()))?;

    tracing::info!("claudevil starting for: {}", root.display());

    // Determine platform-appropriate data directory
    let base_dirs = BaseDirs::new().context("could not determine data directory")?;
    let db_path = base_dirs
        .data_dir()
        .join("claudevil")
        .join(dir_name_for(&root));
    tokio::fs::create_dir_all(&db_path).await?;

    // Initialize the embedding model (may download on first run)
    tracing::info!("loading embedding model...");
    let embedder = embed::Embedder::new().context("failed to initialize embedding model")?;
    tracing::info!("embedding model ready");

    // Initialize vector store
    let store = store::VectorStore::new(
        db_path
            .to_str()
            .context("database path contains non-UTF-8 characters")?,
    )
    .await
    .context("failed to open vector store")?;

    // Index files in the background so the MCP server is available immediately
    let indexer = indexer::Indexer::new(embedder.clone(), store.clone());
    let index_root = root.clone();
    tokio::spawn(async move {
        if let Err(e) = indexer.index_directory(&index_root).await {
            tracing::error!("indexing failed: {e:#}");
        }
    });

    // Start MCP server over stdio
    let mcp_server = server::ClaudevilServer::new(embedder, store, root);
    tracing::info!("MCP server starting on stdio");

    let service = mcp_server
        .serve(rmcp::transport::stdio())
        .await
        .inspect_err(|e| tracing::error!("serve error: {e:?}"))
        .context("MCP server failed to start")?;

    service.waiting().await.context("MCP server error")?;
    Ok(())
}

/// Generate a unique directory name from a root path.
///
/// Uses the directory basename + a truncated hash for human readability
/// while avoiding collisions between different roots with the same name.
fn dir_name_for(root: &std::path::Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut hasher);
    let hash = hasher.finish() as u32;
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_else(|| "root".into());
    format!("{name}-{hash:08x}")
}
