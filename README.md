# claudevil

**Semantic code search. One binary. Zero bullshit.**

Stop dumping your entire codebase into context and praying. claudevil indexes your code locally, understands it semantically, and hands your AI exactly the chunks it needs — nothing more.

## What it is

An MCP server that gives Claude Code actual code retrieval instead of dumb `grep`. Point it at a directory. It chunks your source files by language-aware boundaries (functions, types, interfaces — not random line splits), embeds them with all-MiniLM-L6-v2, and serves vector search over stdio.

Your code never leaves your machine. Not even a little.

## Install

```sh
# nix (recommended)
nix run github:claudevil/claudevil -- /path/to/project

# from source
cargo install --git https://github.com/claudevil/claudevil
```

Pre-built static binaries for Linux (musl), macOS (x86_64 + arm64), and Windows are on the [releases page](https://github.com/claudevil/claudevil/releases).

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
2. **Chunks by declaration** — functions, methods, types, interfaces, consts, vars (Go today, more languages coming)
3. **Embeds locally** — all-MiniLM-L6-v2 running on your CPU via candle. Pure Rust, no ONNX Runtime, no Python
4. **Stores in LanceDB** — embedded vector database, file-based, no separate process
5. **Serves over MCP** — `search` tool returns the N most relevant code chunks for any natural language query

## Stack

| what | how |
|------|-----|
| Language | Rust |
| MCP | rmcp (stdio transport) |
| Embeddings | candle (all-MiniLM-L6-v2, pure Rust BERT inference) |
| Vector store | LanceDB (embedded, file-based) |
| Binary | Static musl on Linux, universal on macOS |

No Python. No Node. No Docker. No CUDA drivers. No runtime dependencies. One binary.

## Development

```sh
# enter dev shell (requires nix with flakes)
nix develop

# build + test
cargo build
cargo test
cargo clippy -- -D warnings

# static musl binary
nix build .#claudevil

# run the docs site locally
nix run .#site

# security scanning
nix run .#trufflehog
nix run .#audit

# test coverage
cargo llvm-cov
```

## License

MIT

---

Built with spite and Rust.
