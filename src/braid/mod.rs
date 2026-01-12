//! Braid-theoretic analysis of crate dependency graphs.
//!
//! Models the dependency graph as a braid word where each crate is a generator σᵢ.
//! Crossings between strands represent type-level interactions (trait bounds,
//! wrapper nesting, lifetime constraints).
//!
//! Key concepts:
//! - **Braid Word**: Sequence of generators representing dependency composition order
//! - **Crossing**: Point where two crate "strands" interact via shared types
//! - **Tangle**: Topological conflict (e.g., competing async runtimes)
//! - **Fano Constraint**: Parity check using the 8D octonion coordinates

pub mod crossing;
pub mod fano;
pub mod word;

pub use crossing::{Crossing, CrossingKind, TypeCharge};
pub use fano::{FanoConstraint, FanoLine};
pub use word::{BraidGenerator, BraidWord};

use crate::octo_index::OctonionProfile;

/// Result of analyzing crossings between two crate strands.
#[derive(Debug)]
pub enum CrossingAnalysis {
    /// No conflict - strands pass cleanly
    Clean,
    /// Resolvable via shim - includes the template name
    Resolvable {
        template: &'static str,
        fano_match: f32,
    },
    /// Essential tangle - architectural incompatibility
    Essential {
        conflict: ManifoldConflict,
        suggestion: String,
    },
}

/// Describes an essential (non-resolvable) conflict between dimensions.
#[derive(Debug, Clone)]
pub struct ManifoldConflict {
    /// First conflicting dimension index (0-7)
    pub dim_a: usize,
    /// Second conflicting dimension index (0-7)
    pub dim_b: usize,
    /// Human-readable conflict description
    pub description: String,
}

impl ManifoldConflict {
    pub fn new(dim_a: usize, dim_b: usize, description: impl Into<String>) -> Self {
        Self {
            dim_a,
            dim_b,
            description: description.into(),
        }
    }

    /// Generate a re-projection suggestion based on the conflict type.
    pub fn suggest_reproject(&self) -> String {
        match (self.dim_a, self.dim_b) {
            // e1 (Concurrency) vs e6 (Environment) - thread-local vs global
            (1, 6) | (6, 1) => {
                "Shift from thread-local to atomic: Rc → Arc, RefCell → Mutex".into()
            }
            // e2 (Safety) vs e3 (Async) - unsafe + async is dangerous
            (2, 3) | (3, 2) => {
                "Isolate unsafe code from async boundaries, use spawn_blocking".into()
            }
            // e3 (Async) vs e3 (Async) - competing runtimes
            (3, 3) => "Feature-gate one runtime, or use async-compat bridge".into(),
            // e1 (Concurrency) vs e4 (Memory) - Send vs heap ownership
            (1, 4) | (4, 1) => "Use Arc<T> for shared heap data across threads".into(),
            _ => format!(
                "Dimensions e{} and e{} conflict - manual intervention required",
                self.dim_a, self.dim_b
            ),
        }
    }
}

/// Analyze a pair of crates for potential tangles.
pub fn analyze_crossing(
    crate_a: &OctonionProfile,
    crate_b: &OctonionProfile,
    crossing: &Crossing,
) -> CrossingAnalysis {
    // Check Fano constraints for compatibility score
    let fano_score = fano::check_all_lines(&crate_a.coeffs, &crate_b.coeffs);

    // FIRST: Check for essential conflicts (these override everything)
    if let Some(conflict) = detect_essential_conflict(&crate_a.coeffs, &crate_b.coeffs) {
        let suggestion = conflict.suggest_reproject();
        return CrossingAnalysis::Essential {
            conflict,
            suggestion,
        };
    }

    // SECOND: Check if the crossing itself indicates a conflict that needs resolution
    // The crossing kind tells us there's a semantic conflict even if profiles are similar
    if let Some(template) = find_matching_template(crate_a, crate_b, crossing) {
        return CrossingAnalysis::Resolvable {
            template,
            fano_match: fano_score,
        };
    }

    // THIRD: If no explicit crossing conflict, use Fano score
    // Low Fano score with no template = unknown conflict
    if fano_score < 0.5 {
        return CrossingAnalysis::Essential {
            conflict: ManifoldConflict::new(
                0,
                0,
                "Low Fano compatibility with no known resolution pattern",
            ),
            suggestion: "Manual review required - profiles have conflicting characteristics".into(),
        };
    }

    // Default: clean (profiles compatible, no crossing conflict)
    CrossingAnalysis::Clean
}

/// Find a pre-verified template that matches this crossing pattern.
fn find_matching_template(
    crate_a: &OctonionProfile,
    crate_b: &OctonionProfile,
    crossing: &Crossing,
) -> Option<&'static str> {
    let a = &crate_a.coeffs;
    let b = &crate_b.coeffs;

    match &crossing.kind {
        // Both are async-heavy, different runtimes
        CrossingKind::TraitConflict { trait_name, .. } if trait_name.contains("AsyncRead") => {
            if a[3] > 0.5 && b[3] > 0.5 {
                return Some("AsyncReadAdapter");
            }
        }

        // Send/Sync mismatch
        CrossingKind::MarkerConflict { send_mismatch, .. } if *send_mismatch => {
            // Check if it's a simple Rc → Arc case
            if a[1] > 0.5 && b[1] < 0.3 {
                return Some("SyncProxy");
            }
        }

        // Wrapper nesting conflict
        CrossingKind::WrapperNesting { .. } => {
            // Pin projection needed
            if crossing.involves_pin {
                return Some("PinnedFutureBridge");
            }
        }

        _ => {}
    }

    None
}

/// Detect essential (non-bridgeable) conflicts.
fn detect_essential_conflict(a: &[f32; 8], b: &[f32; 8]) -> Option<ManifoldConflict> {
    // e3 vs e3: Both async-heavy but one is e6=1 (no_std) - can't bridge
    if a[3] > 0.7 && b[3] > 0.7 && (a[6] - b[6]).abs() > 0.8 {
        return Some(ManifoldConflict::new(
            3,
            6,
            "Async runtime vs no_std async - incompatible execution models",
        ));
    }

    // e1 (concurrency) vs e6 (environment): thread-local meets multi-threaded
    if a[1] > 0.7 && b[6] > 0.5 && b[1] < 0.2 {
        return Some(ManifoldConflict::new(
            1,
            6,
            "Thread-local state crossing into concurrent context",
        ));
    }

    // e2 (safety) threshold: if one crate is very unsafe-heavy
    if (a[2] > 0.8 || b[2] > 0.8) && (a[2] - b[2]).abs() > 0.6 {
        return Some(ManifoldConflict::new(
            2,
            2,
            "Unsafe density mismatch - verify invariant preservation manually",
        ));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::octo_index::RawMetrics;

    fn make_profile(name: &str, coeffs: [f32; 8]) -> OctonionProfile {
        OctonionProfile {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            coeffs,
            raw: RawMetrics::default(),
        }
    }

    #[test]
    fn test_async_runtime_conflict() {
        // tokio-like profile: high async, high concurrency
        let tokio = make_profile("tokio", [0.9, 0.9, 0.1, 0.95, 0.4, 0.3, 0.0, 0.2]);

        // smol-like profile: high async, lower concurrency
        let smol = make_profile("smol", [0.7, 0.6, 0.05, 0.9, 0.2, 0.1, 0.0, 0.3]);

        let crossing = Crossing {
            location: "src/main.rs:42".into(),
            sigma_i: 0,
            sigma_j: 1,
            kind: CrossingKind::TraitConflict {
                trait_name: "AsyncRead".into(),
                bound_a: "tokio::io::AsyncRead".into(),
                bound_b: "smol::io::AsyncRead".into(),
            },
            involves_pin: false,
        };

        let result = analyze_crossing(&tokio, &smol, &crossing);
        match result {
            CrossingAnalysis::Resolvable { template, .. } => {
                assert_eq!(template, "AsyncReadAdapter");
            }
            other => panic!("Expected Resolvable, got {:?}", other),
        }
    }

    #[test]
    fn test_essential_conflict() {
        // tokio: async runtime
        let tokio = make_profile("tokio", [0.9, 0.9, 0.1, 0.95, 0.4, 0.3, 0.0, 0.2]);

        // embedded-hal: no_std async
        let embedded = make_profile("embedded-hal", [0.5, 0.2, 0.1, 0.8, 0.1, 0.1, 1.0, 0.1]);

        let crossing = Crossing {
            location: "src/driver.rs:10".into(),
            sigma_i: 0,
            sigma_j: 1,
            kind: CrossingKind::TraitConflict {
                trait_name: "AsyncRead".into(),
                bound_a: "tokio::io::AsyncRead".into(),
                bound_b: "embedded_io_async::Read".into(),
            },
            involves_pin: false,
        };

        let result = analyze_crossing(&tokio, &embedded, &crossing);
        match result {
            CrossingAnalysis::Essential { conflict, .. } => {
                assert!(conflict.dim_a == 3 || conflict.dim_b == 3);
            }
            other => panic!("Expected Essential conflict, got {:?}", other),
        }
    }
}
