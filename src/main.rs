mod cargo;
mod languages;
mod schema;

use crate::cargo::{RegistryCrate, find_crate, list_registry_crates, resolve_project_deps};
use crate::languages::rust::RustParser;
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
    /// List all crates in your cargo registry
    List {
        /// Filter by crate name (substring match)
        #[arg(short, long)]
        filter: Option<String>,

        /// Show only the latest version of each crate
        #[arg(short = 'L', long)]
        latest: bool,
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
    },

    /// Search for a symbol across dependencies
    Find {
        /// Symbol to search for (e.g., "Serialize", "spawn")
        query: String,

        /// Only search in project dependencies (requires Cargo.lock)
        #[arg(short, long)]
        project: bool,
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
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::List { filter, latest } => cmd_list(filter, latest),
        Commands::Deps { path } => cmd_deps(path),
        Commands::Peek { name, full } => cmd_peek(&name, full),
        Commands::Find { query, project } => cmd_find(&query, project),
        Commands::Where { name } => cmd_where(&name),
        Commands::Parse { file, module } => cmd_parse(&file, &module),
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

fn cmd_list(filter: Option<String>, latest: bool) -> Result<(), Box<dyn std::error::Error>> {
    let mut crates = list_registry_crates()?;

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

fn cmd_peek(name: &str, full: bool) -> Result<(), Box<dyn std::error::Error>> {
    let (crate_name, version) = parse_crate_spec(name);
    let krate = find_specific_crate(crate_name, version)?;

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

fn cmd_find(query: &str, project_only: bool) -> Result<(), Box<dyn std::error::Error>> {
    let crates = if project_only {
        resolve_project_deps(&Utf8PathBuf::from("."))?
    } else {
        list_registry_crates()?
    };

    let query_lower = query.to_lowercase();
    let mut parser = RustParser::new()?;
    let mut found = 0;

    for krate in crates {
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
                            println!("{}@{}: {} ({})", krate.name, krate.version, item.path, kind);
                            found += 1;
                        }
                    }
                }
            }
        }
    }

    eprintln!("\n{} matches found", found);
    Ok(())
}

fn cmd_where(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let (crate_name, version) = parse_crate_spec(name);
    let krate = find_specific_crate(crate_name, version)?;

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
fn find_specific_crate(
    name: &str,
    version: Option<&str>,
) -> Result<RegistryCrate, Box<dyn std::error::Error>> {
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
fn path_to_module(crate_name: &str, path: &camino::Utf8Path) -> String {
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
