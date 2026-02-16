# claudevil

**Semantic code search. One binary. Zero bullshit.**

Stop dumping your entire codebase into context and praying. claudevil indexes your code locally, understands it semantically, and hands your AI exactly the chunks it needs — nothing more.

## What it is

An MCP server that gives Claude Code actual code retrieval instead of dumb `grep`. Point it at a directory. It chunks your source files by language-aware boundaries using tree-sitter (functions, types, classes, traits — not random line splits), embeds them with all-MiniLM-L6-v2, and serves vector search over stdio. Supports Go, Rust, and Python.

Your code never leaves your machine. Not even a little.

## Install

```sh
# cargo
cargo install --git https://github.com/cpcloud/claudevil

# nix
nix run github:cpcloud/claudevil -- /path/to/project
```

## Use

```sh
claudevil ~/src/myproject
```

```
INFO claudevil starting for: /home/you/src/myproject
INFO loading embedding model...
INFO embedding model ready
INFO indexing complete: 847 chunks stored
INFO MCP server starting on stdio
```

Add it to your MCP config. Forget about it. Watch your token bills drop.

### Claude Code

```json
{
  "mcpServers": {
    "claudevil": {
      "command": "claudevil",
      "args": ["/absolute/path/to/your/project"]
    }
  }
}
```

## How it works

1. **Walks your code** — finds source files, skips hidden dirs and non-code
2. **Chunks by declaration** — tree-sitter parses Go, Rust, and Python at the AST level (functions, types, traits, classes, methods)
3. **Embeds locally** — all-MiniLM-L6-v2 running on your CPU via candle. Pure Rust, no ONNX Runtime, no Python
4. **Stores in usearch** — HNSW vector index with JSON metadata sidecar, file-based, no separate process
5. **Serves over MCP** — `search` tool returns the N most relevant code chunks for any natural language query

## Stack

| what | how |
|------|-----|
| Language | Rust |
| MCP | rmcp (stdio transport) |
| Parsing | tree-sitter (native compiled grammars for Go, Rust, Python) |
| Embeddings | candle (all-MiniLM-L6-v2, pure Rust BERT inference) |
| Vector store | usearch (HNSW, C++ FFI, file-based) |

No Python. No Node. No Docker. No CUDA drivers. One binary.

## Development

```sh
# enter dev shell (requires nix with flakes)
nix develop

# build + test
cargo build
cargo test
cargo clippy -- -D warnings

# build via nix
nix build '.#claudevil'

# run the docs site locally
nix run '.#site'

# security scanning
nix run '.#trufflehog'
nix run '.#audit'

# test coverage
cargo llvm-cov
```

## License

MIT

---

Built with spite and Rust.
