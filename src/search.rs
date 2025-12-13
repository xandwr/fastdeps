//! Smart search with fuzzy matching, scoring, and pagination.

use crate::cache::Cache;
use crate::cargo::{RegistryCrate, get_direct_dep_names, resolve_project_deps};
use camino::{Utf8Path, Utf8PathBuf};
use std::collections::HashSet;

/// A scored search result with metadata.
#[derive(Debug, Clone)]
pub struct ScoredResult {
    pub crate_name: String,
    pub crate_version: String,
    pub path: String,
    pub kind: String,
    pub signature: Option<String>,
    pub score: u32,
    pub is_direct_dep: bool,
    pub match_type: MatchType,
}

/// How the result matched the query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchType {
    /// Exact match on item name
    Exact,
    /// Item name starts with query
    Prefix,
    /// Item name contains query
    Contains,
    /// Fuzzy match with edit distance
    Fuzzy { distance: usize },
    /// Crate name match (for crate queries like "bevy")
    CrateName,
    /// Crate prefix match (bevy -> bevy_ecs)
    CratePrefix,
}

impl std::fmt::Display for MatchType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MatchType::Exact => write!(f, "exact"),
            MatchType::Prefix => write!(f, "prefix"),
            MatchType::Contains => write!(f, "contains"),
            MatchType::Fuzzy { distance } => write!(f, "fuzzy~{}", distance),
            MatchType::CrateName => write!(f, "crate"),
            MatchType::CratePrefix => write!(f, "crate_prefix"),
        }
    }
}

/// Pagination info for results.
#[derive(Debug, Clone)]
pub struct Pagination {
    pub offset: usize,
    pub limit: usize,
    pub total: usize,
}

impl Pagination {
    pub fn has_more(&self) -> bool {
        self.offset + self.limit < self.total
    }

    pub fn next_offset(&self) -> usize {
        self.offset + self.limit
    }
}

/// Search options.
#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    /// Filter to specific crate(s)
    pub crate_filter: Option<String>,
    /// Maximum results to return
    pub limit: usize,
    /// Offset for pagination
    pub offset: usize,
    /// Enable fuzzy matching
    pub fuzzy: bool,
    /// Maximum edit distance for fuzzy matching
    pub max_edit_distance: usize,
    /// Only show direct dependencies
    pub direct_only: bool,
    /// Filter by item kind (struct, trait, function, etc.)
    pub kind_filter: Option<String>,
}

impl SearchOptions {
    pub fn new() -> Self {
        Self {
            limit: 25,
            offset: 0,
            fuzzy: true,
            max_edit_distance: 2,
            ..Default::default()
        }
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    pub fn with_offset(mut self, offset: usize) -> Self {
        self.offset = offset;
        self
    }

    pub fn with_crate(mut self, crate_name: &str) -> Self {
        self.crate_filter = Some(crate_name.to_string());
        self
    }

    pub fn direct_only(mut self) -> Self {
        self.direct_only = true;
        self
    }
}

/// Search response with results and metadata.
#[derive(Debug)]
pub struct SearchResponse {
    pub results: Vec<ScoredResult>,
    pub pagination: Pagination,
    /// Suggested alternative queries if few/no results
    pub suggestions: Vec<String>,
    /// Related crates (for crate-level queries like "bevy")
    pub related_crates: Vec<RelatedCrate>,
}

/// A related crate (e.g., bevy_ecs when searching for bevy).
#[derive(Debug, Clone)]
pub struct RelatedCrate {
    pub name: String,
    pub version: String,
    pub item_count: usize,
    pub relationship: CrateRelationship,
}

#[derive(Debug, Clone, Copy)]
pub enum CrateRelationship {
    /// The crate itself
    Direct,
    /// Crate with matching prefix (bevy -> bevy_ecs)
    Prefix,
    /// Re-exported by the queried crate
    ReExport,
}

impl std::fmt::Display for CrateRelationship {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CrateRelationship::Direct => write!(f, "direct"),
            CrateRelationship::Prefix => write!(f, "prefix"),
            CrateRelationship::ReExport => write!(f, "re-export"),
        }
    }
}

/// Smart search engine.
pub struct SearchEngine {
    project_dir: Utf8PathBuf,
    direct_deps: HashSet<String>,
    all_deps: Vec<RegistryCrate>,
}

impl SearchEngine {
    pub fn new(project_dir: &Utf8Path) -> Result<Self, String> {
        let all_deps = resolve_project_deps(project_dir, false).map_err(|e| e.to_string())?;
        let direct_deps = get_direct_dep_names(project_dir).map_err(|e| e.to_string())?;

        Ok(Self {
            project_dir: project_dir.to_owned(),
            direct_deps,
            all_deps,
        })
    }

    /// Search for symbols with smart matching and scoring.
    pub fn search(&self, query: &str, options: &SearchOptions) -> Result<SearchResponse, String> {
        let query_lower = query.to_lowercase();

        // Check if this looks like a crate-level query
        let is_crate_query = self.is_crate_query(&query_lower);

        let mut all_results = Vec::new();
        let mut related_crates = Vec::new();

        // If it's a crate query, find related crates first
        if is_crate_query {
            related_crates = self.find_related_crates(&query_lower);
        }

        // Get results from cache if available
        if Cache::exists() {
            if let Ok(cache) = Cache::open_existing() {
                // Get raw results
                let raw_results = if let Some(ref crate_filter) = options.crate_filter {
                    // Search within specific crate
                    self.search_in_crate(&cache, crate_filter, &query_lower, options)?
                } else {
                    // Search across all deps
                    self.search_all(&cache, &query_lower, options)?
                };

                all_results = raw_results;
            }
        }

        // If we have few results and fuzzy is enabled, try fuzzy matching
        if all_results.len() < 5 && options.fuzzy {
            let fuzzy_results = self.fuzzy_search(&query_lower, options)?;

            // Add fuzzy results that aren't already in all_results
            let existing: HashSet<(String, String)> = all_results
                .iter()
                .map(|r| (r.crate_name.clone(), r.path.clone()))
                .collect();

            for result in fuzzy_results {
                if !existing.contains(&(result.crate_name.clone(), result.path.clone())) {
                    all_results.push(result);
                }
            }
        }

        // Sort by score (descending), then by path
        all_results.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.path.cmp(&b.path)));

        // Build suggestions if few results
        let suggestions = if all_results.len() < 3 {
            self.build_suggestions(&query_lower)
        } else {
            vec![]
        };

        let total = all_results.len();

        // Apply pagination
        let paginated: Vec<_> = all_results
            .into_iter()
            .skip(options.offset)
            .take(options.limit)
            .collect();

        Ok(SearchResponse {
            results: paginated,
            pagination: Pagination {
                offset: options.offset,
                limit: options.limit,
                total,
            },
            suggestions,
            related_crates,
        })
    }

    /// Check if a query looks like a crate name (no ::, lowercase, matches crate pattern).
    fn is_crate_query(&self, query: &str) -> bool {
        !query.contains("::")
            && query
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
            && self
                .all_deps
                .iter()
                .any(|d| d.name == query || d.name.starts_with(&format!("{}_", query)))
    }

    /// Find related crates for a crate-level query.
    fn find_related_crates(&self, query: &str) -> Vec<RelatedCrate> {
        let mut related = Vec::new();
        let query_prefix = format!("{}_", query);

        for dep in &self.all_deps {
            let relationship = if dep.name == query {
                Some(CrateRelationship::Direct)
            } else if dep.name.starts_with(&query_prefix) {
                Some(CrateRelationship::Prefix)
            } else {
                None
            };

            if let Some(rel) = relationship {
                // Get item count from cache if possible
                let item_count = if Cache::exists() {
                    Cache::open_existing()
                        .ok()
                        .and_then(|cache| cache.search_crate(&dep.name, Some(&dep.version)).ok())
                        .map(|items| items.len())
                        .unwrap_or(0)
                } else {
                    0
                };

                related.push(RelatedCrate {
                    name: dep.name.clone(),
                    version: dep.version.clone(),
                    item_count,
                    relationship: rel,
                });
            }
        }

        // Sort: direct first, then by item count (descending)
        related.sort_by(|a, b| match (&a.relationship, &b.relationship) {
            (CrateRelationship::Direct, CrateRelationship::Direct) => std::cmp::Ordering::Equal,
            (CrateRelationship::Direct, _) => std::cmp::Ordering::Less,
            (_, CrateRelationship::Direct) => std::cmp::Ordering::Greater,
            _ => b.item_count.cmp(&a.item_count),
        });

        related
    }

    /// Search within a specific crate.
    fn search_in_crate(
        &self,
        cache: &Cache,
        crate_name: &str,
        query: &str,
        options: &SearchOptions,
    ) -> Result<Vec<ScoredResult>, String> {
        let items = cache
            .search_crate(crate_name, None)
            .map_err(|e| e.to_string())?;

        let is_direct = self.direct_deps.contains(crate_name);

        let results: Vec<_> = items
            .into_iter()
            .filter_map(|item| {
                // Apply kind filter
                if let Some(ref kind) = options.kind_filter {
                    if !item.kind.eq_ignore_ascii_case(kind) {
                        return None;
                    }
                }

                let (score, match_type) = self.score_item(&item.path, query);
                if score > 0 {
                    Some(ScoredResult {
                        crate_name: crate_name.to_string(),
                        crate_version: String::new(), // TODO: get from cache
                        path: item.path,
                        kind: item.kind,
                        signature: item.signature,
                        score: if is_direct { score + 10 } else { score },
                        is_direct_dep: is_direct,
                        match_type,
                    })
                } else {
                    None
                }
            })
            .collect();

        Ok(results)
    }

    /// Search across all dependencies.
    fn search_all(
        &self,
        cache: &Cache,
        query: &str,
        options: &SearchOptions,
    ) -> Result<Vec<ScoredResult>, String> {
        // Use FTS search for efficiency
        let raw_results = cache.search(query).map_err(|e| e.to_string())?;

        // Build dep set for filtering
        let dep_set: HashSet<_> = self
            .all_deps
            .iter()
            .map(|d| (d.name.as_str(), d.version.as_str()))
            .collect();

        let results: Vec<_> = raw_results
            .into_iter()
            .filter(|r| dep_set.contains(&(r.crate_name.as_str(), r.crate_version.as_str())))
            .filter(|r| {
                // Apply direct-only filter
                if options.direct_only && !self.direct_deps.contains(&r.crate_name) {
                    return false;
                }
                // Apply kind filter
                if let Some(ref kind) = options.kind_filter {
                    if !r.kind.eq_ignore_ascii_case(kind) {
                        return false;
                    }
                }
                true
            })
            .map(|r| {
                let is_direct = self.direct_deps.contains(&r.crate_name);
                let (score, match_type) = self.score_item(&r.path, query);
                ScoredResult {
                    crate_name: r.crate_name,
                    crate_version: r.crate_version,
                    path: r.path,
                    kind: r.kind,
                    signature: r.signature,
                    score: if is_direct { score + 10 } else { score },
                    is_direct_dep: is_direct,
                    match_type,
                }
            })
            .collect();

        Ok(results)
    }

    /// Score an item path against a query.
    fn score_item(&self, path: &str, query: &str) -> (u32, MatchType) {
        let path_lower = path.to_lowercase();
        let query_lower = query.to_lowercase();

        // Extract the item name (last component)
        let item_name = path_lower.split("::").last().unwrap_or(&path_lower);

        // Exact match on item name
        if item_name == query_lower {
            return (100, MatchType::Exact);
        }

        // Prefix match
        if item_name.starts_with(&query_lower) {
            let score = 80 - (item_name.len() - query_lower.len()).min(20) as u32;
            return (score, MatchType::Prefix);
        }

        // Contains match
        if item_name.contains(&query_lower) {
            return (50, MatchType::Contains);
        }

        // Full path contains
        if path_lower.contains(&query_lower) {
            return (30, MatchType::Contains);
        }

        (0, MatchType::Contains)
    }

    /// Perform fuzzy search using edit distance.
    fn fuzzy_search(
        &self,
        query: &str,
        options: &SearchOptions,
    ) -> Result<Vec<ScoredResult>, String> {
        let mut results = Vec::new();

        if !Cache::exists() {
            return Ok(results);
        }

        let cache = Cache::open_existing().map_err(|e| e.to_string())?;

        // Get all indexed items and check edit distance
        // This is expensive, so we limit the scope
        for dep in &self.all_deps {
            if options.direct_only && !self.direct_deps.contains(&dep.name) {
                continue;
            }

            if let Some(ref filter) = options.crate_filter {
                if &dep.name != filter {
                    continue;
                }
            }

            if let Ok(items) = cache.search_crate(&dep.name, Some(&dep.version)) {
                for item in items {
                    let item_name = item.path.split("::").last().unwrap_or(&item.path);
                    let distance = levenshtein(query, &item_name.to_lowercase());

                    if distance <= options.max_edit_distance {
                        let score = (100 - distance * 25).max(10) as u32;
                        results.push(ScoredResult {
                            crate_name: dep.name.clone(),
                            crate_version: dep.version.clone(),
                            path: item.path,
                            kind: item.kind,
                            signature: item.signature,
                            score,
                            is_direct_dep: self.direct_deps.contains(&dep.name),
                            match_type: MatchType::Fuzzy { distance },
                        });
                    }
                }
            }
        }

        Ok(results)
    }

    /// Build query suggestions.
    fn build_suggestions(&self, query: &str) -> Vec<String> {
        let mut suggestions = Vec::new();

        // Suggest crates with matching prefix
        for dep in &self.all_deps {
            if dep.name.contains(query) && dep.name != query {
                suggestions.push(format!("crate:{}", dep.name));
            }
        }

        // Common typo corrections could go here

        suggestions.truncate(5);
        suggestions
    }

    /// Get crate info for peek command (detects re-export crates).
    pub fn get_crate_info(&self, crate_name: &str) -> Result<CrateInfo, String> {
        let dep = self
            .all_deps
            .iter()
            .find(|d| d.name == crate_name)
            .ok_or_else(|| format!("Crate '{}' not found", crate_name))?;

        // Check if this is a re-export crate by looking at lib.rs
        let is_reexport = self.detect_reexport_crate(dep);

        // Get item count
        let item_count = if Cache::exists() {
            Cache::open_existing()
                .ok()
                .and_then(|cache| cache.search_crate(crate_name, Some(&dep.version)).ok())
                .map(|items| items.len())
                .unwrap_or(0)
        } else {
            0
        };

        // Find related crates
        let related = self.find_related_crates(crate_name);

        Ok(CrateInfo {
            name: dep.name.clone(),
            version: dep.version.clone(),
            path: dep.path.clone(),
            item_count,
            is_reexport,
            is_direct_dep: self.direct_deps.contains(crate_name),
            related_crates: related,
        })
    }

    /// Detect if a crate is a thin re-export wrapper.
    fn detect_reexport_crate(&self, dep: &RegistryCrate) -> bool {
        let lib_path = dep.path.join("src/lib.rs");
        if let Ok(content) = std::fs::read_to_string(&lib_path) {
            // Check for patterns like `pub use other_crate::*;` or re-export patterns
            let lines: Vec<_> = content
                .lines()
                .filter(|l| {
                    !l.trim().starts_with("//")
                        && !l.trim().starts_with("#")
                        && !l.trim().is_empty()
                })
                .collect();

            // If the file is very short and mostly re-exports, it's a wrapper
            if lines.len() < 20 {
                let reexport_count = lines
                    .iter()
                    .filter(|l| l.contains("pub use") || l.contains("pub extern crate"))
                    .count();

                // If more than half the code lines are re-exports
                if reexport_count > 0 && reexport_count >= lines.len() / 2 {
                    return true;
                }
            }
        }
        false
    }
}

/// Information about a crate.
#[derive(Debug)]
pub struct CrateInfo {
    pub name: String,
    pub version: String,
    pub path: Utf8PathBuf,
    pub item_count: usize,
    pub is_reexport: bool,
    pub is_direct_dep: bool,
    pub related_crates: Vec<RelatedCrate>,
}

/// Levenshtein edit distance for fuzzy matching.
fn levenshtein(a: &str, b: &str) -> usize {
    let a_chars: Vec<_> = a.chars().collect();
    let b_chars: Vec<_> = b.chars().collect();

    let m = a_chars.len();
    let n = b_chars.len();

    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }

    let mut dp = vec![vec![0; n + 1]; m + 1];

    for i in 0..=m {
        dp[i][0] = i;
    }
    for j in 0..=n {
        dp[0][j] = j;
    }

    for i in 1..=m {
        for j in 1..=n {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            dp[i][j] = (dp[i - 1][j] + 1)
                .min(dp[i][j - 1] + 1)
                .min(dp[i - 1][j - 1] + cost);
        }
    }

    dp[m][n]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_levenshtein() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", "abc"), 0);
        assert_eq!(levenshtein("DistnceFog", "DistanceFog"), 1);
    }

    #[test]
    fn test_score_item() {
        let engine = SearchEngine {
            project_dir: Utf8PathBuf::from("."),
            direct_deps: HashSet::new(),
            all_deps: vec![],
        };

        let (score, match_type) = engine.score_item("serde::Serialize", "serialize");
        assert_eq!(match_type, MatchType::Exact);
        assert_eq!(score, 100);

        let (score, match_type) = engine.score_item("serde::Serializer", "serial");
        assert_eq!(match_type, MatchType::Prefix);
        assert!(score > 50);

        let (score, match_type) = engine.score_item("serde::de::Deserialize", "serial");
        assert_eq!(match_type, MatchType::Contains);
    }
}
