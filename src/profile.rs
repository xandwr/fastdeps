//! Octonion-based crate profiling for semantic + structural search.
//!
//! Each crate is represented as an 8-dimensional octonion where:
//! - e0 (real): Utility score (downloads, maintenance)
//! - e1: Concurrency (Send/Sync implementations)
//! - e2: Safety (unsafe block density)
//! - e3: Async (async fn ratio)
//! - e4: Memory (heap allocation patterns)
//! - e5: Friction (dependency count, compile time proxy)
//! - e6: Environment (no_std, WASM compatibility)
//! - e7: Entropy (API volatility, semver changes)
//!
//! The Fano plane structure encodes conflict triads:
//! - (e1, e2, e4): Unsafe concurrency
//! - (e2, e3, e5): Blocking in async
//! - (e3, e4, e6): Environment leak (async + heap in no_std)
//! - (e4, e5, e7): Volatility bloat
//! - (e5, e6, e1): Runtime friction
//! - (e6, e7, e2): Experimental unsafe
//! - (e7, e1, e3): Unstable async

use octonion::Octonion;
use std::path::Path;
use syn::visit::Visit;

/// Octonion profile for a crate, computed from static analysis.
#[derive(Debug, Clone)]
pub struct CrateProfile {
    /// Crate name
    pub name: String,
    /// Crate version
    pub version: String,
    /// The 8D octonion representation
    pub octonion: Octonion,
    /// Raw dimension values before normalization (for debugging)
    pub raw: RawProfile,
}

/// Raw extracted metrics before normalization to [0, 1].
#[derive(Debug, Clone, Default)]
pub struct RawProfile {
    pub utility: f32,         // e0: placeholder for now
    pub send_sync_count: u32, // e1: count of Send/Sync impls
    pub unsafe_blocks: u32,   // e2: count of unsafe blocks
    pub total_loc: u32,       // for e2 density
    pub async_fns: u32,       // e3: async fn count
    pub total_fns: u32,       // for e3 ratio
    pub heap_types: u32,      // e4: Box, Vec, Rc, Arc usage
    pub dep_count: u32,       // e5: direct dependency count
    pub is_no_std: bool,      // e6: no_std flag
    pub has_wasm: bool,       // e6: wasm target support
                              // e7 (entropy) requires version history - skip for MVP
}

impl CrateProfile {
    /// Analyze a crate's source directory and build its profile.
    pub fn from_source(name: &str, version: &str, source_dir: &Path) -> anyhow::Result<Self> {
        let mut raw = RawProfile::default();

        // Walk all .rs files
        analyze_directory(source_dir, &mut raw)?;

        // Check for no_std in lib.rs
        let lib_rs = source_dir.join("src/lib.rs");
        if lib_rs.exists() {
            let content = std::fs::read_to_string(&lib_rs)?;
            raw.is_no_std = content.contains("#![no_std]");
        }

        // Read Cargo.toml for dependency count
        let cargo_toml = source_dir.join("Cargo.toml");
        if cargo_toml.exists() {
            if let Ok(content) = std::fs::read_to_string(&cargo_toml) {
                raw.dep_count = count_dependencies(&content);
            }
        }

        // Normalize to octonion
        let octonion = raw.to_octonion();

        Ok(Self {
            name: name.to_string(),
            version: version.to_string(),
            octonion,
            raw,
        })
    }

    /// Compute compatibility score with a query octonion.
    /// Returns (similarity, friction) where:
    /// - similarity: Real part of Q * conj(C) — higher is better
    /// - friction: Norm of imaginary part — lower is better
    pub fn score(&self, query: &Octonion) -> (f32, f32) {
        let product = *query * self.octonion.conj();
        let similarity = product.real() as f32;

        // Imaginary norm (coefficients 1-7)
        let mut im_sq = 0.0;
        for i in 1..8 {
            let c = product.coeff(i);
            im_sq += c * c;
        }
        let friction = im_sq.sqrt() as f32;

        (similarity, friction)
    }

    /// Combined score: similarity weighted by lack of friction.
    pub fn combined_score(&self, query: &Octonion) -> f32 {
        let (similarity, friction) = self.score(query);
        similarity / (1.0 + friction)
    }
}

impl RawProfile {
    /// Convert raw metrics to normalized octonion.
    fn to_octonion(&self) -> Octonion {
        // e0: utility (placeholder - would come from crates.io API)
        let e0 = self.utility.clamp(0.0, 1.0) as f64;

        // e1: concurrency - normalized by total types (estimate)
        let e1 = if self.send_sync_count > 0 {
            (self.send_sync_count as f64 / 20.0).min(1.0)
        } else {
            0.0
        };

        // e2: safety - unsafe density per 1000 LoC
        let e2 = if self.total_loc > 0 {
            let density = (self.unsafe_blocks as f64 / self.total_loc as f64) * 1000.0;
            (density / 50.0).min(1.0) // 50 unsafe per 1000 LoC = max
        } else {
            0.0
        };

        // e3: async ratio
        let e3 = if self.total_fns > 0 {
            self.async_fns as f64 / self.total_fns as f64
        } else {
            0.0
        };

        // e4: memory/heap usage (normalized)
        let e4 = (self.heap_types as f64 / 100.0).min(1.0);

        // e5: friction - dependency count
        let e5 = (self.dep_count as f64 / 50.0).min(1.0); // 50 deps = max friction

        // e6: environment (no_std/wasm)
        let e6 = match (self.is_no_std, self.has_wasm) {
            (true, true) => 1.0,
            (true, false) => 0.7,
            (false, true) => 0.5,
            (false, false) => 0.0,
        };

        // e7: entropy (placeholder - would need version history)
        let e7 = 0.0;

        Octonion::new(e0, e1, e2, e3, e4, e5, e6, e7)
    }
}

/// Analyze all .rs files in a directory recursively.
fn analyze_directory(dir: &Path, raw: &mut RawProfile) -> anyhow::Result<()> {
    if !dir.exists() {
        return Ok(());
    }

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_file() && path.extension().map(|e| e == "rs").unwrap_or(false) {
            analyze_file(&path, raw)?;
        } else if path.is_dir() {
            analyze_directory(&path, raw)?;
        }
    }

    Ok(())
}

/// Analyze a single .rs file.
fn analyze_file(path: &Path, raw: &mut RawProfile) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(path)?;
    raw.total_loc += content.lines().count() as u32;

    // Parse with syn
    let Ok(syntax) = syn::parse_file(&content) else {
        return Ok(()); // Skip unparseable files
    };

    let mut visitor = ProfileVisitor::default();
    visitor.visit_file(&syntax);

    raw.unsafe_blocks += visitor.unsafe_blocks;
    raw.async_fns += visitor.async_fns;
    raw.total_fns += visitor.total_fns;
    raw.send_sync_count += visitor.send_sync_impls;
    raw.heap_types += visitor.heap_types;

    Ok(())
}

/// AST visitor to extract profile metrics.
#[derive(Default)]
struct ProfileVisitor {
    unsafe_blocks: u32,
    async_fns: u32,
    total_fns: u32,
    send_sync_impls: u32,
    heap_types: u32,
}

impl<'ast> Visit<'ast> for ProfileVisitor {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.total_fns += 1;
        if node.sig.asyncness.is_some() {
            self.async_fns += 1;
        }
        syn::visit::visit_item_fn(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.total_fns += 1;
        if node.sig.asyncness.is_some() {
            self.async_fns += 1;
        }
        syn::visit::visit_impl_item_fn(self, node);
    }

    fn visit_expr_unsafe(&mut self, node: &'ast syn::ExprUnsafe) {
        self.unsafe_blocks += 1;
        syn::visit::visit_expr_unsafe(self, node);
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        // Check if implementing Send or Sync
        if let Some((_, trait_path, _)) = &node.trait_ {
            if let Some(last) = trait_path.segments.last() {
                let name = last.ident.to_string();
                if name == "Send" || name == "Sync" {
                    self.send_sync_impls += 1;
                }
            }
        }
        syn::visit::visit_item_impl(self, node);
    }

    fn visit_type_path(&mut self, node: &'ast syn::TypePath) {
        // Check for heap-allocating types
        if let Some(last) = node.path.segments.last() {
            let name = last.ident.to_string();
            if matches!(
                name.as_str(),
                "Box" | "Vec" | "String" | "Rc" | "Arc" | "HashMap" | "BTreeMap"
            ) {
                self.heap_types += 1;
            }
        }
        syn::visit::visit_type_path(self, node);
    }
}

/// Count dependencies from Cargo.toml content (simple heuristic).
fn count_dependencies(cargo_toml: &str) -> u32 {
    let mut count = 0;
    let mut in_deps = false;

    for line in cargo_toml.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("[dependencies]")
            || trimmed.starts_with("[dev-dependencies]")
            || trimmed.starts_with("[build-dependencies]")
        {
            in_deps = true;
            continue;
        }

        if trimmed.starts_with('[') {
            in_deps = false;
            continue;
        }

        if in_deps && !trimmed.is_empty() && !trimmed.starts_with('#') {
            // Check if it's a dependency line (contains = or starts without whitespace)
            if trimmed.contains('=') || !trimmed.starts_with(char::is_whitespace) {
                count += 1;
            }
        }
    }

    count
}

/// Build a query octonion from semantic requirements.
/// This is a simple manual mapping for testing - the real version
/// would use the trained 384D → 8D projection.
pub fn query_octonion(
    wants_async: bool,
    wants_sync_send: bool,
    wants_no_std: bool,
    prefers_safe: bool,
    prefers_light: bool,
) -> Octonion {
    let e0 = 0.9; // Always want utility
    let e1 = if wants_sync_send { 0.9 } else { 0.0 };
    let e2 = if prefers_safe { -0.5 } else { 0.0 }; // Negative = avoid unsafe
    let e3 = if wants_async { 0.9 } else { 0.0 };
    let e4 = if prefers_light { -0.3 } else { 0.0 }; // Negative = prefer stack
    let e5 = if prefers_light { -0.5 } else { 0.0 }; // Negative = fewer deps
    let e6 = if wants_no_std { 0.9 } else { 0.0 };
    let e7 = 0.0; // Don't care about entropy for now

    Octonion::new(e0, e1, e2, e3, e4, e5, e6, e7)
}

/// Helper to extract octonion coefficients for display
pub fn octonion_coeffs(o: &Octonion) -> [f64; 8] {
    [
        o.real(),
        o.coeff(1),
        o.coeff(2),
        o.coeff(3),
        o.coeff(4),
        o.coeff(5),
        o.coeff(6),
        o.coeff(7),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_octonion_scoring() {
        // Simulate a high-quality async crate with Send/Sync
        let crate_oct = Octonion::new(
            0.9, // utility
            1.0, // send/sync
            0.1, // low unsafe
            0.8, // mostly async
            0.3, // some heap
            0.2, // few deps
            0.0, // std only
            0.1, // stable
        );

        let profile = CrateProfile {
            name: "test".into(),
            version: "1.0.0".into(),
            octonion: crate_oct,
            raw: RawProfile::default(),
        };

        // Query: async + send/sync
        let query = query_octonion(true, true, false, true, true);
        let (sim, friction) = profile.score(&query);

        println!("Similarity: {:.3}", sim);
        println!("Friction: {:.3}", friction);
        println!("Combined: {:.3}", profile.combined_score(&query));

        assert!(sim > 0.0, "Should have positive similarity");
    }

    #[test]
    fn test_conflict_detection() {
        // A crate with async + lots of unsafe (potential conflict)
        let risky = Octonion::new(
            0.7, // utility
            0.0, // no send/sync
            0.9, // high unsafe
            0.9, // high async
            0.5, // heap
            0.3, // deps
            0.0, // std
            0.5, // volatile
        );

        let profile = CrateProfile {
            name: "risky".into(),
            version: "1.0.0".into(),
            octonion: risky,
            raw: RawProfile::default(),
        };

        // Query: async + safe
        let query = query_octonion(true, true, false, true, false);
        let (sim, friction) = profile.score(&query);

        println!(
            "Risky crate - Similarity: {:.3}, Friction: {:.3}",
            sim, friction
        );

        // Should have higher friction due to unsafe+async combination
        assert!(friction > 0.3, "Should detect friction from unsafe+async");
    }
}
