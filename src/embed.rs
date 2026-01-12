//! Embedding model with baked-in weights.

use fastembed::{
    Pooling, QuantizationMode, TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel,
};

pub type Embedding = Vec<f32>;

pub struct Embedder {
    model: TextEmbedding,
}

#[allow(dead_code)]
impl Embedder {
    pub fn new() -> Result<Self, anyhow::Error> {
        #[cfg(target_arch = "aarch64")]
        let onnx_bytes = include_bytes!("../models/all-MiniLM-L6-v2/model-arm64.onnx");
        #[cfg(not(target_arch = "aarch64"))]
        let onnx_bytes = include_bytes!("../models/all-MiniLM-L6-v2/model.onnx");

        let model_data = UserDefinedEmbeddingModel {
            onnx_file: onnx_bytes.to_vec(),
            tokenizer_files: TokenizerFiles {
                tokenizer_file: include_bytes!("../models/all-MiniLM-L6-v2/tokenizer.json")
                    .to_vec(),
                config_file: include_bytes!("../models/all-MiniLM-L6-v2/config.json").to_vec(),
                special_tokens_map_file: include_bytes!(
                    "../models/all-MiniLM-L6-v2/special_tokens_map.json"
                )
                .to_vec(),
                tokenizer_config_file: include_bytes!(
                    "../models/all-MiniLM-L6-v2/tokenizer_config.json"
                )
                .to_vec(),
            },
            pooling: Some(Pooling::Mean),
            quantization: QuantizationMode::None,
            output_key: Default::default(),
        };

        let model = TextEmbedding::try_new_from_user_defined(model_data, Default::default())?;
        Ok(Self { model })
    }

    pub fn embed(&mut self, texts: &[String]) -> Result<Vec<Embedding>, anyhow::Error> {
        let texts_ref: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        let embeddings = self.model.embed(texts_ref, None)?;
        Ok(embeddings)
    }

    pub fn embed_one(&mut self, text: &str) -> Result<Embedding, anyhow::Error> {
        let embeddings = self.model.embed(vec![text], None)?;
        Ok(embeddings.into_iter().next().unwrap())
    }
}
