use std::path::PathBuf;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::{ErrorData as McpError, ServerHandler, schemars, tool, tool_handler, tool_router};
use serde::Deserialize;

use crate::embed::Embedder;
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

#[derive(Clone)]
pub struct ClaudevilServer {
    embedder: Embedder,
    store: VectorStore,
    root: PathBuf,
    tool_router: ToolRouter<Self>,
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

        let mut output = String::new();
        for result in results.iter().take(limit) {
            let symbol_info = match (&result.symbol_kind, &result.symbol_name) {
                (Some(kind), Some(name)) => format!(" ({kind} {name})"),
                _ => String::new(),
            };

            output.push_str(&format!(
                "## {path}:{start}-{end}{symbol_info} [{dist:.3}]\n```\n{content}\n```\n\n",
                path = result.file_path,
                start = result.start_line,
                end = result.end_line,
                dist = result.distance,
                content = result.content,
            ));
        }

        Ok(CallToolResult::success(vec![Content::text(output)]))
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
}

#[tool_handler]
impl ServerHandler for ClaudevilServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            instructions: Some(
                "claudevil provides semantic code search over local files. \
                 Use the 'search' tool with natural language queries to find \
                 relevant code in the indexed codebase."
                    .into(),
            ),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..Default::default()
        }
    }
}
