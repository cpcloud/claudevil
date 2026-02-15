use std::path::PathBuf;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::{ErrorData as McpError, ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;

use crate::embed::Embedder;
use crate::indexer::Indexer;
use crate::store::VectorStore;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchParams {
    /// Natural language search query describing what you're looking for.
    pub query: String,
    /// Filter by programming language (e.g. "go"). If omitted, searches all languages.
    pub language: Option<String>,
    /// Maximum number of results to return (default: 10).
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct IndexStatusParams {}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FindSymbolParams {
    /// Symbol name to search for (case-insensitive substring match).
    pub name: String,
    /// Filter by symbol kind (e.g. "func", "method", "type", "interface").
    pub kind: Option<String>,
    /// Maximum number of results to return (default: 20).
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListFilesParams {
    /// Filter by programming language (e.g. "go"). If omitted, lists all indexed files.
    pub language: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadFileParams {
    /// File path relative to the indexed root directory.
    pub path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReindexParams {}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FindSimilarParams {
    /// Code snippet to find similar chunks for.
    pub code: String,
    /// Filter by programming language (e.g. "go"). If omitted, searches all languages.
    pub language: Option<String>,
    /// Maximum number of results to return (default: 10).
    pub limit: Option<usize>,
}

#[derive(Clone)]
pub struct ClaudevilServer {
    embedder: Embedder,
    store: VectorStore,
    root: PathBuf,
    tool_router: ToolRouter<Self>,
}

/// Format search results into a markdown string.
fn format_results(results: &[crate::store::SearchResult], show_distance: bool) -> String {
    let mut output = String::new();
    for result in results {
        let symbol_info = match (&result.symbol_kind, &result.symbol_name) {
            (Some(kind), Some(name)) => format!(" ({kind} {name})"),
            _ => String::new(),
        };

        if show_distance {
            output.push_str(&format!(
                "## {path}:{start}-{end}{symbol_info} [{dist:.3}]\n```\n{content}\n```\n\n",
                path = result.file_path,
                start = result.start_line,
                end = result.end_line,
                dist = result.distance,
                content = result.content,
            ));
        } else {
            output.push_str(&format!(
                "## {path}:{start}-{end}{symbol_info}\n```\n{content}\n```\n\n",
                path = result.file_path,
                start = result.start_line,
                end = result.end_line,
                content = result.content,
            ));
        }
    }
    output
}

#[tool_router]
impl ClaudevilServer {
    pub fn new(embedder: Embedder, store: VectorStore, root: PathBuf) -> Self {
        Self {
            embedder,
            store,
            root,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Semantic code search over the indexed codebase. Finds functions, types, methods, and other code by natural language query. Returns matching code chunks with file paths and line numbers."
    )]
    async fn search(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let limit = params.limit.unwrap_or(10);

        let query_vec = self
            .embedder
            .embed_one(&params.query)
            .await
            .map_err(|e| McpError::internal_error(format!("embedding failed: {e}"), None))?;

        let results = self
            .store
            .search(&query_vec, limit, params.language.as_deref())
            .await
            .map_err(|e| McpError::internal_error(format!("search failed: {e}"), None))?;

        if results.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No results found. The index may still be building, or no matching code was found.",
            )]));
        }

        Ok(CallToolResult::success(vec![Content::text(
            format_results(&results[..results.len().min(limit)], true),
        )]))
    }

    #[tool(
        description = "Get the current indexing status: number of chunks stored and the root directory being indexed."
    )]
    async fn index_status(
        &self,
        Parameters(_params): Parameters<IndexStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        let count = self
            .store
            .chunk_count()
            .await
            .map_err(|e| McpError::internal_error(format!("count failed: {e}"), None))?;

        let status = format!(
            "Root: {}\nChunks indexed: {count}\nSupported languages: go",
            self.root.display()
        );

        Ok(CallToolResult::success(vec![Content::text(status)]))
    }

    #[tool(
        description = "Find symbols (functions, types, methods, etc.) by name. Performs a case-insensitive substring match on symbol names in the index. Use this when you know the name of what you're looking for."
    )]
    async fn find_symbol(
        &self,
        Parameters(params): Parameters<FindSymbolParams>,
    ) -> Result<CallToolResult, McpError> {
        let limit = params.limit.unwrap_or(20);

        let results = self
            .store
            .find_by_symbol(&params.name, params.kind.as_deref(), limit)
            .await
            .map_err(|e| McpError::internal_error(format!("symbol search failed: {e}"), None))?;

        if results.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "No symbols matching '{}' found in the index.",
                params.name
            ))]));
        }

        Ok(CallToolResult::success(vec![Content::text(
            format_results(&results, false),
        )]))
    }

    #[tool(
        description = "List all files currently in the index. Optionally filter by programming language."
    )]
    async fn list_files(
        &self,
        Parameters(params): Parameters<ListFilesParams>,
    ) -> Result<CallToolResult, McpError> {
        let files = self
            .store
            .list_files(params.language.as_deref())
            .await
            .map_err(|e| McpError::internal_error(format!("list files failed: {e}"), None))?;

        if files.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No files in the index. The index may still be building.",
            )]));
        }

        let output = format!("{} files indexed:\n{}", files.len(), files.join("\n"));
        Ok(CallToolResult::success(vec![Content::text(output)]))
    }

    #[tool(
        description = "Read the contents of a file from the indexed directory. The path must be relative to the project root."
    )]
    async fn read_file(
        &self,
        Parameters(params): Parameters<ReadFileParams>,
    ) -> Result<CallToolResult, McpError> {
        let requested = self.root.join(&params.path);
        let canonical = requested.canonicalize().map_err(|_| {
            McpError::invalid_params(format!("file not found: {}", params.path), None)
        })?;

        // Prevent directory traversal outside the root
        if !canonical.starts_with(&self.root) {
            return Err(McpError::invalid_params(
                format!(
                    "path '{}' is outside the project root -- only files within {} are accessible",
                    params.path,
                    self.root.display()
                ),
                None,
            ));
        }

        let content = tokio::fs::read_to_string(&canonical).await.map_err(|e| {
            McpError::internal_error(format!("failed to read {}: {e}", params.path), None)
        })?;

        Ok(CallToolResult::success(vec![Content::text(content)]))
    }

    #[tool(
        description = "Trigger a full re-index of the codebase. Runs in the background and returns immediately. Use index_status to check progress."
    )]
    async fn reindex(
        &self,
        Parameters(_params): Parameters<ReindexParams>,
    ) -> Result<CallToolResult, McpError> {
        let indexer = Indexer::new(self.embedder.clone(), self.store.clone());
        let root = self.root.clone();
        tokio::spawn(async move {
            if let Err(e) = indexer.index_directory(&root).await {
                tracing::error!("reindex failed: {e:#}");
            }
        });

        Ok(CallToolResult::success(vec![Content::text(format!(
            "Re-indexing started for {}. Use index_status to check progress.",
            self.root.display()
        ))]))
    }

    #[tool(
        description = "Find code chunks semantically similar to a given code snippet. Use this to find related implementations, similar patterns, or duplicated logic."
    )]
    async fn find_similar(
        &self,
        Parameters(params): Parameters<FindSimilarParams>,
    ) -> Result<CallToolResult, McpError> {
        let limit = params.limit.unwrap_or(10);

        let query_vec = self
            .embedder
            .embed_one(&params.code)
            .await
            .map_err(|e| McpError::internal_error(format!("embedding failed: {e}"), None))?;

        let results = self
            .store
            .search(&query_vec, limit, params.language.as_deref())
            .await
            .map_err(|e| McpError::internal_error(format!("search failed: {e}"), None))?;

        if results.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No similar code found. The index may still be building.",
            )]));
        }

        Ok(CallToolResult::success(vec![Content::text(
            format_results(&results[..results.len().min(limit)], true),
        )]))
    }
}

#[tool_handler]
impl ServerHandler for ClaudevilServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "claudevil provides semantic code search over local files. \
                 Use the 'search' tool with natural language queries to find \
                 relevant code in the indexed codebase. Use 'find_symbol' for \
                 exact name lookups, 'list_files' to see indexed files, \
                 'read_file' to view file contents, 'reindex' to refresh the \
                 index, and 'find_similar' to find related code."
                    .into(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}
