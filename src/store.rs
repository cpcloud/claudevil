use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

use crate::embed::EMBEDDING_DIM;
use crate::error::{Error, Result};

const INDEX_FILE: &str = "index.usearch";
const META_FILE: &str = "metadata.json";

// usearch::Index contains raw C++ pointers that aren't Send/Sync in Rust,
// but the underlying C++ library is thread-safe for concurrent reads and
// exclusive writes -- which we enforce via RwLock.
struct SendSyncIndex(Index);
unsafe impl Send for SendSyncIndex {}
unsafe impl Sync for SendSyncIndex {}

#[derive(Serialize, Deserialize)]
struct Metadata {
    next_key: u64,
    chunks: HashMap<u64, ChunkMeta>,
}

#[derive(Clone, Serialize, Deserialize)]
struct ChunkMeta {
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

/// A row ready to be inserted into the vector store.
pub struct ChunkRow {
    pub file_path: String,
    pub chunk_id: i64,
    pub content: String,
    pub symbol_name: Option<String>,
    pub symbol_kind: Option<String>,
    pub language: String,
    pub start_line: i64,
    pub end_line: i64,
    pub last_modified: i64,
    pub vector: Vec<f32>,
}

/// A single search result.
#[derive(Debug)]
pub struct SearchResult {
    pub file_path: String,
    pub content: String,
    pub symbol_name: Option<String>,
    pub symbol_kind: Option<String>,
    pub start_line: i64,
    pub end_line: i64,
    pub distance: f32,
}

#[derive(Clone)]
pub struct VectorStore {
    index: Arc<RwLock<SendSyncIndex>>,
    meta: Arc<RwLock<Metadata>>,
    db_path: PathBuf,
}

impl VectorStore {
    pub async fn new(path: &str) -> Result<Self> {
        let db_path = PathBuf::from(path);
        let index_path = db_path.join(INDEX_FILE);
        let meta_path = db_path.join(META_FILE);

        let opts = IndexOptions {
            dimensions: EMBEDDING_DIM,
            metric: MetricKind::Cos,
            quantization: ScalarKind::F32,
            ..Default::default()
        };

        let index = Index::new(&opts).map_err(|e| Error::StoreIndex(e.to_string()))?;

        // Load existing index from disk if present
        if index_path.exists() {
            index
                .load(index_path.to_str().unwrap_or_default())
                .map_err(|e| Error::StoreIndex(e.to_string()))?;
        }

        // Load existing metadata or start fresh
        let meta = if meta_path.exists() {
            let data = tokio::fs::read_to_string(&meta_path)
                .await
                .map_err(|e| Error::StoreIo {
                    context: format!("reading {}", meta_path.display()),
                    source: e,
                })?;
            serde_json::from_str(&data).map_err(Error::StoreSerde)?
        } else {
            Metadata {
                next_key: 0,
                chunks: HashMap::new(),
            }
        };

        Ok(Self {
            index: Arc::new(RwLock::new(SendSyncIndex(index))),
            meta: Arc::new(RwLock::new(meta)),
            db_path,
        })
    }

    /// Insert a batch of chunk rows.
    pub async fn insert(&self, rows: Vec<ChunkRow>) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }

        let mut meta = self.meta.write().await;
        let index = self.index.write().await;

        // Reserve space in the index for the new rows
        let new_capacity = index.0.size() + rows.len();
        index
            .0
            .reserve(new_capacity)
            .map_err(|e| Error::StoreIndex(e.to_string()))?;

        for row in rows {
            let key = meta.next_key;
            meta.next_key += 1;

            index
                .0
                .add(key, &row.vector)
                .map_err(|e| Error::StoreIndex(e.to_string()))?;

            meta.chunks.insert(
                key,
                ChunkMeta {
                    file_path: row.file_path,
                    chunk_id: row.chunk_id,
                    content: row.content,
                    symbol_name: row.symbol_name,
                    symbol_kind: row.symbol_kind,
                    language: row.language,
                    start_line: row.start_line,
                    end_line: row.end_line,
                    last_modified: row.last_modified,
                },
            );
        }

        self.persist_locked(&index, &meta).await?;
        Ok(())
    }

    /// Semantic search by vector similarity.
    pub async fn search(
        &self,
        query_vec: &[f32],
        limit: usize,
        language_filter: Option<&str>,
    ) -> Result<Vec<SearchResult>> {
        let index = self.index.read().await;
        let meta = self.meta.read().await;

        if meta.chunks.is_empty() {
            return Ok(Vec::new());
        }

        let results = match language_filter {
            Some(lang) => {
                let lang = lang.to_string();
                index
                    .0
                    .filtered_search(query_vec, limit, |key| {
                        meta.chunks.get(&key).is_some_and(|c| c.language == lang)
                    })
                    .map_err(|e| Error::StoreIndex(e.to_string()))?
            }
            None => index
                .0
                .search(query_vec, limit)
                .map_err(|e| Error::StoreIndex(e.to_string()))?,
        };

        Ok(results
            .keys
            .iter()
            .zip(results.distances.iter())
            .filter_map(|(&key, &dist)| {
                let chunk = meta.chunks.get(&key)?;
                Some(SearchResult {
                    file_path: chunk.file_path.clone(),
                    content: chunk.content.clone(),
                    symbol_name: chunk.symbol_name.clone(),
                    symbol_kind: chunk.symbol_kind.clone(),
                    start_line: chunk.start_line,
                    end_line: chunk.end_line,
                    distance: dist,
                })
            })
            .collect())
    }

    /// Delete all chunks for a given file path.
    pub async fn delete_file(&self, file_path: &str) -> Result<()> {
        let mut meta = self.meta.write().await;
        let index = self.index.write().await;

        let keys_to_remove: Vec<u64> = meta
            .chunks
            .iter()
            .filter(|(_, c)| c.file_path == file_path)
            .map(|(&k, _)| k)
            .collect();

        if keys_to_remove.is_empty() {
            return Ok(());
        }

        for &key in &keys_to_remove {
            index
                .0
                .remove(key)
                .map_err(|e| Error::StoreIndex(e.to_string()))?;
            meta.chunks.remove(&key);
        }

        self.persist_locked(&index, &meta).await?;
        Ok(())
    }

    /// Find chunks whose symbol_name contains the given pattern (case-insensitive substring match).
    pub async fn find_by_symbol(
        &self,
        pattern: &str,
        kind_filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let meta = self.meta.read().await;
        let lower_pattern = pattern.to_lowercase();

        let results: Vec<SearchResult> = meta
            .chunks
            .values()
            .filter(|c| {
                c.symbol_name
                    .as_ref()
                    .is_some_and(|name| name.to_lowercase().contains(&lower_pattern))
            })
            .filter(|c| kind_filter.is_none_or(|kind| c.symbol_kind.as_deref() == Some(kind)))
            .take(limit)
            .map(|c| SearchResult {
                file_path: c.file_path.clone(),
                content: c.content.clone(),
                symbol_name: c.symbol_name.clone(),
                symbol_kind: c.symbol_kind.clone(),
                start_line: c.start_line,
                end_line: c.end_line,
                distance: 0.0,
            })
            .collect();

        Ok(results)
    }

    /// Return distinct file paths in the index, optionally filtered by language.
    pub async fn list_files(&self, language_filter: Option<&str>) -> Result<Vec<String>> {
        let meta = self.meta.read().await;

        let mut paths = std::collections::BTreeSet::new();
        for chunk in meta.chunks.values() {
            if language_filter.is_none_or(|lang| chunk.language == lang) {
                paths.insert(chunk.file_path.clone());
            }
        }

        Ok(paths.into_iter().collect())
    }

    /// Count total indexed chunks.
    pub async fn chunk_count(&self) -> Result<u64> {
        let meta = self.meta.read().await;
        Ok(meta.chunks.len() as u64)
    }

    /// Persist index and metadata to disk. Caller must hold both locks.
    async fn persist_locked(&self, index: &SendSyncIndex, meta: &Metadata) -> Result<()> {
        let index_path = self.db_path.join(INDEX_FILE);
        let meta_path = self.db_path.join(META_FILE);

        index
            .0
            .save(index_path.to_str().unwrap_or_default())
            .map_err(|e| Error::StoreIndex(e.to_string()))?;

        let json = serde_json::to_string(meta).map_err(Error::StoreSerde)?;
        tokio::fs::write(&meta_path, json)
            .await
            .map_err(|e| Error::StoreIo {
                context: format!("writing {}", meta_path.display()),
                source: e,
            })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_vector(seed: f32) -> Vec<f32> {
        // Create a normalized vector with a distinguishable direction.
        let mut v = vec![0.0f32; EMBEDDING_DIM];
        let idx = (seed.abs() as usize) % EMBEDDING_DIM;
        v[idx] = 1.0;
        for (i, val) in v.iter_mut().enumerate() {
            *val += (i as f32 * seed * 0.001).sin() * 0.01;
        }
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter_mut().for_each(|x| *x /= norm);
        v
    }

    fn sample_row(
        file_path: &str,
        chunk_id: i64,
        content: &str,
        language: &str,
        vector: Vec<f32>,
    ) -> ChunkRow {
        ChunkRow {
            file_path: file_path.to_string(),
            chunk_id,
            content: content.to_string(),
            symbol_name: Some(format!("symbol_{chunk_id}")),
            symbol_kind: Some("func".to_string()),
            language: language.to_string(),
            start_line: chunk_id * 10 + 1,
            end_line: chunk_id * 10 + 9,
            last_modified: 1700000000,
            vector,
        }
    }

    #[tokio::test]
    async fn empty_store_has_zero_count() {
        let tmp = TempDir::new().unwrap();
        let store = VectorStore::new(tmp.path().to_str().unwrap())
            .await
            .unwrap();
        assert_eq!(store.chunk_count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn empty_store_search_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let store = VectorStore::new(tmp.path().to_str().unwrap())
            .await
            .unwrap();
        let query = make_vector(1.0);
        let results = store.search(&query, 10, None).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn insert_empty_batch_is_noop() {
        let tmp = TempDir::new().unwrap();
        let store = VectorStore::new(tmp.path().to_str().unwrap())
            .await
            .unwrap();
        store.insert(vec![]).await.unwrap();
        assert_eq!(store.chunk_count().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn insert_and_count() {
        let tmp = TempDir::new().unwrap();
        let store = VectorStore::new(tmp.path().to_str().unwrap())
            .await
            .unwrap();

        let rows = vec![
            sample_row("main.go", 0, "func main() {}", "go", make_vector(1.0)),
            sample_row("main.go", 1, "func helper() {}", "go", make_vector(2.0)),
            sample_row("util.go", 0, "func util() {}", "go", make_vector(3.0)),
        ];
        store.insert(rows).await.unwrap();

        assert_eq!(store.chunk_count().await.unwrap(), 3);
    }

    #[tokio::test]
    async fn search_returns_closest_match() {
        let tmp = TempDir::new().unwrap();
        let store = VectorStore::new(tmp.path().to_str().unwrap())
            .await
            .unwrap();

        let v1 = make_vector(1.0);
        let v2 = make_vector(50.0);
        let v3 = make_vector(100.0);

        let rows = vec![
            sample_row("a.go", 0, "func alpha() {}", "go", v1.clone()),
            sample_row("b.go", 0, "func beta() {}", "go", v2.clone()),
            sample_row("c.go", 0, "func gamma() {}", "go", v3),
        ];
        store.insert(rows).await.unwrap();

        let results = store.search(&v1, 3, None).await.unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].content, "func alpha() {}");
        assert!(results[0].distance < results.last().unwrap().distance);
    }

    #[tokio::test]
    async fn search_respects_limit() {
        let tmp = TempDir::new().unwrap();
        let store = VectorStore::new(tmp.path().to_str().unwrap())
            .await
            .unwrap();

        let rows: Vec<ChunkRow> = (0..10)
            .map(|i| {
                sample_row(
                    "file.go",
                    i,
                    &format!("func f{i}() {{}}"),
                    "go",
                    make_vector(i as f32),
                )
            })
            .collect();
        store.insert(rows).await.unwrap();

        let results = store.search(&make_vector(0.0), 3, None).await.unwrap();
        assert_eq!(results.len(), 3);
    }

    #[tokio::test]
    async fn delete_file_removes_only_that_file() {
        let tmp = TempDir::new().unwrap();
        let store = VectorStore::new(tmp.path().to_str().unwrap())
            .await
            .unwrap();

        let rows = vec![
            sample_row("keep.go", 0, "func keep() {}", "go", make_vector(1.0)),
            sample_row("keep.go", 1, "func keep2() {}", "go", make_vector(2.0)),
            sample_row("remove.go", 0, "func remove() {}", "go", make_vector(3.0)),
        ];
        store.insert(rows).await.unwrap();
        assert_eq!(store.chunk_count().await.unwrap(), 3);

        store.delete_file("remove.go").await.unwrap();

        assert_eq!(store.chunk_count().await.unwrap(), 2);

        let results = store.search(&make_vector(3.0), 10, None).await.unwrap();
        for r in &results {
            assert_eq!(
                r.file_path, "keep.go",
                "deleted file should not appear in results"
            );
        }
    }

    #[tokio::test]
    async fn search_with_language_filter() {
        let tmp = TempDir::new().unwrap();
        let store = VectorStore::new(tmp.path().to_str().unwrap())
            .await
            .unwrap();

        let rows = vec![
            sample_row("main.go", 0, "func goFunc() {}", "go", make_vector(1.0)),
            sample_row("main.rs", 0, "fn rust_func() {}", "rust", make_vector(2.0)),
            sample_row(
                "app.py",
                0,
                "def python_func():",
                "python",
                make_vector(3.0),
            ),
        ];
        store.insert(rows).await.unwrap();

        let results = store
            .search(&make_vector(1.0), 10, Some("go"))
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "func goFunc() {}");

        let results = store
            .search(&make_vector(2.0), 10, Some("rust"))
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "fn rust_func() {}");
    }

    #[tokio::test]
    async fn search_result_contains_metadata() {
        let tmp = TempDir::new().unwrap();
        let store = VectorStore::new(tmp.path().to_str().unwrap())
            .await
            .unwrap();

        let rows = vec![ChunkRow {
            file_path: "handler.go".to_string(),
            chunk_id: 0,
            content: "func HandleRequest() {}".to_string(),
            symbol_name: Some("HandleRequest".to_string()),
            symbol_kind: Some("func".to_string()),
            language: "go".to_string(),
            start_line: 10,
            end_line: 25,
            last_modified: 1700000000,
            vector: make_vector(1.0),
        }];
        store.insert(rows).await.unwrap();

        let results = store.search(&make_vector(1.0), 1, None).await.unwrap();
        assert_eq!(results.len(), 1);
        let r = &results[0];
        assert_eq!(r.file_path, "handler.go");
        assert_eq!(r.content, "func HandleRequest() {}");
        assert_eq!(r.symbol_name.as_deref(), Some("HandleRequest"));
        assert_eq!(r.symbol_kind.as_deref(), Some("func"));
        assert_eq!(r.start_line, 10);
        assert_eq!(r.end_line, 25);
    }

    #[tokio::test]
    async fn insert_multiple_batches() {
        let tmp = TempDir::new().unwrap();
        let store = VectorStore::new(tmp.path().to_str().unwrap())
            .await
            .unwrap();

        let rows1 = vec![sample_row("a.go", 0, "func a() {}", "go", make_vector(1.0))];
        store.insert(rows1).await.unwrap();

        let rows2 = vec![
            sample_row("b.go", 0, "func b() {}", "go", make_vector(2.0)),
            sample_row("c.go", 0, "func c() {}", "go", make_vector(3.0)),
        ];
        store.insert(rows2).await.unwrap();

        assert_eq!(store.chunk_count().await.unwrap(), 3);
    }

    #[tokio::test]
    async fn delete_nonexistent_file_is_noop() {
        let tmp = TempDir::new().unwrap();
        let store = VectorStore::new(tmp.path().to_str().unwrap())
            .await
            .unwrap();

        let rows = vec![sample_row(
            "exists.go",
            0,
            "func exists() {}",
            "go",
            make_vector(1.0),
        )];
        store.insert(rows).await.unwrap();

        store.delete_file("does_not_exist.go").await.unwrap();
        assert_eq!(store.chunk_count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn null_optional_fields() {
        let tmp = TempDir::new().unwrap();
        let store = VectorStore::new(tmp.path().to_str().unwrap())
            .await
            .unwrap();

        let rows = vec![ChunkRow {
            file_path: "test.go".to_string(),
            chunk_id: 0,
            content: "package main".to_string(),
            symbol_name: None,
            symbol_kind: None,
            language: "go".to_string(),
            start_line: 1,
            end_line: 1,
            last_modified: 1700000000,
            vector: make_vector(1.0),
        }];
        store.insert(rows).await.unwrap();

        let results = store.search(&make_vector(1.0), 1, None).await.unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].symbol_name.is_none());
        assert!(results[0].symbol_kind.is_none());
    }

    // ---------------------------------------------------------------
    // find_by_symbol tests
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn find_by_symbol_matches_substring() {
        let tmp = TempDir::new().unwrap();
        let store = VectorStore::new(tmp.path().to_str().unwrap())
            .await
            .unwrap();

        let rows = vec![
            ChunkRow {
                file_path: "server.go".to_string(),
                chunk_id: 0,
                content: "func NewServer() {}".to_string(),
                symbol_name: Some("NewServer".to_string()),
                symbol_kind: Some("func".to_string()),
                language: "go".to_string(),
                start_line: 1,
                end_line: 1,
                last_modified: 1700000000,
                vector: make_vector(1.0),
            },
            ChunkRow {
                file_path: "server.go".to_string(),
                chunk_id: 1,
                content: "func (s *Server) Start() {}".to_string(),
                symbol_name: Some("Server.Start".to_string()),
                symbol_kind: Some("method".to_string()),
                language: "go".to_string(),
                start_line: 3,
                end_line: 3,
                last_modified: 1700000000,
                vector: make_vector(2.0),
            },
            ChunkRow {
                file_path: "client.go".to_string(),
                chunk_id: 0,
                content: "func NewClient() {}".to_string(),
                symbol_name: Some("NewClient".to_string()),
                symbol_kind: Some("func".to_string()),
                language: "go".to_string(),
                start_line: 1,
                end_line: 1,
                last_modified: 1700000000,
                vector: make_vector(3.0),
            },
        ];
        store.insert(rows).await.unwrap();

        let results = store.find_by_symbol("Server", None, 10).await.unwrap();
        assert_eq!(results.len(), 2);
        let names: Vec<_> = results
            .iter()
            .filter_map(|r| r.symbol_name.as_deref())
            .collect();
        assert!(names.contains(&"NewServer"));
        assert!(names.contains(&"Server.Start"));
    }

    #[tokio::test]
    async fn find_by_symbol_is_case_insensitive() {
        let tmp = TempDir::new().unwrap();
        let store = VectorStore::new(tmp.path().to_str().unwrap())
            .await
            .unwrap();

        let rows = vec![ChunkRow {
            file_path: "handler.go".to_string(),
            chunk_id: 0,
            content: "func HandleRequest() {}".to_string(),
            symbol_name: Some("HandleRequest".to_string()),
            symbol_kind: Some("func".to_string()),
            language: "go".to_string(),
            start_line: 1,
            end_line: 1,
            last_modified: 1700000000,
            vector: make_vector(1.0),
        }];
        store.insert(rows).await.unwrap();

        let results = store
            .find_by_symbol("handlerequest", None, 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].symbol_name.as_deref(), Some("HandleRequest"));
    }

    #[tokio::test]
    async fn find_by_symbol_with_kind_filter() {
        let tmp = TempDir::new().unwrap();
        let store = VectorStore::new(tmp.path().to_str().unwrap())
            .await
            .unwrap();

        let rows = vec![
            ChunkRow {
                file_path: "types.go".to_string(),
                chunk_id: 0,
                content: "type Server struct {}".to_string(),
                symbol_name: Some("Server".to_string()),
                symbol_kind: Some("type".to_string()),
                language: "go".to_string(),
                start_line: 1,
                end_line: 1,
                last_modified: 1700000000,
                vector: make_vector(1.0),
            },
            ChunkRow {
                file_path: "funcs.go".to_string(),
                chunk_id: 0,
                content: "func NewServer() {}".to_string(),
                symbol_name: Some("NewServer".to_string()),
                symbol_kind: Some("func".to_string()),
                language: "go".to_string(),
                start_line: 1,
                end_line: 1,
                last_modified: 1700000000,
                vector: make_vector(2.0),
            },
        ];
        store.insert(rows).await.unwrap();

        let results = store
            .find_by_symbol("Server", Some("type"), 10)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].symbol_kind.as_deref(), Some("type"));
    }

    #[tokio::test]
    async fn find_by_symbol_empty_on_no_match() {
        let tmp = TempDir::new().unwrap();
        let store = VectorStore::new(tmp.path().to_str().unwrap())
            .await
            .unwrap();

        let rows = vec![sample_row(
            "a.go",
            0,
            "func alpha() {}",
            "go",
            make_vector(1.0),
        )];
        store.insert(rows).await.unwrap();

        let results = store.find_by_symbol("nonexistent", None, 10).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn find_by_symbol_skips_null_names() {
        let tmp = TempDir::new().unwrap();
        let store = VectorStore::new(tmp.path().to_str().unwrap())
            .await
            .unwrap();

        let rows = vec![ChunkRow {
            file_path: "test.go".to_string(),
            chunk_id: 0,
            content: "package main".to_string(),
            symbol_name: None,
            symbol_kind: None,
            language: "go".to_string(),
            start_line: 1,
            end_line: 1,
            last_modified: 1700000000,
            vector: make_vector(1.0),
        }];
        store.insert(rows).await.unwrap();

        let results = store.find_by_symbol("main", None, 10).await.unwrap();
        assert!(results.is_empty());
    }

    // ---------------------------------------------------------------
    // list_files tests
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn list_files_returns_unique_paths() {
        let tmp = TempDir::new().unwrap();
        let store = VectorStore::new(tmp.path().to_str().unwrap())
            .await
            .unwrap();

        let rows = vec![
            sample_row("main.go", 0, "package main", "go", make_vector(1.0)),
            sample_row("main.go", 1, "func main() {}", "go", make_vector(2.0)),
            sample_row("util.go", 0, "func util() {}", "go", make_vector(3.0)),
            sample_row("server.go", 0, "func serve() {}", "go", make_vector(4.0)),
        ];
        store.insert(rows).await.unwrap();

        let files = store.list_files(None).await.unwrap();
        assert_eq!(files.len(), 3);
        assert!(files.contains(&"main.go".to_string()));
        assert!(files.contains(&"util.go".to_string()));
        assert!(files.contains(&"server.go".to_string()));
    }

    #[tokio::test]
    async fn list_files_sorted() {
        let tmp = TempDir::new().unwrap();
        let store = VectorStore::new(tmp.path().to_str().unwrap())
            .await
            .unwrap();

        let rows = vec![
            sample_row("z.go", 0, "func z() {}", "go", make_vector(1.0)),
            sample_row("a.go", 0, "func a() {}", "go", make_vector(2.0)),
            sample_row("m.go", 0, "func m() {}", "go", make_vector(3.0)),
        ];
        store.insert(rows).await.unwrap();

        let files = store.list_files(None).await.unwrap();
        assert_eq!(files, vec!["a.go", "m.go", "z.go"]);
    }

    #[tokio::test]
    async fn list_files_with_language_filter() {
        let tmp = TempDir::new().unwrap();
        let store = VectorStore::new(tmp.path().to_str().unwrap())
            .await
            .unwrap();

        let rows = vec![
            sample_row("main.go", 0, "func main() {}", "go", make_vector(1.0)),
            sample_row("lib.rs", 0, "fn lib() {}", "rust", make_vector(2.0)),
        ];
        store.insert(rows).await.unwrap();

        let go_files = store.list_files(Some("go")).await.unwrap();
        assert_eq!(go_files, vec!["main.go"]);

        let rust_files = store.list_files(Some("rust")).await.unwrap();
        assert_eq!(rust_files, vec!["lib.rs"]);
    }

    #[tokio::test]
    async fn list_files_empty_store() {
        let tmp = TempDir::new().unwrap();
        let store = VectorStore::new(tmp.path().to_str().unwrap())
            .await
            .unwrap();

        let files = store.list_files(None).await.unwrap();
        assert!(files.is_empty());
    }

    // ---------------------------------------------------------------
    // Persistence tests
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn data_survives_reopen() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().to_str().unwrap();

        {
            let store = VectorStore::new(path).await.unwrap();
            let rows = vec![
                sample_row("a.go", 0, "func a() {}", "go", make_vector(1.0)),
                sample_row("b.go", 0, "func b() {}", "go", make_vector(2.0)),
            ];
            store.insert(rows).await.unwrap();
            assert_eq!(store.chunk_count().await.unwrap(), 2);
        }

        // Reopen the store from the same path
        let store = VectorStore::new(path).await.unwrap();
        assert_eq!(store.chunk_count().await.unwrap(), 2);

        // Search should still work
        let results = store.search(&make_vector(1.0), 1, None).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "func a() {}");
    }
}
