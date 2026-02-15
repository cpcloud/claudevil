use std::sync::Arc;

use arrow_array::types::Float32Type;
use arrow_array::{
    Array, FixedSizeListArray, Int64Array, RecordBatch, RecordBatchIterator, StringArray,
};
use arrow_schema::{DataType, Field, Schema};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use lancedb::{Connection, DistanceType, Table, connect};
use tokio::sync::RwLock;

use crate::embed::EMBEDDING_DIM;
use crate::error::{Error, Result};

const TABLE_NAME: &str = "code_chunks";

fn chunk_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("file_path", DataType::Utf8, false),
        Field::new("chunk_id", DataType::Int64, false),
        Field::new("content", DataType::Utf8, false),
        Field::new("symbol_name", DataType::Utf8, true),
        Field::new("symbol_kind", DataType::Utf8, true),
        Field::new("package_name", DataType::Utf8, true),
        Field::new("language", DataType::Utf8, false),
        Field::new("start_line", DataType::Int64, false),
        Field::new("end_line", DataType::Int64, false),
        Field::new("last_modified", DataType::Int64, false),
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                EMBEDDING_DIM as i32,
            ),
            true,
        ),
    ]))
}

/// A row ready to be inserted into the vector store.
pub struct ChunkRow {
    pub file_path: String,
    pub chunk_id: i64,
    pub content: String,
    pub symbol_name: Option<String>,
    pub symbol_kind: Option<String>,
    pub package_name: Option<String>,
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
    db: Connection,
    table: Arc<RwLock<Option<Table>>>,
}

impl VectorStore {
    pub async fn new(path: &str) -> Result<Self> {
        let db = connect(path)
            .execute()
            .await
            .map_err(|e| Error::StoreConnect {
                path: path.to_string(),
                source: e,
            })?;

        let table = db.open_table(TABLE_NAME).execute().await.ok();

        Ok(Self {
            db,
            table: Arc::new(RwLock::new(table)),
        })
    }

    /// Ensure the table exists, creating it if necessary.
    async fn ensure_table(&self) -> Result<Table> {
        {
            let guard = self.table.read().await;
            if let Some(ref t) = *guard {
                return Ok(t.clone());
            }
        }

        let mut guard = self.table.write().await;
        if let Some(ref t) = *guard {
            return Ok(t.clone());
        }

        let schema = chunk_schema();
        let table = self
            .db
            .create_empty_table(TABLE_NAME, schema)
            .execute()
            .await
            .map_err(|e| Error::StoreCreateTable {
                table: TABLE_NAME.to_string(),
                source: e,
            })?;
        *guard = Some(table.clone());
        Ok(table)
    }

    /// Insert a batch of chunk rows.
    pub async fn insert(&self, rows: Vec<ChunkRow>) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }

        let table = self.ensure_table().await?;
        let schema = chunk_schema();

        let file_paths: Vec<&str> = rows.iter().map(|r| r.file_path.as_str()).collect();
        let chunk_ids: Vec<i64> = rows.iter().map(|r| r.chunk_id).collect();
        let contents: Vec<&str> = rows.iter().map(|r| r.content.as_str()).collect();
        let symbol_names: Vec<Option<&str>> =
            rows.iter().map(|r| r.symbol_name.as_deref()).collect();
        let symbol_kinds: Vec<Option<&str>> =
            rows.iter().map(|r| r.symbol_kind.as_deref()).collect();
        let package_names: Vec<Option<&str>> =
            rows.iter().map(|r| r.package_name.as_deref()).collect();
        let languages: Vec<&str> = rows.iter().map(|r| r.language.as_str()).collect();
        let start_lines: Vec<i64> = rows.iter().map(|r| r.start_line).collect();
        let end_lines: Vec<i64> = rows.iter().map(|r| r.end_line).collect();
        let last_modifieds: Vec<i64> = rows.iter().map(|r| r.last_modified).collect();

        let vectors: Vec<Option<Vec<Option<f32>>>> = rows
            .iter()
            .map(|r| Some(r.vector.iter().copied().map(Some).collect()))
            .collect();

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(file_paths)),
                Arc::new(Int64Array::from(chunk_ids)),
                Arc::new(StringArray::from(contents)),
                Arc::new(StringArray::from(symbol_names)),
                Arc::new(StringArray::from(symbol_kinds)),
                Arc::new(StringArray::from(package_names)),
                Arc::new(StringArray::from(languages)),
                Arc::new(Int64Array::from(start_lines)),
                Arc::new(Int64Array::from(end_lines)),
                Arc::new(Int64Array::from(last_modifieds)),
                Arc::new(
                    FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
                        vectors,
                        EMBEDDING_DIM as i32,
                    ),
                ),
            ],
        )
        .map_err(Error::ArrowBatch)?;

        let batches = RecordBatchIterator::new(vec![Ok(batch)], schema);
        table
            .add(batches)
            .execute()
            .await
            .map_err(Error::StoreInsert)?;

        Ok(())
    }

    /// Semantic search by vector similarity.
    pub async fn search(
        &self,
        query_vec: &[f32],
        limit: usize,
        language_filter: Option<&str>,
    ) -> Result<Vec<SearchResult>> {
        let table = match *self.table.read().await {
            Some(ref t) => t.clone(),
            None => return Ok(Vec::new()),
        };

        let mut query = table.vector_search(query_vec).map_err(Error::StoreSearch)?;
        query = query
            .distance_type(DistanceType::Cosine)
            .limit(limit)
            .select(Select::columns(&[
                "file_path",
                "content",
                "symbol_name",
                "symbol_kind",
                "start_line",
                "end_line",
            ]));

        if let Some(lang) = language_filter {
            query = query.only_if(format!("language = '{lang}'"));
        }

        let batches: Vec<RecordBatch> = query
            .execute()
            .await
            .map_err(Error::StoreSearch)?
            .try_collect()
            .await
            .map_err(Error::StoreSearch)?;

        let mut results = Vec::new();
        for batch in &batches {
            let paths = batch
                .column_by_name("file_path")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let contents = batch
                .column_by_name("content")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let sym_names = batch
                .column_by_name("symbol_name")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let sym_kinds = batch
                .column_by_name("symbol_kind")
                .and_then(|c| c.as_any().downcast_ref::<StringArray>());
            let start_lines = batch
                .column_by_name("start_line")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>());
            let end_lines = batch
                .column_by_name("end_line")
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>());
            let distances = batch
                .column_by_name("_distance")
                .and_then(|c| c.as_any().downcast_ref::<arrow_array::Float32Array>());

            let (Some(paths), Some(contents), Some(start_lines), Some(end_lines)) =
                (paths, contents, start_lines, end_lines)
            else {
                continue;
            };

            for row in 0..batch.num_rows() {
                results.push(SearchResult {
                    file_path: paths.value(row).to_string(),
                    content: contents.value(row).to_string(),
                    symbol_name: sym_names.and_then(|a| {
                        if a.is_null(row) {
                            None
                        } else {
                            Some(a.value(row).to_string())
                        }
                    }),
                    symbol_kind: sym_kinds.and_then(|a| {
                        if a.is_null(row) {
                            None
                        } else {
                            Some(a.value(row).to_string())
                        }
                    }),
                    start_line: start_lines.value(row),
                    end_line: end_lines.value(row),
                    distance: distances.map_or(0.0, |d| d.value(row)),
                });
            }
        }

        Ok(results)
    }

    /// Delete all chunks for a given file path.
    pub async fn delete_file(&self, file_path: &str) -> Result<()> {
        let guard = self.table.read().await;
        if let Some(ref table) = *guard {
            table
                .delete(&format!("file_path = '{file_path}'"))
                .await
                .map_err(|e| Error::StoreDelete {
                    path: file_path.to_string(),
                    source: e,
                })?;
        }
        Ok(())
    }

    /// Count total indexed chunks.
    pub async fn chunk_count(&self) -> Result<u64> {
        let guard = self.table.read().await;
        match *guard {
            Some(ref table) => {
                let count = table.count_rows(None).await.map_err(Error::StoreCount)?;
                Ok(count as u64)
            }
            None => Ok(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_vector(seed: f32) -> Vec<f32> {
        // Create a normalized vector with a distinguishable direction.
        // Each seed produces a different unit vector by placing weight at different positions.
        let mut v = vec![0.0f32; EMBEDDING_DIM];
        let idx = (seed.abs() as usize) % EMBEDDING_DIM;
        v[idx] = 1.0;
        // Add some noise so vectors aren't perfectly orthogonal
        for (i, val) in v.iter_mut().enumerate() {
            *val += (i as f32 * seed * 0.001).sin() * 0.01;
        }
        // L2 normalize
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
            package_name: Some("main".to_string()),
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

        // Insert 3 chunks with distinct vectors
        let v1 = make_vector(1.0);
        let v2 = make_vector(50.0);
        let v3 = make_vector(100.0);

        let rows = vec![
            sample_row("a.go", 0, "func alpha() {}", "go", v1.clone()),
            sample_row("b.go", 0, "func beta() {}", "go", v2.clone()),
            sample_row("c.go", 0, "func gamma() {}", "go", v3),
        ];
        store.insert(rows).await.unwrap();

        // Search with v1 — the closest result should be the alpha chunk
        let results = store.search(&v1, 3, None).await.unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].content, "func alpha() {}");
        // The closest match should have the smallest distance
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

        // Search should only return chunks from keep.go
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

        // Filter to only Go
        let results = store
            .search(&make_vector(1.0), 10, Some("go"))
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "func goFunc() {}");

        // Filter to only Rust
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
            package_name: Some("api".to_string()),
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

        // First batch
        let rows1 = vec![sample_row("a.go", 0, "func a() {}", "go", make_vector(1.0))];
        store.insert(rows1).await.unwrap();

        // Second batch
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

        // Deleting a file that doesn't exist should not error
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
            package_name: None,
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
}
