mod cache;
mod cargo;
mod languages;
mod mcp;
mod npm;
mod schema;

use crate::cache::{Cache, parallel_index};
use crate::cargo::{RegistryCrate, find_crate, list_registry_crates, resolve_project_deps};
use crate::languages::rust::RustParser;
use crate::languages::typescript::{TsLanguage, TypeScriptParser};
use crate::npm::parse_package_json;
use crate::schema::{Item, PackageItems};
use camino::Utf8PathBuf;
use clap::{Parser, Subcommand};
use std::fs;

#[derive(Parser)]
#[command(name = "fastdeps")]
#[command(about = "Quickly peek at dependency source code", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// List dependencies (project deps by default, --all for full registry)
    List {
        /// Filter by crate name (substring match)
        #[arg(short, long)]
        filter: Option<String>,

        /// Show only the latest version of each crate
        #[arg(short = 'L', long)]
        latest: bool,

        /// List ALL crates in cargo registry (not just project deps)
        #[arg(short, long)]
        all: bool,
    },

    /// List dependencies of the current project
    Deps {
        /// Path to project directory (defaults to current dir)
        #[arg(short, long)]
        path: Option<Utf8PathBuf>,
    },

    /// Peek at a crate's API surface
    Peek {
        /// Crate name (e.g., "serde" or "serde@1.0.200")
        name: String,

        /// Show full details including methods and fields
        #[arg(short, long)]
        full: bool,

        /// Skip cache, parse fresh
        #[arg(long)]
        no_cache: bool,

        /// Project path to search for path dependencies
        #[arg(short, long)]
        project: Option<Utf8PathBuf>,
    },

    /// Search for a symbol across dependencies
    Find {
        /// Symbol to search for (e.g., "Serialize", "spawn")
        query: String,

        /// Search ALL registry crates (not just project deps)
        #[arg(short, long)]
        all: bool,

        /// Skip cache, parse fresh (slow)
        #[arg(long)]
        no_cache: bool,
    },

    /// Show the source path for a crate
    Where {
        /// Crate name (e.g., "serde" or "serde@1.0.200")
        name: String,
    },

    /// Parse a single Rust source file (for debugging)
    Parse {
        /// Path to the .rs file
        file: Utf8PathBuf,

        /// Module path prefix
        #[arg(short, long, default_value = "crate")]
        module: String,
    },

    /// Parse a single TypeScript/JavaScript file
    ParseTs {
        /// Path to the .ts/.tsx/.js file
        file: Utf8PathBuf,

        /// Module path prefix
        #[arg(short, long, default_value = "module")]
        module: String,
    },

    /// Peek at a TypeScript/JavaScript project's API surface
    PeekTs {
        /// Path to project directory (with package.json)
        #[arg(short, long, default_value = ".")]
        path: Utf8PathBuf,

        /// Show full details including methods and fields
        #[arg(short, long)]
        full: bool,
    },

    /// Manage the local cache
    Cache {
        #[command(subcommand)]
        action: CacheAction,
    },

    /// Start MCP server for AI assistant integration (stdio transport)
    Mcp,
}

#[derive(Subcommand)]
enum CacheAction {
    /// Build/update cache for project dependencies
    Build {
        /// Re-index even if already cached
        #[arg(short, long)]
        force: bool,
    },
    /// Show cache statistics
    Stats,
    /// Clear all cached data
    Clear,
    /// List all indexed crates
    List,
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::List {
            filter,
            latest,
            all,
        } => cmd_list(filter, latest, all),
        Commands::Deps { path } => cmd_deps(path),
        Commands::Peek {
            name,
            full,
            no_cache,
            project,
        } => cmd_peek(&name, full, no_cache, project),
        Commands::Find {
            query,
            all,
            no_cache,
        } => cmd_find(&query, all, no_cache),
        Commands::Where { name } => cmd_where(&name),
        Commands::Parse { file, module } => cmd_parse(&file, &module),
        Commands::ParseTs { file, module } => cmd_parse_ts(&file, &module),
        Commands::PeekTs { path, full } => cmd_peek_ts(&path, full),
        Commands::Cache { action } => match action {
            CacheAction::Build { force } => cmd_cache_build(force),
            CacheAction::Stats => cmd_cache_stats(),
            CacheAction::Clear => cmd_cache_clear(),
            CacheAction::List => cmd_cache_list(),
        },
        Commands::Mcp => {
            std::process::exit(mcp::cmd_mcp());
        }
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

fn cmd_list(
    filter: Option<String>,
    latest: bool,
    all: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut crates = if all {
        // List all crates in the cargo registry
        list_registry_crates()?
    } else {
        // Default: list only project dependencies
        // Try Rust first (Cargo.lock), then TypeScript (package.json)
        let project_dir = Utf8PathBuf::from(".");
        match resolve_project_deps(&project_dir) {
            Ok(deps) => deps,
            Err(_) => {
                // Try npm/TypeScript
                match npm::get_project_deps(&project_dir) {
                    Ok(npm_deps) => {
                        // Convert to display format and print directly
                        let mut deps: Vec<_> = npm_deps
                            .iter()
                            .map(|d| (d.name.clone(), d.version.clone()))
                            .collect();

                        if let Some(ref f) = filter {
                            deps.retain(|(name, _)| name.contains(f));
                        }

                        deps.sort();
                        for (name, version) in &deps {
                            println!("{}@{}", name, version);
                        }
                        eprintln!("\n{} dependencies found", deps.len());
                        return Ok(());
                    }
                    Err(_) => {
                        eprintln!(
                            "No Cargo.lock or package.json found. Use --all to list all registry crates."
                        );
                        return Ok(());
                    }
                }
            }
        }
    };

    if let Some(ref f) = filter {
        crates.retain(|c| c.name.contains(f));
    }

    if latest {
        // Keep only the latest version of each crate
        let mut latest_map: std::collections::BTreeMap<String, RegistryCrate> =
            std::collections::BTreeMap::new();
        for krate in crates {
            latest_map
                .entry(krate.name.clone())
                .and_modify(|existing| {
                    if version_cmp(&krate.version, &existing.version) == std::cmp::Ordering::Greater
                    {
                        *existing = krate.clone();
                    }
                })
                .or_insert(krate);
        }
        crates = latest_map.into_values().collect();
    }

    for krate in &crates {
        println!("{}@{}", krate.name, krate.version);
    }

    eprintln!("\n{} crates found", crates.len());
    Ok(())
}

fn cmd_deps(path: Option<Utf8PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let project_dir = path.unwrap_or_else(|| Utf8PathBuf::from("."));
    let deps = resolve_project_deps(&project_dir)?;

    for dep in &deps {
        println!("{}@{}", dep.name, dep.version);
    }

    eprintln!("\n{} dependencies", deps.len());
    Ok(())
}

fn cmd_peek(
    name: &str,
    full: bool,
    no_cache: bool,
    project: Option<Utf8PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (crate_name, version) = parse_crate_spec(name);

    // Try cache first
    if !no_cache && Cache::exists() {
        if let Ok(cache) = Cache::open_existing() {
            let items = cache.search_crate(crate_name, version)?;
            if !items.is_empty() {
                eprintln!("(from cache)");
                for item in &items {
                    if let Some(sig) = &item.signature {
                        println!("{} ({}) - {}", item.path, item.kind, sig);
                    } else {
                        println!("{} ({})", item.path, item.kind);
                    }
                }
                eprintln!("\n{} items found", items.len());
                return Ok(());
            }
        }
    }

    // Fall back to parsing - try project path deps first, then registry
    let krate = find_specific_crate(crate_name, version, project.as_ref())?;
    eprintln!("Parsing {}@{} ...", krate.name, krate.version);

    let mut parser = RustParser::new()?;
    let mut all_items: Vec<Item> = Vec::new();

    for source_file in krate.source_files() {
        let relative = source_file
            .strip_prefix(&krate.path)
            .unwrap_or(&source_file);
        let module_path = path_to_module(&krate.name, relative);

        if let Ok(source) = fs::read_to_string(&source_file) {
            if let Ok(items) = parser.parse_source(&source, &module_path) {
                all_items.extend(items);
            }
        }
    }

    // Sort items by path for consistent output
    all_items.sort_by(|a, b| a.path.cmp(&b.path));

    if full {
        let package = PackageItems { items: all_items };
        println!("{}", serde_json::to_string_pretty(&package)?);
    } else {
        // Compact output: just paths and kinds
        for item in &all_items {
            let kind = format!("{:?}", item.kind).to_lowercase();
            if let Some(sig) = &item.signature {
                println!("{} ({}) - {}", item.path, kind, sig);
            } else {
                println!("{} ({})", item.path, kind);
            }
        }
        eprintln!("\n{} items found", all_items.len());
    }

    Ok(())
}

fn cmd_find(
    query: &str,
    search_all: bool,
    no_cache: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Try cache first
    if !no_cache && Cache::exists() {
        if let Ok(cache) = Cache::open_existing() {
            let results = cache.search(query)?;
            if !results.is_empty() {
                // Default: filter to project deps (unless --all)
                let results = if !search_all {
                    let deps = resolve_project_deps(&Utf8PathBuf::from("."))?;
                    let dep_set: std::collections::HashSet<_> = deps
                        .iter()
                        .map(|d| (d.name.as_str(), d.version.as_str()))
                        .collect();
                    results
                        .into_iter()
                        .filter(|r| {
                            dep_set.contains(&(r.crate_name.as_str(), r.crate_version.as_str()))
                        })
                        .collect()
                } else {
                    results
                };

                eprintln!("(from cache)");
                for r in &results {
                    println!(
                        "{}@{}: {} ({})",
                        r.crate_name, r.crate_version, r.path, r.kind
                    );
                }
                eprintln!("\n{} matches found", results.len());
                return Ok(());
            }
        }
    }

    // Fall back to parallel parsing using rayon
    use rayon::prelude::*;

    // Default: project deps only (unless --all)
    let crates = if search_all {
        list_registry_crates()?
    } else {
        resolve_project_deps(&Utf8PathBuf::from("."))?
    };

    let query_lower = query.to_lowercase();

    eprintln!("Searching {} crates (no cache)...", crates.len());

    // Parse crates in parallel and collect matches
    let matches: Vec<(String, String, String, String)> = crates
        .par_iter()
        .flat_map(|krate| {
            let mut results = Vec::new();
            if let Ok(mut parser) = RustParser::new() {
                for source_file in krate.source_files() {
                    let relative = source_file
                        .strip_prefix(&krate.path)
                        .unwrap_or(&source_file);
                    let module_path = path_to_module(&krate.name, relative);

                    if let Ok(source) = fs::read_to_string(&source_file) {
                        if let Ok(items) = parser.parse_source(&source, &module_path) {
                            for item in items {
                                if item.path.to_lowercase().contains(&query_lower) {
                                    let kind = format!("{:?}", item.kind).to_lowercase();
                                    results.push((
                                        krate.name.clone(),
                                        krate.version.clone(),
                                        item.path,
                                        kind,
                                    ));
                                }
                            }
                        }
                    }
                }
            }
            results
        })
        .collect();

    for (name, version, path, kind) in &matches {
        println!("{}@{}: {} ({})", name, version, path, kind);
    }

    eprintln!("\n{} matches found", matches.len());
    Ok(())
}

fn cmd_where(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let (crate_name, version) = parse_crate_spec(name);
    let krate = find_specific_crate(crate_name, version, None)?;

    println!("{}", krate.path);

    if let Some(lib) = krate.lib_path() {
        println!("Entry point: {}", lib);
    }

    Ok(())
}

fn cmd_parse(file: &Utf8PathBuf, module: &str) -> Result<(), Box<dyn std::error::Error>> {
    let source = fs::read_to_string(file)?;
    let mut parser = RustParser::new()?;
    let items = parser.parse_source(&source, module)?;
    let package = PackageItems { items };
    println!("{}", serde_json::to_string_pretty(&package)?);
    Ok(())
}

fn cmd_parse_ts(file: &Utf8PathBuf, module: &str) -> Result<(), Box<dyn std::error::Error>> {
    let source = fs::read_to_string(file)?;

    // Determine language from extension
    let language = match file.extension() {
        Some("tsx") => TsLanguage::Tsx,
        Some("jsx") => TsLanguage::Tsx,
        Some("js") | Some("mjs") | Some("cjs") => TsLanguage::JavaScript,
        _ => TsLanguage::TypeScript,
    };

    let mut parser = TypeScriptParser::new(language)?;
    let items = parser.parse_source(&source, module)?;
    let package = PackageItems { items };
    println!("{}", serde_json::to_string_pretty(&package)?);
    Ok(())
}

fn cmd_peek_ts(path: &Utf8PathBuf, full: bool) -> Result<(), Box<dyn std::error::Error>> {
    let pkg = parse_package_json(path)?;
    eprintln!("Parsing {}@{} ...", pkg.name, pkg.version);

    let mut all_items: Vec<Item> = Vec::new();

    for source_file in pkg.source_files() {
        let relative = source_file.strip_prefix(&pkg.path).unwrap_or(&source_file);
        let module_path = npm::path_to_module(&pkg.name, relative);

        // Determine language from extension
        let language = match source_file.extension() {
            Some("tsx") | Some("jsx") => TsLanguage::Tsx,
            Some("js") | Some("mjs") | Some("cjs") => TsLanguage::JavaScript,
            _ => TsLanguage::TypeScript,
        };

        if let Ok(source) = fs::read_to_string(&source_file) {
            if let Ok(mut parser) = TypeScriptParser::new(language) {
                if let Ok(items) = parser.parse_source(&source, &module_path) {
                    all_items.extend(items);
                }
            }
        }
    }

    // Sort items by path for consistent output
    all_items.sort_by(|a, b| a.path.cmp(&b.path));

    if full {
        let package = PackageItems { items: all_items };
        println!("{}", serde_json::to_string_pretty(&package)?);
    } else {
        // Compact output: just paths and kinds
        for item in &all_items {
            let kind = format!("{:?}", item.kind).to_lowercase();
            if let Some(sig) = &item.signature {
                println!("{} ({}) - {}", item.path, kind, sig);
            } else {
                println!("{} ({})", item.path, kind);
            }
        }
        eprintln!("\n{} items found", all_items.len());
    }

    Ok(())
}

// === Cache commands ===

fn cmd_cache_build(force: bool) -> Result<(), Box<dyn std::error::Error>> {
    let deps = resolve_project_deps(&Utf8PathBuf::from("."))?;
    eprintln!("Found {} dependencies", deps.len());

    let stats = parallel_index(&deps, force).map_err(|e| -> Box<dyn std::error::Error> { e })?;

    eprintln!(
        "\nDone! Indexed {} crates ({} items), skipped {}, failed {}",
        stats.indexed, stats.total_items, stats.skipped, stats.failed
    );
    Ok(())
}

fn cmd_cache_stats() -> Result<(), Box<dyn std::error::Error>> {
    let cache = Cache::open_existing()?;
    let stats = cache.stats()?;

    println!("Crates indexed: {}", stats.crate_count);
    println!("Items indexed:  {}", stats.item_count);
    println!(
        "Database size:  {:.2} MB",
        stats.db_size_bytes as f64 / 1_000_000.0
    );

    Ok(())
}

fn cmd_cache_clear() -> Result<(), Box<dyn std::error::Error>> {
    let cache = Cache::open()?;
    cache.clear()?;
    eprintln!("Cache cleared");
    Ok(())
}

fn cmd_cache_list() -> Result<(), Box<dyn std::error::Error>> {
    let cache = Cache::open_existing()?;
    let crates = cache.list_indexed()?;

    for (name, version) in &crates {
        println!("{}@{}", name, version);
    }

    eprintln!("\n{} crates indexed", crates.len());
    Ok(())
}

// === Helpers ===

/// Parse "crate@version" or just "crate".
fn parse_crate_spec(spec: &str) -> (&str, Option<&str>) {
    if let Some((name, version)) = spec.split_once('@') {
        (name, Some(version))
    } else {
        (spec, None)
    }
}

/// Find a crate, preferring specific version or latest.
/// If project_dir is provided, also checks path dependencies.
fn find_specific_crate(
    name: &str,
    version: Option<&str>,
    project_dir: Option<&Utf8PathBuf>,
) -> Result<RegistryCrate, Box<dyn std::error::Error>> {
    // First, check project path dependencies if a project dir is provided
    if let Some(proj_dir) = project_dir {
        let deps = resolve_project_deps(proj_dir)?;
        for dep in deps {
            if dep.name == name {
                if let Some(v) = version {
                    if dep.version == v {
                        return Ok(dep);
                    }
                } else {
                    return Ok(dep);
                }
            }
        }
    }

    // Fall back to registry
    let crates = find_crate(name)?;

    if crates.is_empty() {
        return Err(format!("Crate '{}' not found in registry", name).into());
    }

    if let Some(v) = version {
        crates
            .into_iter()
            .find(|c| c.version == v)
            .ok_or_else(|| format!("Version {} of '{}' not found", v, name).into())
    } else {
        // Return the latest version
        crates
            .into_iter()
            .max_by(|a, b| version_cmp(&a.version, &b.version))
            .ok_or_else(|| format!("No versions found for '{}'", name).into())
    }
}

/// Convert a file path to a module path.
/// e.g., "src/ser/mod.rs" -> "serde::ser"
pub fn path_to_module(crate_name: &str, path: &camino::Utf8Path) -> String {
    let path_str = path.as_str();

    // Strip src/ prefix
    let path_str = path_str.strip_prefix("src/").unwrap_or(path_str);

    // Strip .rs extension
    let path_str = path_str.strip_suffix(".rs").unwrap_or(path_str);

    // Handle lib.rs and main.rs -> crate root
    if path_str == "lib" || path_str == "main" {
        return crate_name.to_string();
    }

    // Handle mod.rs -> parent module
    let path_str = path_str.strip_suffix("/mod").unwrap_or(path_str);

    // Convert path separators to ::
    let module_part = path_str.replace('/', "::");

    format!("{}::{}", crate_name, module_part)
}

/// Simple semver comparison (handles most common cases).
fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let parse = |v: &str| -> Vec<u64> {
        v.split(|c: char| !c.is_ascii_digit())
            .filter_map(|s| s.parse().ok())
            .collect()
    };

    let a_parts = parse(a);
    let b_parts = parse(b);
    a_parts.cmp(&b_parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_to_module() {
        assert_eq!(
            path_to_module("serde", &Utf8PathBuf::from("src/lib.rs")),
            "serde"
        );
        assert_eq!(
            path_to_module("serde", &Utf8PathBuf::from("src/ser/mod.rs")),
            "serde::ser"
        );
        assert_eq!(
            path_to_module("serde", &Utf8PathBuf::from("src/de/value.rs")),
            "serde::de::value"
        );
    }

    #[test]
    fn test_parse_crate_spec() {
        assert_eq!(parse_crate_spec("serde"), ("serde", None));
        assert_eq!(
            parse_crate_spec("serde@1.0.200"),
            ("serde", Some("1.0.200"))
        );
    }

    #[test]
    fn test_version_cmp() {
        assert_eq!(version_cmp("1.0.0", "1.0.1"), std::cmp::Ordering::Less);
        assert_eq!(version_cmp("1.0.10", "1.0.9"), std::cmp::Ordering::Greater);
        assert_eq!(version_cmp("2.0.0", "1.9.9"), std::cmp::Ordering::Greater);
    }
}
