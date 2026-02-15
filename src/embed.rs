use std::sync::Arc;

use candle_core::{Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config, DTYPE};
use hf_hub::api::sync::Api;
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

use crate::error::{Error, Result};

/// Embedding dimension for all-MiniLM-L6-v2.
pub const EMBEDDING_DIM: usize = 384;

const MODEL_ID: &str = "sentence-transformers/all-MiniLM-L6-v2";
const MAX_SEQ_LEN: usize = 512;

/// Pure-Rust sentence embedder using candle.
///
/// Loads all-MiniLM-L6-v2 from Hugging Face Hub on first use, then runs
/// BERT inference entirely in Rust — no C++ or ONNX Runtime dependency.
#[derive(Clone)]
pub struct Embedder {
    model: Arc<BertModel>,
    tokenizer: Arc<Tokenizer>,
    device: Device,
}

impl Embedder {
    /// Initialize the embedding model.
    ///
    /// Downloads model weights from Hugging Face Hub on first use (~80 MB,
    /// cached in `~/.cache/huggingface/hub`).
    pub fn new() -> Result<Self> {
        let device = Device::Cpu;

        // Download model files from Hugging Face Hub
        let api = Api::new().map_err(|e| Error::ModelDownload(e.into()))?;
        let repo = api.model(MODEL_ID.to_string());

        let config_path = repo
            .get("config.json")
            .map_err(|e| Error::ModelDownload(e.into()))?;
        let tokenizer_path = repo
            .get("tokenizer.json")
            .map_err(|e| Error::ModelDownload(e.into()))?;
        let weights_path = repo
            .get("model.safetensors")
            .map_err(|e| Error::ModelDownload(e.into()))?;

        // Load config
        let config: Config = serde_json::from_str(&std::fs::read_to_string(&config_path)?)
            .map_err(|e| Error::ModelLoad(e.into()))?;

        // Load model weights (memory-mapped for efficiency)
        // SAFETY: The model file is read-only and will not be modified while mapped.
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[weights_path], DTYPE, &device)
                .map_err(|e| Error::ModelLoad(e.into()))?
        };
        let model = BertModel::load(vb, &config).map_err(|e| Error::ModelLoad(e.into()))?;

        // Load and configure tokenizer with padding + truncation
        let mut tokenizer = Tokenizer::from_file(&tokenizer_path).map_err(Error::ModelLoad)?;

        tokenizer.with_padding(Some(PaddingParams {
            strategy: PaddingStrategy::BatchLongest,
            ..Default::default()
        }));
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: MAX_SEQ_LEN,
                ..Default::default()
            }))
            .map_err(Error::ModelLoad)?;

        Ok(Self {
            model: Arc::new(model),
            tokenizer: Arc::new(tokenizer),
            device,
        })
    }

    /// Embed a batch of texts, returning one 384-dim vector per input.
    pub async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let embedder = self.clone();
        tokio::task::spawn_blocking(move || embedder.embed_batch_sync(&texts)).await?
    }

    /// Embed a single text.
    pub async fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        let text = text.to_string();
        let mut results = self.embed_batch(vec![text]).await?;
        results.pop().ok_or(Error::EmptyEmbedding)
    }

    fn embed_batch_sync(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        let encodings = self
            .tokenizer
            .encode_batch(refs, true)
            .map_err(Error::Tokenize)?;

        self.forward(&encodings).map_err(Error::Inference)
    }

    /// Run BERT forward pass, then mean-pool and L2-normalize.
    fn forward(&self, encodings: &[tokenizers::Encoding]) -> candle_core::Result<Vec<Vec<f32>>> {
        let token_ids: Vec<Tensor> = encodings
            .iter()
            .map(|e| Tensor::new(e.get_ids(), &self.device))
            .collect::<candle_core::Result<Vec<_>>>()?;

        let attention_masks: Vec<Tensor> = encodings
            .iter()
            .map(|e| Tensor::new(e.get_attention_mask(), &self.device))
            .collect::<candle_core::Result<Vec<_>>>()?;

        let token_ids = Tensor::stack(&token_ids, 0)?;
        let attention_mask = Tensor::stack(&attention_masks, 0)?;
        let token_type_ids = token_ids.zeros_like()?;

        // BERT forward pass -> [batch, seq_len, hidden_size]
        let embeddings = self
            .model
            .forward(&token_ids, &token_type_ids, Some(&attention_mask))?;

        // Mean pooling with attention mask
        let mask = attention_mask.unsqueeze(2)?.to_dtype(DTYPE)?;
        let pooled = embeddings.broadcast_mul(&mask)?.sum(1)?;
        let counts = mask.sum(1)?;
        let pooled = pooled.broadcast_div(&counts)?;

        // L2 normalization
        let norms = pooled.sqr()?.sum_keepdim(1)?.sqrt()?;
        let normalized = pooled.broadcast_div(&norms)?;

        normalized.to_vec2::<f32>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        dot / (norm_a * norm_b)
    }

    #[tokio::test]
    async fn embedding_dimension_is_384() {
        let embedder = Embedder::new().unwrap();
        let vec = embedder.embed_one("hello world").await.unwrap();
        assert_eq!(vec.len(), EMBEDDING_DIM);
    }

    #[tokio::test]
    async fn similar_texts_have_higher_similarity() {
        let embedder = Embedder::new().unwrap();

        let v_hello = embedder.embed_one("hello world").await.unwrap();
        let v_hi = embedder.embed_one("hi world").await.unwrap();
        let v_quantum = embedder
            .embed_one("quantum chromodynamics in lattice gauge theory")
            .await
            .unwrap();

        let sim_close = cosine_similarity(&v_hello, &v_hi);
        let sim_far = cosine_similarity(&v_hello, &v_quantum);

        assert!(
            sim_close > sim_far,
            "expected 'hello world' closer to 'hi world' ({sim_close:.4}) than to quantum physics ({sim_far:.4})"
        );
    }

    #[tokio::test]
    async fn batch_matches_individual() {
        let embedder = Embedder::new().unwrap();

        let texts = vec![
            "func main() {}".to_string(),
            "type Server struct {}".to_string(),
        ];

        let batch = embedder.embed_batch(texts.clone()).await.unwrap();
        let individual_0 = embedder.embed_one(&texts[0]).await.unwrap();
        let individual_1 = embedder.embed_one(&texts[1]).await.unwrap();

        // Batch and individual embeddings should be very close (not exact due to padding)
        let sim_0 = cosine_similarity(&batch[0], &individual_0);
        let sim_1 = cosine_similarity(&batch[1], &individual_1);

        assert!(
            sim_0 > 0.99,
            "batch[0] vs individual[0] similarity = {sim_0:.4}"
        );
        assert!(
            sim_1 > 0.99,
            "batch[1] vs individual[1] similarity = {sim_1:.4}"
        );
    }

    #[tokio::test]
    async fn empty_batch_returns_empty() {
        let embedder = Embedder::new().unwrap();
        let result = embedder.embed_batch(vec![]).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn embeddings_are_normalized() {
        let embedder = Embedder::new().unwrap();
        let vec = embedder
            .embed_one("func HandleRequest(w http.ResponseWriter, r *http.Request)")
            .await
            .unwrap();

        let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 0.01,
            "embedding should be L2-normalized, got norm = {norm:.4}"
        );
    }

    #[tokio::test]
    async fn code_semantic_similarity() {
        let embedder = Embedder::new().unwrap();

        // Two Go HTTP handlers should be more similar to each other than to a math function
        let v_handler1 = embedder
            .embed_one("func GetUser(w http.ResponseWriter, r *http.Request) { json.NewEncoder(w).Encode(user) }")
            .await.unwrap();
        let v_handler2 = embedder
            .embed_one("func ListUsers(w http.ResponseWriter, r *http.Request) { json.NewEncoder(w).Encode(users) }")
            .await.unwrap();
        let v_math = embedder
            .embed_one("func Fibonacci(n int) int { if n <= 1 { return n } return Fibonacci(n-1) + Fibonacci(n-2) }")
            .await.unwrap();

        let sim_handlers = cosine_similarity(&v_handler1, &v_handler2);
        let sim_handler_math = cosine_similarity(&v_handler1, &v_math);

        assert!(
            sim_handlers > sim_handler_math,
            "HTTP handlers should be more similar ({sim_handlers:.4}) than handler vs math ({sim_handler_math:.4})"
        );
    }
}
