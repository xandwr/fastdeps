//! Crossing detection - finding where crate "strands" interact.
//!
//! A crossing occurs when two crates interact through:
//! - Shared trait bounds (both require `T: AsyncRead`)
//! - Wrapper nesting (one crate's type wraps another's)
//! - Marker trait conflicts (Send vs !Send)
//! - Lifetime intersection

use std::collections::HashSet;

/// The "charge" a type carries through the crate graph.
/// Tracks marker traits that affect composition.
#[derive(Debug, Clone, Default)]
pub struct TypeCharge {
    /// Send bound status
    pub send: Ternary,
    /// Sync bound status
    pub sync: Ternary,
    /// Unpin bound status
    pub unpin: Ternary,
    /// Has 'static lifetime
    pub is_static: bool,
    /// Is Sized
    pub sized: bool,
}

/// Three-valued logic for trait bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Ternary {
    Yes,
    No,
    #[default]
    Unknown,
}

impl Ternary {
    pub fn from_bool(b: bool) -> Self {
        if b { Ternary::Yes } else { Ternary::No }
    }

    /// Check if two ternary values conflict.
    pub fn conflicts(&self, other: &Ternary) -> bool {
        matches!(
            (self, other),
            (Ternary::Yes, Ternary::No) | (Ternary::No, Ternary::Yes)
        )
    }
}

/// A crossing point where two crate strands interact.
#[derive(Debug, Clone)]
pub struct Crossing {
    /// Source location (file:line)
    pub location: String,
    /// Index of first crate in braid word
    pub sigma_i: usize,
    /// Index of second crate in braid word
    pub sigma_j: usize,
    /// Kind of crossing
    pub kind: CrossingKind,
    /// Whether this crossing involves Pin projection
    pub involves_pin: bool,
}

/// The kind of interaction at a crossing.
#[derive(Debug, Clone)]
pub enum CrossingKind {
    /// Both crates expect different impls of the same trait
    TraitConflict {
        trait_name: String,
        bound_a: String,
        bound_b: String,
    },
    /// Marker trait mismatch (Send/Sync/Unpin)
    MarkerConflict {
        send_mismatch: bool,
        sync_mismatch: bool,
        unpin_mismatch: bool,
    },
    /// Type wrapper nesting order matters
    WrapperNesting {
        outer: String,
        inner: String,
        can_commute: bool,
    },
    /// Lifetime bound intersection
    LifetimeIntersection {
        lifetime_a: String,
        lifetime_b: String,
    },
    /// Generic: unspecified interaction
    Generic { description: String },
}

impl Crossing {
    /// Create a new trait conflict crossing.
    pub fn trait_conflict(
        location: impl Into<String>,
        sigma_i: usize,
        sigma_j: usize,
        trait_name: impl Into<String>,
        bound_a: impl Into<String>,
        bound_b: impl Into<String>,
    ) -> Self {
        Self {
            location: location.into(),
            sigma_i,
            sigma_j,
            kind: CrossingKind::TraitConflict {
                trait_name: trait_name.into(),
                bound_a: bound_a.into(),
                bound_b: bound_b.into(),
            },
            involves_pin: false,
        }
    }

    /// Create a marker conflict crossing.
    pub fn marker_conflict(
        location: impl Into<String>,
        sigma_i: usize,
        sigma_j: usize,
        charge_a: &TypeCharge,
        charge_b: &TypeCharge,
    ) -> Option<Self> {
        let send_mismatch = charge_a.send.conflicts(&charge_b.send);
        let sync_mismatch = charge_a.sync.conflicts(&charge_b.sync);
        let unpin_mismatch = charge_a.unpin.conflicts(&charge_b.unpin);

        if !send_mismatch && !sync_mismatch && !unpin_mismatch {
            return None;
        }

        Some(Self {
            location: location.into(),
            sigma_i,
            sigma_j,
            kind: CrossingKind::MarkerConflict {
                send_mismatch,
                sync_mismatch,
                unpin_mismatch,
            },
            involves_pin: unpin_mismatch,
        })
    }

    /// Create a wrapper nesting crossing.
    pub fn wrapper_nesting(
        location: impl Into<String>,
        sigma_i: usize,
        sigma_j: usize,
        outer: impl Into<String>,
        inner: impl Into<String>,
        can_commute: bool,
    ) -> Self {
        Self {
            location: location.into(),
            sigma_i,
            sigma_j,
            kind: CrossingKind::WrapperNesting {
                outer: outer.into(),
                inner: inner.into(),
                can_commute,
            },
            involves_pin: false,
        }
    }

    /// Human-readable description of the crossing.
    pub fn describe(&self) -> String {
        match &self.kind {
            CrossingKind::TraitConflict {
                trait_name,
                bound_a,
                bound_b,
            } => {
                format!(
                    "Trait conflict: {} requires {} but {} expects {}",
                    trait_name, bound_a, trait_name, bound_b
                )
            }
            CrossingKind::MarkerConflict {
                send_mismatch,
                sync_mismatch,
                unpin_mismatch,
            } => {
                let mut conflicts = Vec::new();
                if *send_mismatch {
                    conflicts.push("Send");
                }
                if *sync_mismatch {
                    conflicts.push("Sync");
                }
                if *unpin_mismatch {
                    conflicts.push("Unpin");
                }
                format!("Marker trait conflict: {} mismatch", conflicts.join(", "))
            }
            CrossingKind::WrapperNesting {
                outer,
                inner,
                can_commute,
            } => {
                if *can_commute {
                    format!("Wrapper nesting: {}<{}> (commutable)", outer, inner)
                } else {
                    format!("Wrapper nesting: {}<{}> (order matters!)", outer, inner)
                }
            }
            CrossingKind::LifetimeIntersection {
                lifetime_a,
                lifetime_b,
            } => {
                format!("Lifetime intersection: {} vs {}", lifetime_a, lifetime_b)
            }
            CrossingKind::Generic { description } => description.clone(),
        }
    }
}

/// Trait bounds extracted from a crate's API.
#[derive(Debug, Clone, Default)]
pub struct ExtractedBounds {
    /// Trait bounds by trait name
    pub trait_bounds: HashSet<String>,
    /// Required marker traits
    pub markers: TypeCharge,
    /// Common wrapper types used
    pub wrappers: Vec<String>,
}

impl ExtractedBounds {
    /// Check for async-related traits.
    pub fn is_async(&self) -> bool {
        self.trait_bounds.iter().any(|t| {
            t.contains("Future")
                || t.contains("AsyncRead")
                || t.contains("AsyncWrite")
                || t.contains("Stream")
        })
    }

    /// Check for sync-related traits.
    pub fn is_sync_heavy(&self) -> bool {
        self.markers.send == Ternary::Yes || self.markers.sync == Ternary::Yes
    }

    /// Find conflicting bounds with another set.
    pub fn find_conflicts(&self, other: &ExtractedBounds) -> Vec<Crossing> {
        let mut crossings = Vec::new();

        // Check for same-trait different-impl conflicts
        for bound in &self.trait_bounds {
            if let Some(other_bound) = other
                .trait_bounds
                .iter()
                .find(|b| trait_base_name(b) == trait_base_name(bound) && b != &bound)
            {
                crossings.push(Crossing::trait_conflict(
                    "unknown",
                    0,
                    1,
                    trait_base_name(bound),
                    bound,
                    other_bound,
                ));
            }
        }

        // Check marker conflicts
        if let Some(marker_crossing) =
            Crossing::marker_conflict("unknown", 0, 1, &self.markers, &other.markers)
        {
            crossings.push(marker_crossing);
        }

        crossings
    }
}

/// Extract the base name from a fully qualified trait path.
fn trait_base_name(trait_path: &str) -> &str {
    trait_path.rsplit("::").next().unwrap_or(trait_path)
}

/// Detect crossings from two crates' public APIs.
/// This is a simplified heuristic - full analysis would require type flow.
pub fn detect_crossings_heuristic(
    crate_a_name: &str,
    crate_a_bounds: &ExtractedBounds,
    crate_b_name: &str,
    crate_b_bounds: &ExtractedBounds,
) -> Vec<Crossing> {
    let mut crossings = crate_a_bounds.find_conflicts(crate_b_bounds);

    // Check for wrapper conflicts
    for wrapper_a in &crate_a_bounds.wrappers {
        for wrapper_b in &crate_b_bounds.wrappers {
            // If both crates have wrapper types, order might matter
            if is_outer_wrapper(wrapper_a) && is_outer_wrapper(wrapper_b) {
                crossings.push(Crossing::wrapper_nesting(
                    format!("{}+{} composition", crate_a_name, crate_b_name),
                    0,
                    1,
                    wrapper_a,
                    wrapper_b,
                    false, // Conservative: assume order matters
                ));
            }
        }
    }

    // Special case: both async-heavy
    if crate_a_bounds.is_async() && crate_b_bounds.is_async() {
        crossings.push(Crossing {
            location: format!("{}+{} async composition", crate_a_name, crate_b_name),
            sigma_i: 0,
            sigma_j: 1,
            kind: CrossingKind::Generic {
                description: "Both crates are async-heavy - potential runtime conflict".into(),
            },
            involves_pin: true, // Async usually involves Pin
        });
    }

    crossings
}

/// Check if a type name suggests it's an outer wrapper.
fn is_outer_wrapper(type_name: &str) -> bool {
    type_name.contains("Wrapper")
        || type_name.contains("Adapter")
        || type_name.contains("Guard")
        || type_name.contains("Lock")
        || type_name.contains("Ref")
        || type_name.contains("Box")
        || type_name.contains("Arc")
        || type_name.contains("Rc")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ternary_conflicts() {
        assert!(Ternary::Yes.conflicts(&Ternary::No));
        assert!(!Ternary::Yes.conflicts(&Ternary::Yes));
        assert!(!Ternary::Yes.conflicts(&Ternary::Unknown));
        assert!(!Ternary::Unknown.conflicts(&Ternary::Unknown));
    }

    #[test]
    fn test_marker_conflict_detection() {
        let charge_send = TypeCharge {
            send: Ternary::Yes,
            ..Default::default()
        };
        let charge_no_send = TypeCharge {
            send: Ternary::No,
            ..Default::default()
        };

        let crossing = Crossing::marker_conflict("test:1", 0, 1, &charge_send, &charge_no_send);
        assert!(crossing.is_some());

        let c = crossing.unwrap();
        match c.kind {
            CrossingKind::MarkerConflict { send_mismatch, .. } => {
                assert!(send_mismatch);
            }
            _ => panic!("Expected marker conflict"),
        }
    }

    #[test]
    fn test_crossing_describe() {
        let crossing = Crossing::trait_conflict(
            "src/lib.rs:42",
            0,
            1,
            "AsyncRead",
            "tokio::io::AsyncRead",
            "smol::io::AsyncRead",
        );

        let desc = crossing.describe();
        assert!(desc.contains("AsyncRead"));
        assert!(desc.contains("tokio"));
        assert!(desc.contains("smol"));
    }

    #[test]
    fn test_extracted_bounds_conflict() {
        let mut bounds_a = ExtractedBounds::default();
        bounds_a
            .trait_bounds
            .insert("tokio::io::AsyncRead".to_string());
        bounds_a.markers.send = Ternary::Yes;

        let mut bounds_b = ExtractedBounds::default();
        bounds_b
            .trait_bounds
            .insert("smol::io::AsyncRead".to_string());
        bounds_b.markers.send = Ternary::Yes;

        let conflicts = bounds_a.find_conflicts(&bounds_b);
        assert!(!conflicts.is_empty());
    }
}
