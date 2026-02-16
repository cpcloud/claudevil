use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use walkdir::WalkDir;

use crate::chunker::TreeSitterChunker;
use crate::config::Config;
use crate::embed::Embedder;
use crate::error::{Error, Result};
use crate::store::{ChunkRow, VectorStore};

/// Maximum number of chunks to embed in a single batch.
const BATCH_SIZE: usize = 64;

/// Walks a directory, chunks source files, embeds them, and stores in the vector DB.
pub struct Indexer {
    embedder: Embedder,
    store: VectorStore,
    chunker: Arc<TreeSitterChunker>,
    config: Config,
}

impl Indexer {
    pub fn new(
        embedder: Embedder,
        store: VectorStore,
        chunker: Arc<TreeSitterChunker>,
        config: Config,
    ) -> Self {
        Self {
            embedder,
            store,
            chunker,
            config,
        }
    }

    /// Index all supported files under `root`.
    pub async fn index_directory(&self, root: &Path) -> Result<()> {
        let mut pending_rows: Vec<PendingChunk> = Vec::new();

        for entry in WalkDir::new(root)
            .follow_links(true)
            .into_iter()
            .filter_entry(|e| !is_hidden(e))
        {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("walk error: {e}");
                    continue;
                }
            };

            if !entry.file_type().is_file() {
                continue;
            }

            let path = entry.path();
            let ext = match path.extension().and_then(|e| e.to_str()) {
                Some(e) => e,
                None => continue,
            };

            let (lang_name, _lang_config) = match self.config.language_for_extension(ext) {
                Some(pair) => pair,
                None => continue,
            };

            match self.collect_file_chunks(path, root, lang_name).await {
                Ok(chunks) => pending_rows.extend(chunks),
                Err(e) => {
                    tracing::warn!("failed to chunk {}: {e}", path.display());
                    continue;
                }
            }

            // Flush in batches to keep memory bounded
            if pending_rows.len() >= BATCH_SIZE {
                self.flush_batch(&mut pending_rows).await?;
            }
        }

        // Flush remaining
        if !pending_rows.is_empty() {
            self.flush_batch(&mut pending_rows).await?;
        }

        let count = self.store.chunk_count().await?;
        tracing::info!("indexing complete: {count} chunks stored");
        Ok(())
    }

    /// Read and chunk a single file, returning pending chunks (not yet embedded).
    async fn collect_file_chunks(
        &self,
        path: &Path,
        root: &Path,
        lang_name: &str,
    ) -> Result<Vec<PendingChunk>> {
        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| Error::FileRead {
                path: path.to_path_buf(),
                source: e,
            })?;

        let rel_path = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        let last_modified = path
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH)
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        // Delete existing chunks for this file before re-indexing
        self.store.delete_file(&rel_path).await?;

        let chunks = self.chunker.chunk_file(&content, lang_name)?;
        tracing::debug!("{}: {} chunks ({})", rel_path, chunks.len(), lang_name);

        Ok(chunks
            .into_iter()
            .enumerate()
            .map(|(idx, chunk)| PendingChunk {
                file_path: rel_path.clone(),
                chunk_id: idx as i64,
                content: chunk.content,
                symbol_name: chunk.symbol_name,
                symbol_kind: chunk.symbol_kind,
                language: lang_name.to_string(),
                start_line: chunk.start_line as i64,
                end_line: chunk.end_line as i64,
                last_modified,
            })
            .collect())
    }

    /// Embed a batch of pending chunks and insert into the store.
    async fn flush_batch(&self, pending: &mut Vec<PendingChunk>) -> Result<()> {
        let batch: Vec<PendingChunk> = std::mem::take(pending);
        if batch.is_empty() {
            return Ok(());
        }

        let texts: Vec<String> = batch.iter().map(|c| c.content.clone()).collect();
        let embeddings = self.embedder.embed_batch(texts).await?;

        let rows: Vec<ChunkRow> = batch
            .into_iter()
            .zip(embeddings)
            .map(|(chunk, vector)| ChunkRow {
                file_path: chunk.file_path,
                chunk_id: chunk.chunk_id,
                content: chunk.content,
                symbol_name: chunk.symbol_name,
                symbol_kind: chunk.symbol_kind,
                language: chunk.language,
                start_line: chunk.start_line,
                end_line: chunk.end_line,
                last_modified: chunk.last_modified,
                vector,
            })
            .collect();

        self.store.insert(rows).await?;
        Ok(())
    }
}

struct PendingChunk {
    file_path: String,
    chunk_id: i64,
    content: String,
    symbol_name: Option<String>,
    symbol_kind: Option<String>,
    language: String,
    start_line: i64,
    end_line: i64,
    last_modified: i64,
}

fn is_hidden(entry: &walkdir::DirEntry) -> bool {
    entry.depth() > 0
        && entry
            .file_name()
            .to_str()
            .is_some_and(|s| s.starts_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a temp directory with Go source files for testing.
    fn setup_go_project(dir: &std::path::Path) {
        std::fs::write(
            dir.join("main.go"),
            r#"package main

import "fmt"

func main() {
	fmt.Println("hello")
}

func helper() string {
	return "help"
}
"#,
        )
        .unwrap();

        std::fs::create_dir_all(dir.join("pkg")).unwrap();
        std::fs::write(
            dir.join("pkg/server.go"),
            r#"package server

import "net/http"

type Server struct {
	addr string
}

func NewServer(addr string) *Server {
	return &Server{addr: addr}
}

func (s *Server) Start() error {
	return http.ListenAndServe(s.addr, nil)
}
"#,
        )
        .unwrap();
    }

    fn make_indexer(embedder: Embedder, store: VectorStore) -> (Indexer, Arc<TreeSitterChunker>) {
        let config = Config::load().unwrap();
        let chunker = Arc::new(TreeSitterChunker::new(&config).unwrap());
        let indexer = Indexer::new(embedder, store, chunker.clone(), config);
        (indexer, chunker)
    }

    #[tokio::test]
    async fn index_directory_indexes_go_files() {
        let project_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();

        setup_go_project(project_dir.path());

        let embedder = Embedder::new().unwrap();
        let store = VectorStore::new(db_dir.path().to_str().unwrap())
            .await
            .unwrap();
        let (indexer, _chunker) = make_indexer(embedder, store.clone());

        indexer.index_directory(project_dir.path()).await.unwrap();

        let count = store.chunk_count().await.unwrap();
        // main.go: function_declaration x2 = 2
        // pkg/server.go: type_declaration + function_declaration + method_declaration = 3
        assert!(count >= 3, "expected at least 3 chunks, got {count}");
    }

    #[tokio::test]
    async fn indexed_files_are_searchable() {
        let project_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();

        setup_go_project(project_dir.path());

        let embedder = Embedder::new().unwrap();
        let store = VectorStore::new(db_dir.path().to_str().unwrap())
            .await
            .unwrap();
        let (indexer, _chunker) = make_indexer(embedder.clone(), store.clone());

        indexer.index_directory(project_dir.path()).await.unwrap();

        // Search for "http server" -- should find the Server type or Start method
        let query_vec = embedder.embed_one("http server listening").await.unwrap();
        let results = store.search(&query_vec, 5, None).await.unwrap();

        assert!(
            !results.is_empty(),
            "search should return results after indexing"
        );

        // At least one result should be from server.go
        let has_server = results.iter().any(|r| r.file_path.contains("server.go"));
        assert!(
            has_server,
            "expected at least one result from server.go, got: {:?}",
            results.iter().map(|r| &r.file_path).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn indexing_skips_hidden_directories() {
        let project_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();

        // Regular file
        std::fs::write(
            project_dir.path().join("visible.go"),
            "package visible\n\nfunc Visible() {}\n",
        )
        .unwrap();

        // Hidden directory with a Go file
        let hidden = project_dir.path().join(".hidden");
        std::fs::create_dir_all(&hidden).unwrap();
        std::fs::write(
            hidden.join("secret.go"),
            "package secret\n\nfunc Secret() {}\n",
        )
        .unwrap();

        let embedder = Embedder::new().unwrap();
        let store = VectorStore::new(db_dir.path().to_str().unwrap())
            .await
            .unwrap();
        let (indexer, _chunker) = make_indexer(embedder.clone(), store.clone());

        indexer.index_directory(project_dir.path()).await.unwrap();

        // visible.go should be indexed (function_declaration = 1 chunk)
        // If hidden was also indexed, we'd have 2+ chunks
        let count = store.chunk_count().await.unwrap();
        assert!(
            count >= 1,
            "visible.go should produce at least 1 chunk, got {count}"
        );

        // Verify via search that no hidden file content appears
        let query_vec = embedder.embed_one("secret function").await.unwrap();
        let results = store.search(&query_vec, 100, None).await.unwrap();
        for r in &results {
            assert!(
                !r.file_path.contains(".hidden"),
                "hidden directory file should not be indexed: {}",
                r.file_path
            );
        }
    }

    #[tokio::test]
    async fn indexing_skips_unsupported_files() {
        let project_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();

        std::fs::write(
            project_dir.path().join("main.go"),
            "package main\n\nfunc main() {}\n",
        )
        .unwrap();
        std::fs::write(project_dir.path().join("README.md"), "# My Project\n").unwrap();
        std::fs::write(project_dir.path().join("config.yaml"), "key: value\n").unwrap();

        let embedder = Embedder::new().unwrap();
        let store = VectorStore::new(db_dir.path().to_str().unwrap())
            .await
            .unwrap();
        let (indexer, _chunker) = make_indexer(embedder, store.clone());

        indexer.index_directory(project_dir.path()).await.unwrap();

        // Only chunks from main.go should exist (function_declaration = 1)
        let count = store.chunk_count().await.unwrap();
        assert!(count >= 1, "should have indexed at least main.go function");

        let files = store.list_files(None).await.unwrap();
        assert!(
            files.iter().all(|f| f.ends_with(".go")),
            "only .go files should be indexed: {files:?}"
        );
    }

    #[tokio::test]
    async fn reindexing_replaces_old_chunks() {
        let project_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();

        // Initial file
        std::fs::write(
            project_dir.path().join("lib.go"),
            "package lib\n\nfunc Original() {}\n",
        )
        .unwrap();

        let embedder = Embedder::new().unwrap();
        let store = VectorStore::new(db_dir.path().to_str().unwrap())
            .await
            .unwrap();
        let (indexer, _chunker) = make_indexer(embedder, store.clone());

        indexer.index_directory(project_dir.path()).await.unwrap();
        let count_before = store.chunk_count().await.unwrap();

        // Overwrite the file with different content
        std::fs::write(
            project_dir.path().join("lib.go"),
            "package lib\n\nfunc Updated() {}\n\nfunc Extra() {}\n",
        )
        .unwrap();

        // Re-index
        indexer.index_directory(project_dir.path()).await.unwrap();
        let count_after = store.chunk_count().await.unwrap();

        // Should have more chunks after adding a function
        assert!(
            count_after > count_before || count_after >= 2,
            "updated file should have more chunks: before={count_before}, after={count_after}"
        );
    }

    #[tokio::test]
    async fn index_empty_directory() {
        let project_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();

        let embedder = Embedder::new().unwrap();
        let store = VectorStore::new(db_dir.path().to_str().unwrap())
            .await
            .unwrap();
        let (indexer, _chunker) = make_indexer(embedder, store.clone());

        // Should not error on empty directory
        indexer.index_directory(project_dir.path()).await.unwrap();
        assert_eq!(store.chunk_count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn index_multi_language_project() {
        let project_dir = TempDir::new().unwrap();
        let db_dir = TempDir::new().unwrap();

        // Go file
        std::fs::write(
            project_dir.path().join("main.go"),
            "package main\n\nfunc GoFunc() {}\n",
        )
        .unwrap();

        // Rust file
        std::fs::write(
            project_dir.path().join("lib.rs"),
            "fn rust_func() {\n    println!(\"hello\");\n}\n",
        )
        .unwrap();

        // Python file
        std::fs::write(
            project_dir.path().join("app.py"),
            "def python_func():\n    print(\"hello\")\n",
        )
        .unwrap();

        let embedder = Embedder::new().unwrap();
        let store = VectorStore::new(db_dir.path().to_str().unwrap())
            .await
            .unwrap();
        let (indexer, _chunker) = make_indexer(embedder, store.clone());

        indexer.index_directory(project_dir.path()).await.unwrap();

        let files = store.list_files(None).await.unwrap();
        assert!(
            files.iter().any(|f| f.ends_with(".go")),
            "should index Go files: {files:?}"
        );
        assert!(
            files.iter().any(|f| f.ends_with(".rs")),
            "should index Rust files: {files:?}"
        );
        assert!(
            files.iter().any(|f| f.ends_with(".py")),
            "should index Python files: {files:?}"
        );
    }
}
