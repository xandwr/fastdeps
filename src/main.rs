use fastembed::{
    Pooling, QuantizationMode, TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel,
};

fn main() {
    // Bake model into binary at compile time (~23MB)
    #[cfg(target_arch = "aarch64")]
    let onnx_bytes = include_bytes!("../models/all-MiniLM-L6-v2/model-arm64.onnx");
    #[cfg(not(target_arch = "aarch64"))]
    let onnx_bytes = include_bytes!("../models/all-MiniLM-L6-v2/model.onnx");

    let model_data = UserDefinedEmbeddingModel {
        onnx_file: onnx_bytes.to_vec(),
        tokenizer_files: TokenizerFiles {
            tokenizer_file: include_bytes!("../models/all-MiniLM-L6-v2/tokenizer.json").to_vec(),
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
        output_key: Default::default(), // OnlyOne - model has single output
    };

    let mut model = TextEmbedding::try_new_from_user_defined(model_data, Default::default())
        .expect("Failed to load embedding model");

    // Test it
    let texts = vec![
        "spawn children in bevy",
        "ChildSpawnerCommands",
        "EntityCommands with_children",
        "serialize json data",
        "serde Serialize trait",
    ];

    let embeddings = model.embed(texts.clone(), None).expect("Embedding failed");

    println!("Generated {} embeddings of dimension {}", embeddings.len(), embeddings[0].len());

    // Show similarity matrix
    println!("\nSimilarity matrix:");
    print!("{:30}", "");
    for (i, _) in texts.iter().enumerate() {
        print!("{:>6}", i);
    }
    println!();

    for (i, t1) in texts.iter().enumerate() {
        print!("{:30}", if t1.len() > 28 { &t1[..28] } else { t1 });
        for (j, _) in texts.iter().enumerate() {
            let sim = cosine_similarity(&embeddings[i], &embeddings[j]);
            print!("{:6.2}", sim);
        }
        println!();
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (norm_a * norm_b)
}
