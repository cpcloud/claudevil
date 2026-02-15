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

    #[error("failed to connect to vector store at {path}")]
    StoreConnect {
        path: String,
        #[source]
        source: lancedb::Error,
    },

    #[error("failed to create table '{table}'")]
    StoreCreateTable {
        table: String,
        #[source]
        source: lancedb::Error,
    },

    #[error("failed to insert chunks into vector store")]
    StoreInsert(#[source] lancedb::Error),

    #[error("vector search failed")]
    StoreSearch(#[source] lancedb::Error),

    #[error("failed to delete chunks for '{path}'")]
    StoreDelete {
        path: String,
        #[source]
        source: lancedb::Error,
    },

    #[error("failed to count rows")]
    StoreCount(#[source] lancedb::Error),

    #[error("failed to build record batch")]
    ArrowBatch(#[source] arrow_schema::ArrowError),

    #[error("could not read file: {}", path.display())]
    FileRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{0}")]
    Io(#[from] std::io::Error),

    #[error("background task panicked")]
    TaskJoin(#[from] tokio::task::JoinError),
}

pub type Result<T> = std::result::Result<T, Error>;
