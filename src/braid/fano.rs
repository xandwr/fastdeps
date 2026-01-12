//! Fano plane constraints for octonion coordinate compatibility.
//!
//! The Fano plane is a projective plane with 7 points and 7 lines,
//! where each line contains exactly 3 points. We map this to our 8D
//! octonion space (7 imaginary units + 1 real) to define compatibility
//! constraints.
//!
//! When two crates' coordinates satisfy a Fano line constraint, their
//! composition is "algebraically clean" - the crossing can be resolved
//! by a type isomorphism.
//!
//! Fano plane structure:
//! ```text
//!        e1
//!       /  \
//!      /    \
//!     e2----e4
//!    /|\    /|\
//!   / | \  / | \
//!  e3-+-e7-+-e5
//!     |    |
//!     e6---+
//! ```
//!
//! Lines (any 3 collinear points multiply to -1 in octonion algebra):
//! - {e1, e2, e4}
//! - {e2, e3, e7}
//! - {e1, e3, e5}
//! - {e1, e6, e7}
//! - {e4, e5, e7}
//! - {e2, e5, e6}
//! - {e3, e4, e6}

/// A line in the Fano plane - three points that are collinear.
#[derive(Debug, Clone, Copy)]
pub struct FanoLine {
    /// The three dimension indices that form this line (1-7, not 0)
    pub points: [usize; 3],
    /// Semantic meaning of this constraint
    pub meaning: &'static str,
}

/// The 7 lines of the Fano plane, mapped to octonion dimensions.
pub const FANO_LINES: [FanoLine; 7] = [
    FanoLine {
        points: [1, 2, 4], // Concurrency, Safety, Memory
        meaning: "Thread-safe memory access",
    },
    FanoLine {
        points: [2, 3, 7], // Safety, Async, Entropy
        meaning: "Safe async stability",
    },
    FanoLine {
        points: [1, 3, 5], // Concurrency, Async, Friction
        meaning: "Lightweight concurrent async",
    },
    FanoLine {
        points: [1, 6, 7], // Concurrency, Environment, Entropy
        meaning: "Stable concurrent environment",
    },
    FanoLine {
        points: [4, 5, 7], // Memory, Friction, Entropy
        meaning: "Low-churn memory patterns",
    },
    FanoLine {
        points: [2, 5, 6], // Safety, Friction, Environment
        meaning: "Safe minimal no_std",
    },
    FanoLine {
        points: [3, 4, 6], // Async, Memory, Environment
        meaning: "Async memory in environment",
    },
];

/// A constraint derived from the Fano plane structure.
#[derive(Debug, Clone)]
pub struct FanoConstraint {
    /// Which Fano line this constraint comes from
    pub line: FanoLine,
    /// Threshold for "satisfied" (sum should be close to this)
    pub threshold: f32,
}

impl FanoConstraint {
    /// Check if two coordinate sets satisfy this constraint.
    /// Returns a score from 0.0 (violated) to 1.0 (satisfied).
    pub fn check(&self, coords_a: &[f32; 8], coords_b: &[f32; 8]) -> f32 {
        // Compute the "parity" - product of differences along the line
        let mut product = 1.0f32;
        let mut sum = 0.0f32;

        for &dim in &self.line.points {
            let diff = (coords_a[dim] - coords_b[dim]).abs();
            product *= 1.0 - diff; // High diff = low product
            sum += coords_a[dim] * coords_b[dim]; // Cosine-like
        }

        // Combine: we want low difference AND positive correlation
        let diff_score = product;
        let corr_score = (sum / 3.0).clamp(0.0, 1.0);

        (diff_score + corr_score) / 2.0
    }

    /// Check if coordinates are "on the line" (all three dims are active).
    pub fn is_active(&self, coords: &[f32; 8]) -> bool {
        self.line.points.iter().all(|&dim| coords[dim] > 0.3)
    }
}

/// Check all Fano line constraints between two coordinate sets.
/// Returns the minimum score (worst constraint).
pub fn check_all_lines(coords_a: &[f32; 8], coords_b: &[f32; 8]) -> f32 {
    let mut min_score = 1.0f32;

    for line in &FANO_LINES {
        let constraint = FanoConstraint {
            line: *line,
            threshold: 0.5,
        };

        // Only check constraints that are "active" for both crates
        if constraint.is_active(coords_a) || constraint.is_active(coords_b) {
            let score = constraint.check(coords_a, coords_b);
            min_score = min_score.min(score);
        }
    }

    min_score
}

/// Find the most violated constraint between two crates.
pub fn find_worst_violation(coords_a: &[f32; 8], coords_b: &[f32; 8]) -> Option<(FanoLine, f32)> {
    let mut worst: Option<(FanoLine, f32)> = None;

    for line in &FANO_LINES {
        let constraint = FanoConstraint {
            line: *line,
            threshold: 0.5,
        };

        if constraint.is_active(coords_a) || constraint.is_active(coords_b) {
            let score = constraint.check(coords_a, coords_b);
            match &worst {
                None => worst = Some((*line, score)),
                Some((_, worst_score)) if score < *worst_score => {
                    worst = Some((*line, score));
                }
                _ => {}
            }
        }
    }

    worst
}

/// Compute the "octonion parity" - whether the crossing is Real or Imaginary.
///
/// In true octonion algebra, e_i * e_j * e_k = ±1 depending on the line.
/// We approximate this: if the coordinates "sum to an integer" on active
/// lines, the crossing is Real (resolvable). Otherwise, Imaginary (essential).
pub fn octonion_parity(coords_a: &[f32; 8], coords_b: &[f32; 8]) -> OctonionParity {
    // e0 is the "real" component - both should have high utility
    let real_component = coords_a[0] * coords_b[0];

    // Imaginary components: check if their interaction is "clean"
    let mut imaginary_sum = 0.0f32;
    for i in 1..8 {
        imaginary_sum += coords_a[i] * coords_b[i];
    }

    // If imaginary sum is close to an integer, we can "flatten" the braid
    let fractional_part = (imaginary_sum - imaginary_sum.round()).abs();

    if fractional_part < 0.2 && real_component > 0.3 {
        OctonionParity::Real {
            confidence: 1.0 - fractional_part,
        }
    } else {
        OctonionParity::Imaginary {
            residue: fractional_part,
        }
    }
}

/// The parity of an octonion crossing.
#[derive(Debug, Clone)]
pub enum OctonionParity {
    /// Crossing can be resolved - coordinates "sum to Real"
    Real { confidence: f32 },
    /// Crossing is essential - has Imaginary residue
    Imaginary { residue: f32 },
}

impl OctonionParity {
    pub fn is_real(&self) -> bool {
        matches!(self, OctonionParity::Real { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fano_constraint_similar_crates() {
        // Two similar async crates should score high
        let tokio = [0.9, 0.9, 0.1, 0.95, 0.4, 0.3, 0.0, 0.2];
        let async_std = [0.8, 0.85, 0.1, 0.9, 0.35, 0.25, 0.0, 0.25];

        let score = check_all_lines(&tokio, &async_std);
        println!("Similar async crates score: {}", score);
        assert!(score > 0.5, "Similar crates should be compatible");
    }

    #[test]
    fn test_fano_constraint_different_crates() {
        // Async runtime vs no_std embedded - should score lower
        let tokio = [0.9, 0.9, 0.1, 0.95, 0.4, 0.3, 0.0, 0.2];
        let embedded = [0.3, 0.1, 0.2, 0.3, 0.0, 0.05, 1.0, 0.1];

        let score = check_all_lines(&tokio, &embedded);
        println!("Tokio vs embedded score: {}", score);
        // The e6 (environment) difference should hurt the score
    }

    #[test]
    fn test_octonion_parity() {
        // Compatible crates: should be Real
        let a = [0.8, 0.5, 0.2, 0.6, 0.3, 0.2, 0.0, 0.1];
        let b = [0.7, 0.6, 0.2, 0.5, 0.3, 0.2, 0.0, 0.1];

        let parity = octonion_parity(&a, &b);
        println!("Parity: {:?}", parity);

        // Incompatible: high values in different dimensions
        let c = [0.9, 0.9, 0.0, 0.9, 0.0, 0.0, 0.0, 0.0]; // Async-heavy
        let d = [0.3, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0]; // no_std only

        let parity2 = octonion_parity(&c, &d);
        println!("Incompatible parity: {:?}", parity2);
    }

    #[test]
    fn test_find_worst_violation() {
        let tokio = [0.9, 0.9, 0.1, 0.95, 0.4, 0.3, 0.0, 0.2];
        let rc_local = [0.5, 0.0, 0.1, 0.2, 0.8, 0.1, 0.0, 0.1]; // Thread-local heavy

        if let Some((line, score)) = find_worst_violation(&tokio, &rc_local) {
            println!(
                "Worst violation: {:?} (score {}): {}",
                line.points, score, line.meaning
            );
        }
    }
}
