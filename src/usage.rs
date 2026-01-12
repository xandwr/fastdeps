//! Analyze how a crate is used within the current project.
//!
//! Scans the project's source files to find:
//! - `use` statements importing from the crate
//! - Direct references to crate items
//! - Trait implementations using crate traits
//! - Derives using crate macros

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use syn::{Attribute, Item, UseTree};

/// A usage site in the project's code
#[derive(Debug, Clone)]
pub struct UsageSite {
    /// File where the usage occurs
    pub file: PathBuf,
    /// Line number (1-indexed)
    pub line: usize,
    /// The imported/used path (e.g., "serde::Deserialize")
    pub path: String,
    /// Kind of usage
    pub kind: UsageKind,
    /// Context around the usage
    pub context: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UsageKind {
    /// `use crate_name::...`
    Import,
    /// `#[derive(Trait)]` where Trait is from the crate
    Derive,
    /// `impl Trait for ...` where Trait is from the crate
    TraitImpl,
    /// `#[macro]` attribute from the crate
    AttributeMacro,
    /// `macro!()` invocation
    MacroCall,
}

/// Summary of how a crate is used in the project
#[derive(Debug)]
pub struct UsageSummary {
    pub crate_name: String,
    pub sites: Vec<UsageSite>,
    /// Most commonly imported items
    pub top_imports: Vec<(String, usize)>,
    /// Derives used
    pub derives_used: Vec<String>,
}

impl UsageSummary {
    /// Get a human-readable summary
    pub fn format(&self) -> String {
        let mut lines = Vec::new();

        lines.push(format!("Usage of '{}' in this project:", self.crate_name));
        lines.push(String::new());

        if !self.top_imports.is_empty() {
            lines.push("Imports:".to_string());
            for (path, count) in &self.top_imports {
                lines.push(format!("  {} (used {} times)", path, count));
            }
            lines.push(String::new());
        }

        if !self.derives_used.is_empty() {
            lines.push(format!("Derives: {}", self.derives_used.join(", ")));
            lines.push(String::new());
        }

        // Group by file
        let mut by_file: std::collections::HashMap<PathBuf, Vec<&UsageSite>> =
            std::collections::HashMap::new();
        for site in &self.sites {
            by_file.entry(site.file.clone()).or_default().push(site);
        }

        lines.push("Usage sites:".to_string());
        for (file, sites) in by_file {
            lines.push(format!("  {}:", file.display()));
            for site in sites.iter().take(5) {
                lines.push(format!(
                    "    L{}: {:?} - {}",
                    site.line, site.kind, site.path
                ));
            }
            if sites.len() > 5 {
                lines.push(format!("    ... and {} more", sites.len() - 5));
            }
        }

        lines.join("\n")
    }
}

/// Analyze usage of a specific crate in a project
pub fn analyze_usage(project_root: &Path, crate_name: &str) -> Result<UsageSummary> {
    let src_dir = project_root.join("src");
    if !src_dir.exists() {
        anyhow::bail!("No src directory found in project");
    }

    let mut sites = Vec::new();
    scan_directory(&src_dir, crate_name, &mut sites)?;

    // Count imports
    let mut import_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut derives = std::collections::HashSet::new();

    for site in &sites {
        match site.kind {
            UsageKind::Import => {
                *import_counts.entry(site.path.clone()).or_insert(0) += 1;
            }
            UsageKind::Derive => {
                derives.insert(site.path.clone());
            }
            _ => {}
        }
    }

    let mut top_imports: Vec<(String, usize)> = import_counts.into_iter().collect();
    top_imports.sort_by(|a, b| b.1.cmp(&a.1));
    top_imports.truncate(10);

    Ok(UsageSummary {
        crate_name: crate_name.to_string(),
        sites,
        top_imports,
        derives_used: derives.into_iter().collect(),
    })
}

fn scan_directory(dir: &Path, crate_name: &str, sites: &mut Vec<UsageSite>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_file() && path.extension().map(|e| e == "rs").unwrap_or(false) {
            let _ = scan_file(&path, crate_name, sites);
        } else if path.is_dir() {
            let _ = scan_directory(&path, crate_name, sites);
        }
    }
    Ok(())
}

fn scan_file(path: &Path, crate_name: &str, sites: &mut Vec<UsageSite>) -> Result<()> {
    let content = std::fs::read_to_string(path)?;
    let syntax = syn::parse_file(&content).context("Failed to parse file")?;

    // Also do a simple line-by-line scan for line numbers
    let lines: Vec<&str> = content.lines().collect();

    for item in &syntax.items {
        scan_item(item, path, crate_name, &lines, sites);
    }

    Ok(())
}

fn scan_item(
    item: &Item,
    file: &Path,
    crate_name: &str,
    lines: &[&str],
    sites: &mut Vec<UsageSite>,
) {
    match item {
        Item::Use(u) => {
            // Check if this use imports from the target crate
            extract_use_paths(&u.tree, crate_name, file, lines, sites);
        }

        Item::Struct(s) => {
            check_derives(&s.attrs, crate_name, file, lines, sites);
            // Recurse into any items this might contain
        }

        Item::Enum(e) => {
            check_derives(&e.attrs, crate_name, file, lines, sites);
        }

        Item::Impl(i) => {
            // Check if implementing a trait from the target crate
            if let Some((_, trait_path, _)) = &i.trait_ {
                if let Some(first) = trait_path.segments.first() {
                    if first.ident == crate_name {
                        let full_path = trait_path
                            .segments
                            .iter()
                            .map(|s| s.ident.to_string())
                            .collect::<Vec<_>>()
                            .join("::");

                        // Find line number
                        let line = find_line_containing(lines, &format!("impl {}", first.ident))
                            .unwrap_or(1);

                        sites.push(UsageSite {
                            file: file.to_path_buf(),
                            line,
                            path: full_path,
                            kind: UsageKind::TraitImpl,
                            context: None,
                        });
                    }
                }
            }

            // Scan items in impl block
            for impl_item in &i.items {
                if let syn::ImplItem::Fn(method) = impl_item {
                    check_attrs(&method.attrs, crate_name, file, lines, sites);
                }
            }
        }

        Item::Fn(f) => {
            check_attrs(&f.attrs, crate_name, file, lines, sites);
        }

        Item::Mod(m) => {
            check_attrs(&m.attrs, crate_name, file, lines, sites);
            if let Some((_, items)) = &m.content {
                for item in items {
                    scan_item(item, file, crate_name, lines, sites);
                }
            }
        }

        _ => {}
    }
}

/// Extract paths from a use tree that reference the target crate
fn extract_use_paths(
    tree: &UseTree,
    crate_name: &str,
    file: &Path,
    lines: &[&str],
    sites: &mut Vec<UsageSite>,
) {
    match tree {
        UseTree::Path(p) => {
            if p.ident == crate_name {
                // This use statement imports from our target crate
                let paths = flatten_use_tree(&p.tree, crate_name);
                for path in paths {
                    let line =
                        find_line_containing(lines, &format!("use {}", crate_name)).unwrap_or(1);
                    sites.push(UsageSite {
                        file: file.to_path_buf(),
                        line,
                        path,
                        kind: UsageKind::Import,
                        context: None,
                    });
                }
            } else {
                // Recurse in case of nested paths
                extract_use_paths(&p.tree, crate_name, file, lines, sites);
            }
        }
        UseTree::Group(g) => {
            for tree in &g.items {
                extract_use_paths(tree, crate_name, file, lines, sites);
            }
        }
        _ => {}
    }
}

/// Flatten a use tree into full paths
fn flatten_use_tree(tree: &UseTree, prefix: &str) -> Vec<String> {
    match tree {
        UseTree::Name(n) => {
            vec![format!("{}::{}", prefix, n.ident)]
        }
        UseTree::Rename(r) => {
            vec![format!("{}::{}", prefix, r.ident)]
        }
        UseTree::Glob(_) => {
            vec![format!("{}::*", prefix)]
        }
        UseTree::Path(p) => {
            let new_prefix = format!("{}::{}", prefix, p.ident);
            flatten_use_tree(&p.tree, &new_prefix)
        }
        UseTree::Group(g) => g
            .items
            .iter()
            .flat_map(|t| flatten_use_tree(t, prefix))
            .collect(),
    }
}

/// Check derive macros for references to target crate
fn check_derives(
    attrs: &[Attribute],
    crate_name: &str,
    file: &Path,
    lines: &[&str],
    sites: &mut Vec<UsageSite>,
) {
    for attr in attrs {
        if attr.path().is_ident("derive") {
            // Parse the derive contents
            if let Ok(meta) = attr.meta.require_list() {
                let tokens = meta.tokens.to_string();
                // Common derives from popular crates
                let known_derives: Vec<(&str, &str)> = vec![
                    ("serde", "Serialize"),
                    ("serde", "Deserialize"),
                    ("thiserror", "Error"),
                    ("clap", "Parser"),
                    ("clap", "Args"),
                    ("clap", "Subcommand"),
                    ("clap", "ValueEnum"),
                ];

                for (crate_match, derive_name) in known_derives {
                    if crate_match == crate_name && tokens.contains(derive_name) {
                        let line = find_line_containing(lines, &format!("derive("))
                            .or_else(|| find_line_containing(lines, derive_name))
                            .unwrap_or(1);
                        sites.push(UsageSite {
                            file: file.to_path_buf(),
                            line,
                            path: derive_name.to_string(),
                            kind: UsageKind::Derive,
                            context: None,
                        });
                    }
                }
            }
        }
    }
}

/// Check attributes for references to target crate
fn check_attrs(
    attrs: &[Attribute],
    crate_name: &str,
    file: &Path,
    lines: &[&str],
    sites: &mut Vec<UsageSite>,
) {
    for attr in attrs {
        let path_str = attr
            .path()
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect::<Vec<_>>()
            .join("::");

        if path_str.starts_with(crate_name) || path_str == crate_name {
            let line = find_line_containing(lines, &format!("#[{}", crate_name)).unwrap_or(1);
            sites.push(UsageSite {
                file: file.to_path_buf(),
                line,
                path: path_str,
                kind: UsageKind::AttributeMacro,
                context: None,
            });
        }
    }
}

/// Find the line number (1-indexed) containing a substring
fn find_line_containing(lines: &[&str], needle: &str) -> Option<usize> {
    lines
        .iter()
        .position(|line| line.contains(needle))
        .map(|i| i + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_analyze_self() {
        // Test on this project itself
        let root = std::env::current_dir().unwrap();
        let result = analyze_usage(&root, "rusqlite");
        if let Ok(summary) = result {
            println!("{}", summary.format());
        }
    }
}
