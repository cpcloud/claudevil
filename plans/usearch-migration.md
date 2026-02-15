# Replace lancedb with usearch

## Motivation

lancedb pulls in **605 transitive crates** (DataFusion, full Arrow suite,
object_store, Lance engine) yet we use only:

- Vector insert + ANN search (cosine)
- Metadata storage alongside vectors (file_path, content, symbol_name, etc.)
- SQL-like predicates for filtering and deletion

The `target/debug` directory is ~40 GB locally. The vast majority of compile
time and disk comes from DataFusion (297 crates) and the Lance engine (593
crates). We use none of the SQL query planning, cloud storage, or columnar
format features.

usearch is a C++ HNSW library with minimal Rust FFI bindings (~3 deps:
`cc`, `libc`, plus optional SIMD). It provides the vector index. Metadata
storage is handled with a serde_json-backed `HashMap<u64, ChunkMeta>` written
to a sidecar file.

## Design

### File layout on disk

```
{db_path}/
  index.usearch    -- usearch HNSW index (binary)
  metadata.json    -- JSON: { next_key: u64, chunks: { "key": ChunkMeta, ... } }
```

### Data model

```rust
struct ChunkMeta {
    file_path: String,
    chunk_id: i64,
    content: String,
    symbol_name: Option<String>,
    symbol_kind: Option<String>,
    package_name: Option<String>,
    language: String,
    start_line: i64,
    end_line: i64,
    last_modified: i64,
}

struct Metadata {
    next_key: u64,
    chunks: HashMap<u64, ChunkMeta>,
}
```

### VectorStore internals

```rust
struct VectorStore {
    index: Arc<RwLock<SendSyncIndex>>,
    meta: Arc<RwLock<Metadata>>,
    db_path: PathBuf,
}
```

`SendSyncIndex` is a newtype wrapping `usearch::Index` with
`unsafe impl Send` and `unsafe impl Sync`. This is sound because the C++
engine is internally thread-safe (documented in usearch).

### Operation mapping

| Current (lancedb)                      | New (usearch + JSON)                                                                    |
| -------------------------------------- | --------------------------------------------------------------------------------------- |
| `connect(path)` + `open_table`         | `Index::new(opts)` + load from file; deserialize metadata JSON                          |
| `create_empty_table(schema)`           | No-op (index + metadata created on first insert)                                        |
| `table.add(record_batches)`            | `index.add(key, vector)` per row + insert into metadata map; persist both               |
| `vector_search(vec).limit(n)`          | `index.search(vec, n)` or `index.filtered_search(vec, n, predicate)`                    |
| `.only_if("language = 'go'")`          | Post-filter via metadata lookup, or use `filtered_search` with a closure                |
| `table.query().only_if(f).select(...)` | In-memory scan of metadata HashMap                                                      |
| `table.delete("file_path = ...")`      | Scan metadata for matching keys, `index.remove(key)` each, remove from map; persist     |
| `table.count_rows()`                   | `meta.chunks.len()`                                                                     |

### Search with filters

usearch provides `filtered_search(query, limit, |key| bool)`. We use this
for language-filtered vector search: look up the key in the metadata map,
check the language field. This avoids over-fetching.

For `find_by_symbol` and `list_files`, no vector search is needed -- these
are pure metadata scans over the in-memory HashMap.

### Persistence strategy

After every mutation (insert batch, delete), persist both files:
1. `index.save(path)` -- usearch binary format
2. `serde_json::to_writer(file, &metadata)` -- human-readable JSON

On startup, load both if they exist. If neither exists, start empty.

### Thread safety

The `Index` wrapper is `Send + Sync` via unsafe impl. All access goes through
`Arc<RwLock<...>>`. Reads (search, count) take a read lock; writes (insert,
delete) take a write lock on both index and metadata.

### Error handling

Remove all `lancedb::Error` and `arrow_schema::ArrowError` variants from
`error.rs`. Add:
- `StoreIo` for file I/O errors during persist/load
- `StoreIndex` for usearch index operation errors (string-based, since usearch
  returns `String` errors)
- `StoreSerde` for JSON serialization errors

### Dependencies removed

- `lancedb` (605 transitive crates)
- `arrow-array`
- `arrow-schema`
- `futures` (only used for `TryStreamExt` on lancedb query streams)

### Dependencies added

- `usearch` (~3 transitive crates; requires C++ compiler at build time,
  already available in our nix dev shell and build derivation)

### Static musl binary

usearch's C++ code needs a C++ standard library at link time. The LLVM-based
build stdenv (`rustStdenv`) doesn't provide `libstdc++.a`, and the musl
cross-GCC puts it in `${crossSystem.config}/lib/` rather than `lib/`.

Solution: add `-L ${pkgs.stdenv.cc.cc}/${crossSystem.config}/lib` to
`RUSTFLAGS` so lld finds the cross-GCC's static `libstdc++.a`. No separate
LLVM package set needed.

For the dev shell, `LD_LIBRARY_PATH` still points at `libstdc++.so` since
dev builds are dynamically linked.

### Risks

- **Not Send+Sync by default**: Requires `unsafe impl`. Sound because the
  C++ library is thread-safe, but worth a comment.
- **No built-in persistence format migration**: If the usearch binary format
  changes between versions, the index file may not load. Mitigation:
  re-index from source files (already supported via the `reindex` MCP tool).
