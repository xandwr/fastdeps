//! Octo-Sleeper: Batch processor for building the Octo-Index.
//!
//! This worker processes the top 10,000 crates from the crates.io db-dump,
//! downloads their source, runs static analysis, and builds the compressed index.
//!
//! Usage:
//!   cargo run --bin octo-sleeper -- --db-dump ./db-dump/2026-01-11-020011 --output octo-index.bin

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Semaphore, mpsc};

// Import from the main crate
use cratefind::octo_index::{OctoIndex, OctonionProfile, RawMetrics};

/// Parsed crate metadata from db-dump.
#[derive(Debug, Clone)]
struct CrateMeta {
    #[allow(dead_code)]
    id: u64,
    name: String,
    created_at: String,
    downloads: u64,
    versions: Vec<VersionMeta>,
}

#[derive(Debug, Clone)]
struct VersionMeta {
    num: String,
    #[allow(dead_code)]
    created_at: String,
    yanked: bool,
}

/// Result of analyzing a crate's source.
#[derive(Debug)]
struct AnalysisResult {
    name: String,
    version: String,
    raw: RawMetrics,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    let mut db_dump_path: Option<PathBuf> = None;
    let mut output_path = PathBuf::from("octo-index.bin");
    let mut limit: usize = 10_000;
    let mut concurrency: usize = 8;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--db-dump" => {
                db_dump_path = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            "--output" | "-o" => {
                output_path = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            "--limit" | "-n" => {
                limit = args[i + 1].parse()?;
                i += 2;
            }
            "--concurrency" | "-j" => {
                concurrency = args[i + 1].parse()?;
                i += 2;
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            _ => {
                eprintln!("Unknown argument: {}", args[i]);
                print_help();
                std::process::exit(1);
            }
        }
    }

    let db_dump = db_dump_path.context("--db-dump path required")?;

    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║                     OCTO-SLEEPER                             ║");
    println!("║         Building Octonion Index for Top Crates               ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!();
    println!("  DB Dump:     {}", db_dump.display());
    println!("  Output:      {}", output_path.display());
    println!("  Limit:       {} crates", limit);
    println!("  Concurrency: {} workers", concurrency);
    println!();

    // Step 1: Load crate metadata from CSV files
    println!("[1/4] Loading crate metadata from db-dump...");
    let crates = load_crate_metadata(&db_dump, limit)?;
    println!("       Loaded {} crates with download data", crates.len());

    // Step 2: Process crates with concurrent workers
    println!("[2/4] Analyzing crate sources...");
    let results = process_crates(crates, concurrency).await?;
    println!("       Successfully analyzed {} crates", results.len());

    // Step 3: Build the index
    println!("[3/4] Building Octo-Index...");
    let mut index = OctoIndex::new();
    for result in results {
        let coeffs = result.raw.to_coeffs();
        index.insert(OctonionProfile {
            name: result.name,
            version: result.version,
            coeffs,
            raw: result.raw,
        });
    }
    println!("       Index contains {} profiles", index.count);

    // Step 4: Serialize and save
    println!("[4/4] Compressing and saving...");
    index.save(&output_path)?;
    let size = std::fs::metadata(&output_path)?.len();
    println!(
        "       Saved to {} ({:.2} KB)",
        output_path.display(),
        size as f64 / 1024.0
    );

    println!();
    println!("✓ Done! Bundle with: include_bytes!(\"octo-index.bin\")");

    Ok(())
}

fn print_help() {
    eprintln!("octo-sleeper - Build the Octo-Index from crates.io db-dump");
    eprintln!();
    eprintln!("USAGE:");
    eprintln!("  octo-sleeper --db-dump <path> [OPTIONS]");
    eprintln!();
    eprintln!("OPTIONS:");
    eprintln!("  --db-dump <path>    Path to extracted crates.io db-dump directory");
    eprintln!("  --output, -o <path> Output file path (default: octo-index.bin)");
    eprintln!("  --limit, -n <num>   Number of top crates to process (default: 10000)");
    eprintln!("  --concurrency, -j   Number of concurrent workers (default: 8)");
    eprintln!("  --help, -h          Show this help");
}

/// Load crate metadata from the db-dump CSV files.
fn load_crate_metadata(db_dump: &Path, limit: usize) -> Result<Vec<CrateMeta>> {
    let data_dir = db_dump.join("data");

    // Load crate downloads
    println!("       Loading crate_downloads.csv...");
    let downloads_path = data_dir.join("crate_downloads.csv");
    let mut downloads_map: HashMap<u64, u64> = HashMap::new();

    let mut rdr = csv::Reader::from_path(&downloads_path)?;
    for result in rdr.records() {
        let record = result?;
        let crate_id: u64 = record.get(0).unwrap_or("0").parse().unwrap_or(0);
        let downloads: u64 = record.get(1).unwrap_or("0").parse().unwrap_or(0);
        downloads_map.insert(crate_id, downloads);
    }
    println!("       {} crates with download counts", downloads_map.len());

    // Load versions to get version counts and dates
    println!("       Loading versions.csv...");
    let versions_path = data_dir.join("versions.csv");
    let mut versions_map: HashMap<u64, Vec<VersionMeta>> = HashMap::new();

    let mut rdr = csv::Reader::from_path(&versions_path)?;
    for result in rdr.records() {
        let record = result?;
        // versions.csv columns: bin_names,categories,checksum,crate_id,crate_size,created_at,...
        let crate_id: u64 = record.get(3).unwrap_or("0").parse().unwrap_or(0);
        let created_at = record.get(5).unwrap_or("").to_string();
        let num = record.get(17).unwrap_or("").to_string(); // "num" column
        let yanked = record.get(23).unwrap_or("f") == "t";

        versions_map.entry(crate_id).or_default().push(VersionMeta {
            num,
            created_at,
            yanked,
        });
    }
    println!("       {} crates with version data", versions_map.len());

    // Load crates
    println!("       Loading crates.csv...");
    let crates_path = data_dir.join("crates.csv");
    let mut crates: Vec<CrateMeta> = Vec::new();

    let mut rdr = csv::Reader::from_path(&crates_path)?;
    for result in rdr.records() {
        let record = result?;
        // crates.csv columns: created_at,description,documentation,homepage,id,max_features,max_upload_size,name,...
        let id: u64 = record.get(4).unwrap_or("0").parse().unwrap_or(0);
        let name = record.get(7).unwrap_or("").to_string();
        let created_at = record.get(0).unwrap_or("").to_string();

        if name.is_empty() {
            continue;
        }

        let downloads = downloads_map.get(&id).copied().unwrap_or(0);
        let versions = versions_map.remove(&id).unwrap_or_default();

        crates.push(CrateMeta {
            id,
            name,
            created_at,
            downloads,
            versions,
        });
    }

    // Sort by downloads descending
    crates.sort_by(|a, b| b.downloads.cmp(&a.downloads));

    // Take top N
    crates.truncate(limit);

    println!(
        "       Top crate: {} ({} downloads)",
        crates.first().map(|c| c.name.as_str()).unwrap_or("none"),
        crates.first().map(|c| c.downloads).unwrap_or(0)
    );

    Ok(crates)
}

/// Process crates concurrently using tokio workers.
async fn process_crates(crates: Vec<CrateMeta>, concurrency: usize) -> Result<Vec<AnalysisResult>> {
    let (tx, mut rx) = mpsc::channel::<AnalysisResult>(100);
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let total = crates.len();

    let mut handles = Vec::new();

    for (idx, crate_meta) in crates.into_iter().enumerate() {
        let tx = tx.clone();
        let sem = semaphore.clone();

        let handle = tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();

            // Find the latest non-yanked version by sorting
            let mut versions: Vec<_> = crate_meta.versions.iter().filter(|v| !v.yanked).collect();
            versions.sort_by_key(|v| v.version_tuple());
            let version = versions.last().map(|v| v.version_str()).unwrap_or_else(|| {
                // Fall back to any version if all are yanked
                let mut all: Vec<_> = crate_meta.versions.iter().collect();
                all.sort_by_key(|v| v.version_tuple());
                all.last().map(|v| v.version_str()).unwrap_or_default()
            });

            if version.is_empty() {
                return;
            }

            // Calculate age in days
            let age_days = calculate_age_days(&crate_meta.created_at);

            // Try to analyze source from local cargo registry
            match analyze_crate_source(&crate_meta.name, &version).await {
                Ok(mut raw) => {
                    raw.downloads = crate_meta.downloads;
                    raw.age_days = age_days;
                    raw.version_count = crate_meta.versions.len() as u32;

                    if (idx + 1) % 100 == 0 || idx + 1 == total {
                        eprintln!("       [{}/{}] {}", idx + 1, total, crate_meta.name);
                    }

                    let _ = tx
                        .send(AnalysisResult {
                            name: crate_meta.name,
                            version,
                            raw,
                        })
                        .await;
                }
                Err(_) => {
                    // Silently skip crates we can't analyze (not downloaded locally)
                }
            }
        });

        handles.push(handle);
    }

    // Drop the original sender so the channel closes when all tasks complete
    drop(tx);

    // Collect results
    let mut results = Vec::new();
    while let Some(result) = rx.recv().await {
        results.push(result);
    }

    // Wait for all tasks to complete
    for handle in handles {
        let _ = handle.await;
    }

    Ok(results)
}

impl VersionMeta {
    fn version_str(&self) -> String {
        self.num.clone()
    }

    /// Parse version for sorting (returns (major, minor, patch, prerelease_penalty)).
    fn version_tuple(&self) -> (i32, i32, i32, i32) {
        let s = &self.num;
        // Remove any leading 'v'
        let s = s.strip_prefix('v').unwrap_or(s);

        // Split on '-' to separate prerelease
        let (version_part, prerelease) = s.split_once('-').unwrap_or((s, ""));
        let prerelease_penalty = if prerelease.is_empty() { 0 } else { -1 };

        let parts: Vec<&str> = version_part.split('.').collect();
        let major: i32 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
        let minor: i32 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        let patch: i32 = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);

        (major, minor, patch, prerelease_penalty)
    }
}

/// Calculate age in days from a timestamp string.
fn calculate_age_days(created_at: &str) -> u32 {
    // Parse ISO timestamp like "2023-05-01 12:06:24.629411+00"
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    // Simple parsing: extract year-month-day
    if created_at.len() < 10 {
        return 0;
    }

    let parts: Vec<&str> = created_at[..10].split('-').collect();
    if parts.len() != 3 {
        return 0;
    }

    let year: i32 = parts[0].parse().unwrap_or(2020);
    let month: i32 = parts[1].parse().unwrap_or(1);
    let day: i32 = parts[2].parse().unwrap_or(1);

    // Approximate days since epoch (good enough for our purposes)
    let days_since_epoch = (year - 1970) * 365 + (month - 1) * 30 + day;
    let now_days = (now / 86400) as i32;

    (now_days - days_since_epoch).max(0) as u32
}

/// Analyze a crate's source code from the local cargo registry.
async fn analyze_crate_source(name: &str, version: &str) -> Result<RawMetrics> {
    // Find source in ~/.cargo/registry/src/
    let source_dir = find_crate_source(name, version)?;

    // Run analysis in blocking task (syn is not async)
    let source_dir_clone = source_dir.clone();
    let result = tokio::task::spawn_blocking(move || analyze_directory(&source_dir_clone)).await?;

    result
}

/// Find crate source in cargo registry.
fn find_crate_source(name: &str, version: &str) -> Result<PathBuf> {
    let home = dirs::home_dir().context("No home directory")?;
    let registry_src = home.join(".cargo/registry/src");

    if !registry_src.exists() {
        anyhow::bail!("Cargo registry not found");
    }

    // Look through all registry indices
    for entry in std::fs::read_dir(&registry_src)? {
        let entry = entry?;
        let index_dir = entry.path();

        // Try exact version match
        let crate_dir = index_dir.join(format!("{}-{}", name, version));
        if crate_dir.exists() {
            return Ok(crate_dir);
        }
    }

    anyhow::bail!("Crate source not found: {}-{}", name, version)
}

/// Analyze all Rust files in a directory.
fn analyze_directory(dir: &Path) -> Result<RawMetrics> {
    let mut raw = RawMetrics::default();
    analyze_dir_recursive(dir, &mut raw)?;
    Ok(raw)
}

fn analyze_dir_recursive(dir: &Path, raw: &mut RawMetrics) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_file() && path.extension().is_some_and(|e| e == "rs") {
            analyze_file(&path, raw)?;
        } else if path.is_dir() {
            // Skip common non-source directories
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !matches!(name, "target" | ".git" | "tests" | "benches" | "examples") {
                analyze_dir_recursive(&path, raw)?;
            }
        }
    }

    // Check for no_std in lib.rs
    let lib_rs = dir.join("src/lib.rs");
    if lib_rs.exists() {
        if let Ok(content) = std::fs::read_to_string(&lib_rs) {
            raw.is_no_std = content.contains("#![no_std]");
        }
    }

    // Count dependencies from Cargo.toml
    let cargo_toml = dir.join("Cargo.toml");
    if cargo_toml.exists() {
        if let Ok(content) = std::fs::read_to_string(&cargo_toml) {
            raw.dep_count = count_dependencies(&content);
        }
    }

    Ok(())
}

/// Analyze a single Rust file.
fn analyze_file(path: &Path, raw: &mut RawMetrics) -> Result<()> {
    let content = std::fs::read_to_string(path)?;
    raw.total_loc += content.lines().count() as u32;

    // Parse with syn
    let Ok(syntax) = syn::parse_file(&content) else {
        return Ok(()); // Skip unparseable files
    };

    // Use visitor pattern
    use syn::visit::Visit;

    struct Visitor {
        unsafe_blocks: u32,
        async_fns: u32,
        total_fns: u32,
        send_sync_impls: u32,
        heap_types: u32,
    }

    impl<'ast> Visit<'ast> for Visitor {
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

    let mut visitor = Visitor {
        unsafe_blocks: 0,
        async_fns: 0,
        total_fns: 0,
        send_sync_impls: 0,
        heap_types: 0,
    };
    visitor.visit_file(&syntax);

    raw.unsafe_blocks += visitor.unsafe_blocks;
    raw.async_fns += visitor.async_fns;
    raw.total_fns += visitor.total_fns;
    raw.send_sync_count += visitor.send_sync_impls;
    raw.heap_types += visitor.heap_types;

    Ok(())
}

/// Count dependencies in Cargo.toml.
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
            if trimmed.contains('=') {
                count += 1;
            }
        }
    }

    count
}
