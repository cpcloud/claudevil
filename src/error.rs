use std::path::PathBuf;

/// Top-level error type for claudevil.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to download model from Hugging Face Hub")]
    ModelDownload(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("failed to load embedding model")]
    ModelLoad(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("tokenization failed")]
    Tokenize(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("embedding inference failed")]
    Inference(#[source] candle_core::Error),

    #[error("embedding returned no results")]
    EmptyEmbedding,

    #[error("vector store I/O error: {context}")]
    StoreIo {
        context: String,
        #[source]
        source: std::io::Error,
    },

    #[error("vector index error: {0}")]
    StoreIndex(String),

    #[error("metadata serialization error")]
    StoreSerde(#[source] serde_json::Error),

    #[error("could not read file: {}", path.display())]
    FileRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("config error: {0}")]
    Config(String),

    #[error("tree-sitter error: {0}")]
    TreeSitter(String),

    #[error("{0}")]
    Io(#[from] std::io::Error),

    #[error("background task panicked")]
    TaskJoin(#[from] tokio::task::JoinError),
}

pub type Result<T> = std::result::Result<T, Error>;
