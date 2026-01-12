//! Contrastive Mapping: 384D → 8D linear projection.
//!
//! Training: Learn a 384×8 matrix that maps all-MiniLM embeddings
//! of crate descriptions to their octonion characteristic vectors.
//!
//! The matrix is ~12KB (3,072 f32 values) and can be bundled into the binary.

use std::io::{Read, Write};

/// The embedding dimension from all-MiniLM-L6-v2.
pub const EMBED_DIM: usize = 384;

/// The target octonion dimension.
pub const OCTO_DIM: usize = 8;

/// A 384×8 linear projection matrix.
/// Row-major: weights[i][j] maps embedding[i] to output[j].
#[derive(Clone)]
pub struct ContrastiveMapper {
    /// The projection matrix: 384 rows × 8 columns.
    pub weights: [[f32; OCTO_DIM]; EMBED_DIM],
    /// Optional bias term for each output dimension.
    pub bias: [f32; OCTO_DIM],
}

impl ContrastiveMapper {
    /// Create a new mapper with random initialization.
    pub fn new_random() -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;

        // Simple LCG for reproducible random init
        let mut rng_state = seed;
        let mut rand_f32 = || {
            rng_state = rng_state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let bits = (rng_state >> 33) as u32;
            (bits as f32 / u32::MAX as f32 - 0.5) * 0.1 // Small random values [-0.05, 0.05]
        };

        let mut weights = [[0.0f32; OCTO_DIM]; EMBED_DIM];
        for row in weights.iter_mut() {
            for val in row.iter_mut() {
                *val = rand_f32();
            }
        }

        Self {
            weights,
            bias: [0.0; OCTO_DIM],
        }
    }

    /// Create a mapper initialized to zero (for loading).
    pub fn new_zeros() -> Self {
        Self {
            weights: [[0.0; OCTO_DIM]; EMBED_DIM],
            bias: [0.0; OCTO_DIM],
        }
    }

    /// Forward pass: project a 384D embedding to 8D.
    /// O(384 × 8) = O(3072) operations.
    pub fn forward(&self, embedding: &[f32]) -> [f32; OCTO_DIM] {
        assert_eq!(embedding.len(), EMBED_DIM, "Expected 384D embedding");

        let mut output = self.bias;

        for (i, &e) in embedding.iter().enumerate() {
            for (j, out) in output.iter_mut().enumerate() {
                *out += e * self.weights[i][j];
            }
        }

        // Apply sigmoid to clamp to [0, 1] range like octonion coefficients
        for out in output.iter_mut() {
            *out = sigmoid(*out);
        }

        output
    }

    /// Compute loss (MSE) for a batch of (embedding, target) pairs.
    pub fn compute_loss(&self, embeddings: &[Vec<f32>], targets: &[[f32; OCTO_DIM]]) -> f32 {
        let n = embeddings.len() as f32;
        let mut total_loss = 0.0;

        for (emb, target) in embeddings.iter().zip(targets.iter()) {
            let pred = self.forward(emb);
            for (p, t) in pred.iter().zip(target.iter()) {
                total_loss += (p - t).powi(2);
            }
        }

        total_loss / (n * OCTO_DIM as f32)
    }

    /// Train the mapper using gradient descent.
    /// Returns final loss.
    pub fn train(
        &mut self,
        embeddings: &[Vec<f32>],
        targets: &[[f32; OCTO_DIM]],
        learning_rate: f32,
        epochs: usize,
        verbose: bool,
    ) -> f32 {
        let n = embeddings.len();

        for epoch in 0..epochs {
            // Accumulate gradients
            let mut grad_weights = [[0.0f32; OCTO_DIM]; EMBED_DIM];
            let mut grad_bias = [0.0f32; OCTO_DIM];

            for (emb, target) in embeddings.iter().zip(targets.iter()) {
                // Forward pass
                let mut pre_sigmoid = self.bias;
                for (i, &e) in emb.iter().enumerate() {
                    for (j, out) in pre_sigmoid.iter_mut().enumerate() {
                        *out += e * self.weights[i][j];
                    }
                }

                let mut pred = pre_sigmoid;
                for p in pred.iter_mut() {
                    *p = sigmoid(*p);
                }

                // Backward pass: d_loss/d_pred = 2 * (pred - target) / (n * 8)
                // d_pred/d_pre_sigmoid = sigmoid * (1 - sigmoid)
                // d_pre_sigmoid/d_weight[i][j] = embedding[i]

                for j in 0..OCTO_DIM {
                    let d_loss_d_pred = 2.0 * (pred[j] - target[j]) / (n as f32 * OCTO_DIM as f32);
                    let d_pred_d_pre = pred[j] * (1.0 - pred[j]); // sigmoid derivative
                    let d_loss_d_pre = d_loss_d_pred * d_pred_d_pre;

                    grad_bias[j] += d_loss_d_pre;

                    for (i, &e) in emb.iter().enumerate() {
                        grad_weights[i][j] += d_loss_d_pre * e;
                    }
                }
            }

            // Update weights
            for i in 0..EMBED_DIM {
                for j in 0..OCTO_DIM {
                    self.weights[i][j] -= learning_rate * grad_weights[i][j];
                }
            }
            for j in 0..OCTO_DIM {
                self.bias[j] -= learning_rate * grad_bias[j];
            }

            if verbose && (epoch % 100 == 0 || epoch == epochs - 1) {
                let loss = self.compute_loss(embeddings, targets);
                println!("Epoch {}/{}: loss = {:.6}", epoch + 1, epochs, loss);
            }
        }

        self.compute_loss(embeddings, targets)
    }

    /// Serialize the mapper to bytes (~12KB).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(4 + EMBED_DIM * OCTO_DIM * 4 + OCTO_DIM * 4);

        // Magic bytes
        buf.extend_from_slice(b"CMAP");

        // Weights (384 × 8 × 4 bytes = 12,288 bytes)
        for row in self.weights.iter() {
            for &val in row.iter() {
                buf.extend_from_slice(&val.to_le_bytes());
            }
        }

        // Bias (8 × 4 bytes = 32 bytes)
        for &val in self.bias.iter() {
            buf.extend_from_slice(&val.to_le_bytes());
        }

        buf
    }

    /// Deserialize from bytes.
    pub fn from_bytes(data: &[u8]) -> anyhow::Result<Self> {
        const EXPECTED_SIZE: usize = 4 + EMBED_DIM * OCTO_DIM * 4 + OCTO_DIM * 4;

        if data.len() != EXPECTED_SIZE {
            anyhow::bail!(
                "Invalid mapper size: expected {} bytes, got {}",
                EXPECTED_SIZE,
                data.len()
            );
        }

        if &data[0..4] != b"CMAP" {
            anyhow::bail!("Invalid mapper magic bytes");
        }

        let mut mapper = Self::new_zeros();
        let mut offset = 4;

        // Read weights
        for row in mapper.weights.iter_mut() {
            for val in row.iter_mut() {
                *val = f32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
                offset += 4;
            }
        }

        // Read bias
        for val in mapper.bias.iter_mut() {
            *val = f32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
            offset += 4;
        }

        Ok(mapper)
    }

    /// Save to a file.
    pub fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let bytes = self.to_bytes();
        let mut file = std::fs::File::create(path)?;
        file.write_all(&bytes)?;
        Ok(())
    }

    /// Load from a file.
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let mut file = std::fs::File::open(path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Self::from_bytes(&bytes)
    }
}

/// Sigmoid activation function.
#[inline]
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// Training data pair: (description text, target 8D coefficients).
#[derive(Clone)]
pub struct TrainingSample {
    pub description: String,
    pub target: [f32; OCTO_DIM],
}

/// Prepare training data from OctoIndex profiles.
/// Uses the crate name + any available description as input text.
pub fn prepare_training_data(
    profiles: &[crate::octo_index::OctonionProfile],
) -> Vec<TrainingSample> {
    profiles
        .iter()
        .map(|p| {
            // Use crate name as the description for now
            // In practice, you'd fetch README/description from crates.io
            let description = format!("{} version {} - Rust crate", p.name, p.version);
            TrainingSample {
                description,
                target: p.coeffs,
            }
        })
        .collect()
}

/// The bundled contrastive mapper, loaded at compile time.
#[cfg(feature = "bundled-mapper")]
pub static BUNDLED_MAPPER: std::sync::LazyLock<Option<ContrastiveMapper>> =
    std::sync::LazyLock::new(|| {
        static BYTES: &[u8] = include_bytes!("../contrastive-mapper.bin");
        if BYTES.is_empty() {
            None
        } else {
            ContrastiveMapper::from_bytes(BYTES).ok()
        }
    });

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_forward_dimensions() {
        let mapper = ContrastiveMapper::new_random();
        let embedding = vec![0.1; EMBED_DIM];
        let output = mapper.forward(&embedding);

        assert_eq!(output.len(), OCTO_DIM);
        for &val in output.iter() {
            assert!(val >= 0.0 && val <= 1.0, "Output should be in [0, 1]");
        }
    }

    #[test]
    fn test_serialization_roundtrip() {
        let mapper = ContrastiveMapper::new_random();
        let bytes = mapper.to_bytes();

        println!("Serialized size: {} bytes", bytes.len());
        assert_eq!(bytes.len(), 4 + 384 * 8 * 4 + 8 * 4); // 12,324 bytes

        let loaded = ContrastiveMapper::from_bytes(&bytes).unwrap();

        // Verify weights match
        for i in 0..EMBED_DIM {
            for j in 0..OCTO_DIM {
                assert_eq!(mapper.weights[i][j], loaded.weights[i][j]);
            }
        }
    }

    #[test]
    fn test_simple_training() {
        let mut mapper = ContrastiveMapper::new_random();

        // Create synthetic training data
        let embeddings: Vec<Vec<f32>> = (0..10)
            .map(|i| {
                let mut emb = vec![0.0; EMBED_DIM];
                emb[i % EMBED_DIM] = 1.0; // One-hot-ish
                emb
            })
            .collect();

        let targets: Vec<[f32; OCTO_DIM]> = (0..10)
            .map(|i| {
                let mut t = [0.0; OCTO_DIM];
                t[i % OCTO_DIM] = 1.0;
                t
            })
            .collect();

        let initial_loss = mapper.compute_loss(&embeddings, &targets);
        let final_loss = mapper.train(&embeddings, &targets, 0.1, 100, false);

        println!(
            "Initial loss: {:.6}, Final loss: {:.6}",
            initial_loss, final_loss
        );
        assert!(final_loss < initial_loss, "Training should reduce loss");
    }
}
