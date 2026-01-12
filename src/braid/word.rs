//! Braid word representation and Yang-Baxter rewriting.
//!
//! A braid word represents the composition order of crate dependencies.
//! Each generator σᵢ represents a crate "strand", and the order encodes
//! how types flow through wrapper nesting.
//!
//! The Yang-Baxter relation σᵢσᵢ₊₁σᵢ = σᵢ₊₁σᵢσᵢ₊₁ provides rewrite rules
//! for finding equivalent orderings that may resolve type conflicts.

use crate::octo_index::OctonionProfile;

/// A generator in the braid group - represents one crate's "strand".
#[derive(Debug, Clone)]
pub struct BraidGenerator {
    /// Index in the braid (position in Cargo.toml order)
    pub index: usize,
    /// Crate name
    pub name: String,
    /// Octonion profile (if available)
    pub profile: Option<[f32; 8]>,
    /// Whether this is an inverse generator (σ⁻¹)
    pub inverse: bool,
}

impl BraidGenerator {
    pub fn new(index: usize, name: impl Into<String>) -> Self {
        Self {
            index,
            name: name.into(),
            profile: None,
            inverse: false,
        }
    }

    pub fn with_profile(mut self, coeffs: [f32; 8]) -> Self {
        self.profile = Some(coeffs);
        self
    }

    pub fn invert(mut self) -> Self {
        self.inverse = !self.inverse;
        self
    }

    /// Symbol representation: σᵢ or σᵢ⁻¹
    pub fn symbol(&self) -> String {
        if self.inverse {
            format!("σ{}⁻¹", self.index)
        } else {
            format!("σ{}", self.index)
        }
    }
}

/// A braid word - sequence of generators representing dependency composition.
#[derive(Debug, Clone, Default)]
pub struct BraidWord {
    /// Ordered sequence of generators
    pub generators: Vec<BraidGenerator>,
}

impl BraidWord {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create from Cargo.toml dependency list.
    pub fn from_deps(deps: &[(String, Option<OctonionProfile>)]) -> Self {
        let generators = deps
            .iter()
            .enumerate()
            .map(|(i, (name, profile))| {
                let mut g = BraidGenerator::new(i, name.clone());
                if let Some(p) = profile {
                    g = g.with_profile(p.coeffs);
                }
                g
            })
            .collect();

        Self { generators }
    }

    /// Push a generator onto the word.
    pub fn push(&mut self, generator: BraidGenerator) {
        self.generators.push(generator);
    }

    /// Apply a single Yang-Baxter rewrite at position i.
    /// σᵢσᵢ₊₁σᵢ → σᵢ₊₁σᵢσᵢ₊₁
    ///
    /// Returns true if the rewrite was applied.
    pub fn yang_baxter_rewrite(&mut self, pos: usize) -> bool {
        if pos + 2 >= self.generators.len() {
            return false;
        }

        let a = &self.generators[pos];
        let b = &self.generators[pos + 1];
        let c = &self.generators[pos + 2];

        // Check pattern: σᵢσᵢ₊₁σᵢ (indices must be adjacent)
        let i = a.index;
        let j = b.index;
        let k = c.index;

        if i == k && (j == i + 1 || j + 1 == i) && !a.inverse && !b.inverse && !c.inverse {
            // Apply rewrite: swap the pattern
            // σᵢσᵢ₊₁σᵢ → σᵢ₊₁σᵢσᵢ₊₁
            let new_a = BraidGenerator {
                index: j,
                name: b.name.clone(),
                profile: b.profile,
                inverse: false,
            };
            let new_b = BraidGenerator {
                index: i,
                name: a.name.clone(),
                profile: a.profile,
                inverse: false,
            };
            let new_c = BraidGenerator {
                index: j,
                name: b.name.clone(),
                profile: b.profile,
                inverse: false,
            };

            self.generators[pos] = new_a;
            self.generators[pos + 1] = new_b;
            self.generators[pos + 2] = new_c;

            return true;
        }

        false
    }

    /// Apply commutation relation: σᵢσⱼ = σⱼσᵢ when |i-j| > 1
    pub fn commute(&mut self, pos: usize) -> bool {
        if pos + 1 >= self.generators.len() {
            return false;
        }

        let a = &self.generators[pos];
        let b = &self.generators[pos + 1];

        // Can commute if indices are far apart
        let diff = (a.index as i32 - b.index as i32).abs();
        if diff > 1 {
            self.generators.swap(pos, pos + 1);
            return true;
        }

        false
    }

    /// Cancel adjacent inverse pairs: σᵢσᵢ⁻¹ → ε
    pub fn cancel_inverses(&mut self) {
        let mut i = 0;
        while i + 1 < self.generators.len() {
            let a = &self.generators[i];
            let b = &self.generators[i + 1];

            if a.index == b.index && a.inverse != b.inverse {
                // Remove both
                self.generators.remove(i);
                self.generators.remove(i);
                // Don't increment i - check new pair at same position
            } else {
                i += 1;
            }
        }
    }

    /// Normalize the braid word using available relations.
    /// Returns the number of rewrites applied.
    pub fn normalize(&mut self) -> usize {
        let mut rewrites = 0;
        let mut changed = true;

        while changed {
            changed = false;

            // Cancel inverses
            let old_len = self.generators.len();
            self.cancel_inverses();
            if self.generators.len() < old_len {
                rewrites += (old_len - self.generators.len()) / 2;
                changed = true;
            }

            // Try commutations to bring related generators together
            for i in 0..self.generators.len().saturating_sub(1) {
                if self.commute(i) {
                    rewrites += 1;
                    changed = true;
                    break;
                }
            }
        }

        rewrites
    }

    /// Find positions where Yang-Baxter can potentially untangle.
    pub fn find_tangle_points(&self) -> Vec<usize> {
        let mut tangles = Vec::new();

        for i in 0..self.generators.len().saturating_sub(2) {
            let a = &self.generators[i];
            let b = &self.generators[i + 1];
            let c = &self.generators[i + 2];

            // Check if this looks like a tangle (same crate appears twice around another)
            if a.index == c.index {
                tangles.push(i);
            }
            // Also check for profile conflicts at adjacent positions
            if let (Some(pa), Some(pb)) = (&a.profile, &b.profile) {
                // High async + high async = potential runtime conflict
                if pa[3] > 0.7 && pb[3] > 0.7 {
                    tangles.push(i);
                }
            }
        }

        tangles
    }

    /// Render as symbolic string
    pub fn to_string(&self) -> String {
        if self.generators.is_empty() {
            return "ε".to_string();
        }
        self.generators
            .iter()
            .map(|g| g.symbol())
            .collect::<Vec<_>>()
            .join("")
    }

    /// Render with crate names
    pub fn to_named_string(&self) -> String {
        if self.generators.is_empty() {
            return "ε".to_string();
        }
        self.generators
            .iter()
            .map(|g| {
                if g.inverse {
                    format!("{}⁻¹", g.name)
                } else {
                    g.name.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" → ")
    }
}

impl std::fmt::Display for BraidWord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_braid_word_creation() {
        let deps = vec![
            ("tokio".to_string(), None),
            ("serde".to_string(), None),
            ("hyper".to_string(), None),
        ];

        let word = BraidWord::from_deps(&deps);
        assert_eq!(word.generators.len(), 3);
        assert_eq!(word.to_string(), "σ0σ1σ2");
        assert_eq!(word.to_named_string(), "tokio → serde → hyper");
    }

    #[test]
    fn test_commutation() {
        let mut word = BraidWord::new();
        word.push(BraidGenerator::new(0, "a"));
        word.push(BraidGenerator::new(2, "c")); // Can commute with σ0 since |2-0| > 1
        word.push(BraidGenerator::new(1, "b"));

        assert!(word.commute(0)); // σ0σ2 → σ2σ0
        assert_eq!(word.generators[0].index, 2);
        assert_eq!(word.generators[1].index, 0);
    }

    #[test]
    fn test_inverse_cancellation() {
        let mut word = BraidWord::new();
        word.push(BraidGenerator::new(0, "a"));
        word.push(BraidGenerator::new(1, "b"));
        word.push(BraidGenerator::new(1, "b").invert()); // σ1⁻¹
        word.push(BraidGenerator::new(2, "c"));

        word.cancel_inverses();
        assert_eq!(word.generators.len(), 2);
        assert_eq!(word.generators[0].name, "a");
        assert_eq!(word.generators[1].name, "c");
    }

    #[test]
    fn test_tangle_detection() {
        let mut word = BraidWord::new();
        // Pattern: σ0σ1σ0 (same crate wrapping around another)
        word.push(BraidGenerator::new(0, "tokio"));
        word.push(BraidGenerator::new(1, "hyper"));
        word.push(BraidGenerator::new(0, "tokio")); // Tangle!

        let tangles = word.find_tangle_points();
        assert!(!tangles.is_empty());
        assert_eq!(tangles[0], 0);
    }
}
