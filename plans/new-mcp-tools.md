# New MCP Tools

Extend the claudevil MCP server with five additional tools to make it
a more complete code intelligence provider.

## Motivation

The server currently has two tools: `search` (semantic vector search) and
`index_status`. Adding these tools makes claudevil useful as a general-purpose
code navigation backend -- not just semantic search.

## New Tools

### 1. `find_symbol`

Exact/substring match on indexed symbol names. Unlike `search`, this does
**not** use vector similarity -- it queries the `symbol_name` column directly.

**Parameters**: `name` (string, required), `kind` (optional symbol kind
filter), `limit` (optional, default 20).

**Implementation**: Add `VectorStore::find_by_symbol()` that uses a LanceDB
scan query with an `only_if` filter on the `symbol_name` column.

### 2. `list_files`

Returns the list of files currently in the index.

**Parameters**: `language` (optional filter).

**Implementation**: Add `VectorStore::list_files()` that scans the
`file_path` column, deduplicates, and returns sorted paths.

### 3. `read_file`

Reads a file from the indexed root directory and returns its contents. The
path is validated to be within the root to prevent directory traversal.

**Parameters**: `path` (string, required -- relative to root).

**Implementation**: Handled entirely in `server.rs`. Resolves the path
relative to `self.root`, canonicalizes, validates it starts with `self.root`,
reads and returns content.

### 4. `reindex`

Triggers a full re-index of the codebase. Runs asynchronously in the
background.

**Parameters**: none.

**Implementation**: Constructs an `Indexer` from the server's embedder and
store, spawns `index_directory` on a background task. Returns immediately
with a confirmation message.

### 5. `find_similar`

Given a code snippet, finds semantically similar chunks. Like `search` but
accepts code input rather than a natural language query.

**Parameters**: `code` (string, required), `language` (optional filter),
`limit` (optional, default 10).

**Implementation**: Embeds the input code and runs vector search -- same
pipeline as `search` but with different framing for the MCP tool description.

## Store Changes

Two new methods on `VectorStore`:

- `find_by_symbol(&self, pattern: &str, kind: Option<&str>, limit: usize)`
  -- scan with `symbol_name LIKE '%pattern%'` filter
- `list_files(&self, language: Option<&str>)` -- scan file_path column,
  deduplicate in Rust

## Test Strategy

Tests must survive chunking strategy changes (regex -> tree-sitter). Assert
on **semantic outcomes**, not implementation artifacts:

- **Do**: assert a result contains a known file path or symbol name
- **Do**: assert result count is > 0 or within a reasonable range
- **Don't**: assert exact chunk counts, exact line ranges, exact content
  strings, or distance scores

### Test plan by tool

- `find_symbol`: insert rows with known symbol names via `VectorStore`
  directly, verify they're found by name
- `list_files`: insert rows for multiple files, verify all file paths
  appear in the result
- `read_file`: create temp files, verify contents are returned; test
  path traversal rejection
- `reindex`: test at the indexer level -- index a directory, modify a file,
  reindex, verify the store reflects the update
- `find_similar`: use the embedder to verify that similar code returns
  higher-ranked results than dissimilar code
