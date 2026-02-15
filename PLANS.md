So to your implicit question: there might actually still be room for claudevil — specifically a polished, single-binary, zero-dependency tool that "just works." The existing landscape is a bunch of Python/TypeScript projects requiring Docker, Ollama, Qdrant, or various runtime dependencies. A clean Go binary with embedded SQLite and a built-in embeddings model would be a genuinely differentiated take, if you're motivated to build it.Probably would use lance5:03 PMGood call. LanceDB is a much better fit than SQLite for this — it's purpose-built for vector search and has native Rust underpinnings, so it pairs well with a Go or Rust binary.Searched the webLanceDB Go SDK embedding search10 resultslancedb package - github.com/lancedb/lancedb-go/pkg/lancedb - Go Packagespkg.go.devManaging Embeddings - LanceDBdocs.lancedb.comGitHub - lancedb/lancedb: Developer-friendly OSS embedded retrieval library for multimodal AI. Search More; Manage Less.github.comLanceDB | Vector Database for RAG, Agents & Hybrid Searchlancedb.comMultitask Embedding with LanceDBblog.lancedb.comlancedb/docs/src/embeddings/index.md at main · lancedb/lancedbgithub.comLanceDB · GitHubgithub.comBuilding a Fully Local RAG API with LanceDB and FastEmbed | by Pvanand | Mediummedium.comBuilding an Image Search Application with LanceDB | by Reflections on AI | Mediummedium.comBottomless vector database storage with Tigris and LanceDB | Tigris Object Storagewww.tigrisdata.comDoneLance is a great fit. There's an official Go SDK (lancedb-go) that provides CGO bindings to the Rust core library Go Packages, so you'd get the Rust performance with Go ergonomics. It's embedded (no separate server process), file-based, and supports vector search out of the box.
The stack for claudevil would be pretty clean:

Go binary with the LanceDB Go SDK (CGO → Rust core under the hood)
Lance for storage + vector search — just a directory on disk, no Docker, no server
An embedded embedding model (or call out to Ollama if available, fall back to a small ONNX model bundled in the binary)
fsnotify for file watching
MCP via the official Go SDK over stdio

The one wrinkle is the embedding model. To be truly zero-dependency you'd want to bundle something like all-MiniLM-L6-v2 via ONNX Runtime in Go, which is doable but adds to binary size (~80-90MB). The alternative is requiring Ollama or an API key, which defeats the "just works" goal.
Want to start speccing this out more concretely, or are you still in the "thinking about whether to build it" phase?easier to build with rust since lance is rust?5:29 PMYeah, that's the right instinct. Since Lance's core is Rust, going with Rust eliminates the CGO boundary entirely — you'd just use the lancedb crate directly. No FFI overhead, no cross-compilation headaches with CGO, and you get native access to the full API without waiting for Go bindings to catch up.
The same logic applies to the embedding model — the ort crate (ONNX Runtime for Rust) is mature, and there are crates like fastembed-rs that wrap it for exactly this use case. You could bundle all-MiniLM-L6-v2 as an ONNX model and run inference natively.
The tradeoff is that the MCP SDK story is a bit noisier in Rust — there's the official rmcp crate and the community rust-mcp-sdk, both functional but newer than the Go SDK. And Rust will be slower to iterate on if you're prototyping. But for a daemon that you want to compile once and forget about, Rust is arguably the better long-term choice.
The full stack would be:

rmcp — MCP server (stdio transport for Claude Code)
lancedb crate — embedded vector storage, no separate process
ort / fastembed-rs — embedded ONNX inference for embeddings
notify crate — cross-platform file watching
tokio — async runtime

Single binary, zero runtime dependencies, just point it at a directory and go. That's a meaningfully better developer experience than anything in the current MCP RAG landscape.
Want to start on it?
