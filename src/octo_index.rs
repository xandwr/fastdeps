//! Octo-Index: Pre-computed octonion profiles for top crates.
//!
//! The "Sleeper Implementation" batch-processes the top 10,000 crates from crates.io
//! db-dump, computes their octonion profiles via static analysis, and serializes
//! to a Zstd-compressed binary file bundled into the binary.
//!
//! Dimension mapping:
//! - e0 (Utility): downloads / age_days
//! - e1 (Concurrency): grep -c "impl.*Send|Sync" normalized
//! - e2 (Safety): unsafe blocks / LoC
//! - e3 (Async): async fn ratio
//! - e6 (no_std): Binary flag (1.0 or 0.0)
//! - e7 (Entropy): versions.len() / age_days

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Write};

/// Compact octonion profile for serialization (no external crate dependency).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OctonionProfile {
    /// Crate name
    pub name: String,
    /// Latest version analyzed
    pub version: String,
    /// 8 octonion coefficients [e0..e7]
    pub coeffs: [f32; 8],
    /// Raw metrics for transparency
    pub raw: RawMetrics,
}

/// Raw metrics extracted from static analysis and db-dump.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RawMetrics {
    /// Total downloads
    pub downloads: u64,
    /// Age in days since first version
    pub age_days: u32,
    /// Number of versions published
    pub version_count: u32,
    /// Count of Send/Sync implementations
    pub send_sync_count: u32,
    /// Number of unsafe blocks
    pub unsafe_blocks: u32,
    /// Total lines of code
    pub total_loc: u32,
    /// Number of async functions
    pub async_fns: u32,
    /// Total functions
    pub total_fns: u32,
    /// Whether crate is no_std
    pub is_no_std: bool,
    /// Direct dependency count
    pub dep_count: u32,
    /// Heap-allocating type usage count
    pub heap_types: u32,
}

impl RawMetrics {
    /// Convert raw metrics to normalized octonion coefficients.
    pub fn to_coeffs(&self) -> [f32; 8] {
        // e0: Utility = downloads / age_days (normalized)
        let e0 = if self.age_days > 0 {
            let rate = self.downloads as f64 / self.age_days as f64;
            // Log scale: 1000 downloads/day = 1.0
            ((rate + 1.0).log10() / 3.0).min(1.0) as f32
        } else {
            0.0
        };

        // e1: Concurrency = Send/Sync impl count (normalized)
        let e1 = (self.send_sync_count as f32 / 20.0).min(1.0);

        // e2: Safety = unsafe density per 1000 LoC (inverted: lower is safer)
        let e2 = if self.total_loc > 0 {
            let density = (self.unsafe_blocks as f32 / self.total_loc as f32) * 1000.0;
            (density / 50.0).min(1.0) // 50 unsafe per 1000 LoC = max
        } else {
            0.0
        };

        // e3: Async = async fn ratio
        let e3 = if self.total_fns > 0 {
            self.async_fns as f32 / self.total_fns as f32
        } else {
            0.0
        };

        // e4: Memory = heap type usage (normalized)
        let e4 = (self.heap_types as f32 / 100.0).min(1.0);

        // e5: Friction = dependency count (normalized)
        let e5 = (self.dep_count as f32 / 50.0).min(1.0);

        // e6: Environment = no_std (binary)
        let e6 = if self.is_no_std { 1.0 } else { 0.0 };

        // e7: Entropy = versions / age_days (high = volatile)
        let e7 = if self.age_days > 0 {
            let rate = self.version_count as f32 / self.age_days as f32;
            // 1 version per 7 days = 1.0 (high volatility)
            (rate * 7.0).min(1.0)
        } else {
            0.0
        };

        [e0, e1, e2, e3, e4, e5, e6, e7]
    }
}

/// The complete Octo-Index containing all pre-computed profiles.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OctoIndex {
    /// Version of the index format
    pub version: u32,
    /// When this index was generated (Unix timestamp)
    pub generated_at: u64,
    /// Number of crates in the index
    pub count: usize,
    /// Map from crate name to profile
    pub profiles: HashMap<String, OctonionProfile>,
}

impl OctoIndex {
    /// Magic bytes for the binary format.
    const MAGIC: &'static [u8] = b"OCTO";
    /// Current format version.
    const FORMAT_VERSION: u32 = 1;

    /// Create a new empty index.
    pub fn new() -> Self {
        Self {
            version: Self::FORMAT_VERSION,
            generated_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            count: 0,
            profiles: HashMap::new(),
        }
    }

    /// Add a profile to the index.
    pub fn insert(&mut self, profile: OctonionProfile) {
        self.profiles.insert(profile.name.clone(), profile);
        self.count = self.profiles.len();
    }

    /// Look up a crate by name.
    pub fn get(&self, name: &str) -> Option<&OctonionProfile> {
        self.profiles.get(name)
    }

    /// Serialize to Zstd-compressed bytes.
    pub fn to_bytes(&self) -> anyhow::Result<Vec<u8>> {
        let mut buf = Vec::new();

        // Write magic
        buf.extend_from_slice(Self::MAGIC);

        // Serialize with bincode-like format using serde + postcard would be ideal,
        // but for simplicity we'll use JSON then compress
        let json = serde_json::to_vec(self)?;

        // Compress with Zstd
        let compressed = zstd::encode_all(json.as_slice(), 19)?; // Level 19 = max compression
        buf.extend_from_slice(&compressed);

        Ok(buf)
    }

    /// Deserialize from Zstd-compressed bytes.
    pub fn from_bytes(data: &[u8]) -> anyhow::Result<Self> {
        // Check magic
        if data.len() < 4 || &data[0..4] != Self::MAGIC {
            anyhow::bail!("Invalid Octo-Index magic bytes");
        }

        // Decompress
        let decompressed = zstd::decode_all(&data[4..])?;

        // Deserialize
        let index: OctoIndex = serde_json::from_slice(&decompressed)?;

        Ok(index)
    }

    /// Save to a file.
    pub fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        let bytes = self.to_bytes()?;
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

    /// Get all profiles sorted by utility (e0) descending.
    pub fn top_by_utility(&self, limit: usize) -> Vec<&OctonionProfile> {
        let mut profiles: Vec<_> = self.profiles.values().collect();
        profiles.sort_by(|a, b| b.coeffs[0].partial_cmp(&a.coeffs[0]).unwrap());
        profiles.into_iter().take(limit).collect()
    }

    /// Search profiles by query octonion, returning (name, score) pairs.
    pub fn search(&self, query: &[f32; 8], limit: usize) -> Vec<(&OctonionProfile, f32)> {
        let mut scored: Vec<_> = self
            .profiles
            .values()
            .map(|p| {
                let score = combined_score(&p.coeffs, query);
                (p, score)
            })
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        scored.into_iter().take(limit).collect()
    }
}

impl Default for OctoIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// Compute octonion product-based similarity score.
/// Returns combined score: similarity / (1 + friction).
fn combined_score(crate_coeffs: &[f32; 8], query: &[f32; 8]) -> f32 {
    // Simplified scoring: dot product of coefficients
    // Real octonion multiplication would be more complex, but this captures the essence
    let mut similarity: f32 = 0.0;
    let mut friction: f32 = 0.0;

    for i in 0..8 {
        let c = crate_coeffs[i];
        let q = query[i];

        if q >= 0.0 {
            // Positive query = want this property
            similarity += c * q;
        } else {
            // Negative query = avoid this property
            friction += c * (-q);
        }
    }

    similarity / (1.0 + friction)
}

/// Build a query coefficients array from semantic flags.
pub fn build_query(
    wants_async: bool,
    wants_sync: bool,
    wants_no_std: bool,
    prefers_safe: bool,
    prefers_light: bool,
) -> [f32; 8] {
    [
        0.9,                                    // e0: always want utility
        if wants_sync { 0.9 } else { 0.0 },     // e1: concurrency
        if prefers_safe { -0.5 } else { 0.0 },  // e2: safety (negative = avoid unsafe)
        if wants_async { 0.9 } else { 0.0 },    // e3: async
        if prefers_light { -0.3 } else { 0.0 }, // e4: memory (negative = prefer stack)
        if prefers_light { -0.5 } else { 0.0 }, // e5: friction (negative = fewer deps)
        if wants_no_std { 0.9 } else { 0.0 },   // e6: no_std
        0.0,                                    // e7: entropy (neutral)
    ]
}

/// The bundled Octo-Index, loaded from include_bytes! at compile time.
/// This is populated by the build.rs script.
#[cfg(feature = "bundled-index")]
pub static BUNDLED_INDEX: std::sync::LazyLock<Option<OctoIndex>> = std::sync::LazyLock::new(|| {
    static BYTES: &[u8] = include_bytes!("../octo-index.bin");
    if BYTES.is_empty() {
        None
    } else {
        OctoIndex::from_bytes(BYTES).ok()
    }
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_raw_metrics_to_coeffs() {
        let raw = RawMetrics {
            downloads: 100_000,
            age_days: 365,
            version_count: 20,
            send_sync_count: 5,
            unsafe_blocks: 10,
            total_loc: 5000,
            async_fns: 25,
            total_fns: 100,
            is_no_std: false,
            dep_count: 10,
            heap_types: 30,
        };

        let coeffs = raw.to_coeffs();
        println!("Coefficients: {:?}", coeffs);

        // Basic sanity checks
        assert!(coeffs[0] > 0.0, "Should have positive utility");
        assert!(coeffs[1] > 0.0, "Should have some concurrency score");
        assert!(coeffs[3] > 0.0, "Should have async score (25% async)");
        assert_eq!(coeffs[6], 0.0, "Should not be no_std");
    }

    #[test]
    fn test_serialization_roundtrip() {
        let mut index = OctoIndex::new();

        let raw = RawMetrics {
            downloads: 50_000,
            age_days: 100,
            ..Default::default()
        };

        index.insert(OctonionProfile {
            name: "test-crate".into(),
            version: "1.0.0".into(),
            coeffs: raw.to_coeffs(),
            raw,
        });

        // Serialize and deserialize
        let bytes = index.to_bytes().unwrap();
        println!("Serialized size: {} bytes", bytes.len());

        let loaded = OctoIndex::from_bytes(&bytes).unwrap();
        assert_eq!(loaded.count, 1);
        assert!(loaded.get("test-crate").is_some());
    }

    #[test]
    fn test_search() {
        let mut index = OctoIndex::new();

        // Async-heavy crate
        index.insert(OctonionProfile {
            name: "async-crate".into(),
            version: "1.0.0".into(),
            coeffs: [0.5, 0.8, 0.1, 0.9, 0.3, 0.2, 0.0, 0.1],
            raw: RawMetrics::default(),
        });

        // no_std crate
        index.insert(OctonionProfile {
            name: "embedded-crate".into(),
            version: "1.0.0".into(),
            coeffs: [0.3, 0.0, 0.0, 0.0, 0.0, 0.1, 1.0, 0.05],
            raw: RawMetrics::default(),
        });

        // Query for async
        let query = build_query(true, true, false, false, false);
        let results = index.search(&query, 10);

        assert_eq!(results[0].0.name, "async-crate");
    }
}
