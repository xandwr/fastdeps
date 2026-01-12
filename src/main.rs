use cratefind::{braid, contrastive, db, embed, octo_index, parse, profile, project, usage};

fn main() {
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(|s| s.as_str()) {
        Some("index") => {
            let project = require_project();
            cmd_index(&project);
        }
        Some("search") => {
            let project = require_project();
            let query = args[2..].join(" ");
            if query.is_empty() {
                eprintln!("usage: cratefind search <query>");
                std::process::exit(1);
            }
            cmd_search(&project, &query);
        }
        Some("stats") => {
            let project = require_project();
            cmd_stats(&project);
        }
        Some("learn") => {
            // Learn about a specific crate (index it deeply)
            let crate_name = args.get(2).map(|s| s.as_str());
            if crate_name.is_none() {
                eprintln!("usage: cratefind learn <crate_name>");
                std::process::exit(1);
            }
            let project = require_project();
            cmd_learn(&project, crate_name.unwrap());
        }
        Some("usage") => {
            // Show how a crate is used in this project
            let crate_name = args.get(2).map(|s| s.as_str());
            if crate_name.is_none() {
                eprintln!("usage: cratefind usage <crate_name>");
                std::process::exit(1);
            }
            let project = require_project();
            cmd_usage(&project, crate_name.unwrap());
        }
        Some("explain") => {
            // Explain a symbol with senior-engineer context
            let symbol = args[2..].join(" ");
            if symbol.is_empty() {
                eprintln!("usage: cratefind explain <symbol_path>");
                eprintln!("example: cratefind explain serde::Deserialize");
                std::process::exit(1);
            }
            let project = require_project();
            cmd_explain(&project, &symbol);
        }
        Some("profile") => {
            // Show octonion profile for a crate
            let crate_name = args.get(2).map(|s| s.as_str());
            if crate_name.is_none() {
                eprintln!("usage: cratefind profile <crate_name>");
                std::process::exit(1);
            }
            let project = require_project();
            cmd_profile(&project, crate_name.unwrap());
        }
        Some("octo-search") => {
            // Experimental octonion-based search
            let project = require_project();
            cmd_octo_search(&project, &args[2..]);
        }
        Some("octo-index") => {
            // Search the pre-built Octo-Index
            cmd_octo_index(&args[2..]);
        }
        Some("octo-lookup") => {
            // Lookup a crate in the Octo-Index
            cmd_octo_lookup(&args[2..]);
        }
        Some("train-mapper") => {
            // Train the contrastive mapper (384D → 8D)
            cmd_train_mapper(&args[2..]);
        }
        Some("semantic-search") => {
            // Natural language search using trained mapper
            let query = args[2..].join(" ");
            if query.is_empty() {
                eprintln!("usage: cratefind semantic-search <natural language query>");
                eprintln!(
                    "example: cratefind semantic-search \"async http client with connection pooling\""
                );
                std::process::exit(1);
            }
            cmd_semantic_search(&query, &args[2..]);
        }
        Some("braid-check") => {
            // Braid analysis for dependency tangles
            let project = require_project();
            cmd_braid_check(&project, &args[2..]);
        }
        _ => {
            eprintln!("cratefind - Understand your Rust dependencies like a senior engineer");
            eprintln!();
            eprintln!("USAGE:");
            eprintln!("  cratefind <command> [args]");
            eprintln!();
            eprintln!("COMMANDS:");
            eprintln!("  index            Index all dependencies (extracts public API)");
            eprintln!("  search <query>   Semantic search across indexed deps");
            eprintln!("  learn <crate>    Deep-index a specific crate");
            eprintln!("  usage <crate>    Show how this project uses a crate");
            eprintln!("  explain <path>   Explain a symbol with context");
            eprintln!("  stats            Show index statistics");
            eprintln!();
            eprintln!("OCTO-INDEX COMMANDS:");
            eprintln!("  octo-index [FLAGS] [--file <path>]");
            eprintln!("                   Search top crates using pre-built octonion index");
            eprintln!("  octo-lookup <crate> [--file <path>]");
            eprintln!("                   Look up a crate's octonion profile");
            eprintln!();
            eprintln!("CONTRASTIVE MAPPING (384D → 8D):");
            eprintln!("  train-mapper --octo-index <path> [-o <output>]");
            eprintln!("                   Train a 384×8 linear mapper from crate descriptions");
            eprintln!("  semantic-search <query> --mapper <path> --octo-index <path>");
            eprintln!("                   Natural language search using trained mapper");
            eprintln!();
            eprintln!("BRAID ANALYSIS:");
            eprintln!("  braid-check      Analyze dependency graph for topological tangles");
            eprintln!("                   Detects async runtime conflicts, Send/Sync mismatches");
            eprintln!();
            eprintln!("EXAMPLES:");
            eprintln!("  cratefind index");
            eprintln!("  cratefind search \"serialize to json\"");
            eprintln!("  cratefind learn serde");
            eprintln!("  cratefind usage anyhow");
            eprintln!("  cratefind explain serde::de::DeserializeOwned");
            eprintln!("  cratefind octo-index --async --safe --file octo-index.bin");
            eprintln!("  cratefind octo-lookup tokio --file octo-index.bin");
            std::process::exit(1);
        }
    }
}

fn require_project() -> project::RustProject {
    match project::RustProject::discover() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!("hint: run this from a Rust project directory (with Cargo.toml)");
            std::process::exit(1);
        }
    }
}

fn cmd_index(project: &project::RustProject) {
    println!("Indexing dependencies for: {}", project.name);
    println!("Found {} dependencies in Cargo.lock", project.deps.len());
    println!();

    let db = db::Database::open().expect("Failed to open database");
    let mut embedder = embed::Embedder::new().expect("Failed to load embedding model");

    let mut indexed = 0;
    let mut skipped = 0;
    let mut failed = 0;

    for dep in &project.deps {
        if db.is_indexed(&dep.name, &dep.version).unwrap_or(false) {
            skipped += 1;
            continue;
        }

        print!("  {}@{} ... ", dep.name, dep.version);
        std::io::Write::flush(&mut std::io::stdout()).ok();

        // Parse the crate source to extract symbols
        match parse::parse_crate(&dep.name, &dep.version) {
            Ok(api) => {
                if api.docs.is_empty() {
                    println!("no public symbols");
                    failed += 1;
                    continue;
                }

                let symbols: Vec<db::Symbol> = api.docs.iter().map(|d| d.to_symbol()).collect();

                // Generate embeddings from enriched text (path + signature + docs)
                let texts: Vec<String> = api.docs.iter().map(|d| d.embedding_text()).collect();

                match embedder.embed(&texts) {
                    Ok(embeddings) => {
                        db.index_crate(&dep.name, &dep.version, &symbols, &embeddings)
                            .expect("Failed to index crate");
                        println!("{} symbols", symbols.len());
                        indexed += 1;
                    }
                    Err(e) => {
                        println!("embedding failed: {}", e);
                        failed += 1;
                    }
                }
            }
            Err(e) => {
                println!("parse failed: {}", e);
                failed += 1;
            }
        }
    }

    println!();
    println!(
        "Done: {} indexed, {} cached, {} failed",
        indexed, skipped, failed
    );
}

fn cmd_search(project: &project::RustProject, query: &str) {
    let db = db::Database::open().expect("Failed to open database");
    let mut embedder = embed::Embedder::new().expect("Failed to load embedding model");

    // Get crate IDs for this project's deps
    let crate_ids: Vec<i64> = project
        .deps
        .iter()
        .filter_map(|dep| db.get_crate_id(&dep.name, &dep.version).ok().flatten())
        .collect();

    if crate_ids.is_empty() {
        eprintln!("No indexed dependencies. Run `cratefind index` first.");
        std::process::exit(1);
    }

    // Embed query
    let query_embedding = embedder
        .embed(&[query.to_string()])
        .expect("Embedding failed");
    let query_vec = &query_embedding[0];

    // Search
    let results = db.search(query_vec, &crate_ids, 10).expect("Search failed");

    if results.is_empty() {
        println!("No results for: {query}");
        return;
    }

    println!("Results for: {query}\n");
    for (i, result) in results.iter().enumerate() {
        let crate_info = format!("{}@{}", result.crate_name, result.crate_version);
        println!(
            "{}. {} ({}) [score: {:.2}]",
            i + 1,
            result.path,
            result.kind,
            result.score
        );
        println!("   from: {}", crate_info);
        if let Some(sig) = &result.signature {
            println!("   {}", sig);
        }
        println!();
    }
}

fn cmd_stats(project: &project::RustProject) {
    let db = db::Database::open().expect("Failed to open database");
    let stats = db.stats().expect("Failed to get stats");

    println!("Global index: {}", db::Database::path().display());
    println!("  {} crates indexed", stats.crate_count);
    println!("  {} symbols", stats.symbol_count);
    println!("  {:.1} MB", stats.db_size_bytes as f64 / 1_000_000.0);
    println!();
    println!("Project: {}", project.name);
    println!("  {} dependencies", project.deps.len());

    let indexed: usize = project
        .deps
        .iter()
        .filter(|d| db.is_indexed(&d.name, &d.version).unwrap_or(false))
        .count();
    println!("  {} indexed", indexed);
}

fn cmd_learn(project: &project::RustProject, crate_name: &str) {
    // Find the version of this crate in the project
    let dep = project.deps.iter().find(|d| d.name == crate_name);

    let version = match dep {
        Some(d) => d.version.clone(),
        None => {
            eprintln!("Crate '{}' not found in project dependencies.", crate_name);
            eprintln!("Available crates:");
            for d in project.deps.iter().take(10) {
                eprintln!("  {}", d.name);
            }
            if project.deps.len() > 10 {
                eprintln!("  ... and {} more", project.deps.len() - 10);
            }
            std::process::exit(1);
        }
    };

    println!("Learning {}@{} ...", crate_name, version);
    println!();

    // Parse the crate
    match parse::parse_crate(crate_name, &version) {
        Ok(api) => {
            println!("Public API of {}:", crate_name);
            println!();

            // Group by kind
            let mut by_kind: std::collections::HashMap<&str, Vec<&parse::SymbolDoc>> =
                std::collections::HashMap::new();
            for doc in &api.docs {
                by_kind.entry(&doc.kind).or_default().push(doc);
            }

            // Print summary
            for (kind, items) in &by_kind {
                println!("  {} {}s", items.len(), kind);
            }
            println!();

            // Print key items
            let important_kinds = ["trait", "struct", "enum", "fn"];
            for kind in important_kinds {
                if let Some(items) = by_kind.get(kind) {
                    println!("{}s:", kind.to_uppercase());
                    for item in items.iter().take(10) {
                        println!("  {}", item.path);
                        if let Some(sig) = &item.signature {
                            println!("    {}", sig);
                        }
                        if let Some(doc) = &item.doc {
                            let first_line = doc.lines().next().unwrap_or("");
                            if !first_line.is_empty() {
                                println!("    /// {}", first_line);
                            }
                        }
                    }
                    if items.len() > 10 {
                        println!("  ... and {} more", items.len() - 10);
                    }
                    println!();
                }
            }

            // Index it
            let db = db::Database::open().expect("Failed to open database");
            let mut embedder = embed::Embedder::new().expect("Failed to load embedding model");

            let symbols: Vec<db::Symbol> = api.docs.iter().map(|d| d.to_symbol()).collect();

            let texts: Vec<String> = api.docs.iter().map(|d| d.embedding_text()).collect();

            let embeddings = embedder.embed(&texts).expect("Embedding failed");
            db.index_crate(crate_name, &version, &symbols, &embeddings)
                .expect("Failed to index crate");

            println!("Indexed {} symbols.", symbols.len());
        }
        Err(e) => {
            eprintln!("Failed to parse crate: {}", e);
            std::process::exit(1);
        }
    }
}

fn cmd_usage(project: &project::RustProject, crate_name: &str) {
    // Verify crate is a dependency
    if !project.deps.iter().any(|d| d.name == crate_name) {
        eprintln!("Crate '{}' not found in project dependencies.", crate_name);
        std::process::exit(1);
    }

    println!(
        "Analyzing usage of '{}' in {} ...",
        crate_name, project.name
    );
    println!();

    match usage::analyze_usage(&project.root, crate_name) {
        Ok(summary) => {
            println!("{}", summary.format());
        }
        Err(e) => {
            eprintln!("Failed to analyze usage: {}", e);
            std::process::exit(1);
        }
    }
}

fn cmd_profile(project: &project::RustProject, crate_name: &str) {
    // Find the version of this crate in the project
    let dep = project.deps.iter().find(|d| d.name == crate_name);

    let version = match dep {
        Some(d) => d.version.clone(),
        None => {
            eprintln!("Crate '{}' not found in project dependencies.", crate_name);
            std::process::exit(1);
        }
    };

    // Find source directory
    let source_dir = match parse::find_crate_source(crate_name, &version) {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("Failed to find crate source: {}", e);
            std::process::exit(1);
        }
    };

    println!("Profiling {}@{} ...", crate_name, version);
    println!();

    match profile::CrateProfile::from_source(crate_name, &version, &source_dir) {
        Ok(p) => {
            println!("Raw metrics:");
            println!("  Lines of code: {}", p.raw.total_loc);
            println!(
                "  Functions: {} ({} async)",
                p.raw.total_fns, p.raw.async_fns
            );
            println!("  Unsafe blocks: {}", p.raw.unsafe_blocks);
            println!("  Send/Sync impls: {}", p.raw.send_sync_count);
            println!("  Heap types: {}", p.raw.heap_types);
            println!("  Dependencies: {}", p.raw.dep_count);
            println!("  no_std: {}", p.raw.is_no_std);
            println!();

            let c = profile::octonion_coeffs(&p.octonion);
            println!("Octonion profile:");
            println!("  e0 (utility):     {:.3}", c[0]);
            println!("  e1 (concurrency): {:.3}", c[1]);
            println!("  e2 (safety):      {:.3}", c[2]);
            println!("  e3 (async):       {:.3}", c[3]);
            println!("  e4 (memory):      {:.3}", c[4]);
            println!("  e5 (friction):    {:.3}", c[5]);
            println!("  e6 (environment): {:.3}", c[6]);
            println!("  e7 (entropy):     {:.3}", c[7]);
            println!();

            // Test against some sample queries
            println!("Sample query scores:");

            let q_async = profile::query_octonion(true, true, false, true, true);
            println!(
                "  'async + send/sync + safe + light': {:.3}",
                p.combined_score(&q_async)
            );

            let q_nostd = profile::query_octonion(false, false, true, true, true);
            println!(
                "  'no_std + safe + light':            {:.3}",
                p.combined_score(&q_nostd)
            );

            let q_simple = profile::query_octonion(false, false, false, false, false);
            println!(
                "  'just utility':                     {:.3}",
                p.combined_score(&q_simple)
            );
        }
        Err(e) => {
            eprintln!("Failed to profile crate: {}", e);
            std::process::exit(1);
        }
    }
}

fn cmd_octo_search(project: &project::RustProject, args: &[String]) {
    // Parse flags
    let mut wants_async = false;
    let mut wants_sync = false;
    let mut wants_nostd = false;
    let mut prefers_safe = false;
    let mut prefers_light = false;

    for arg in args {
        match arg.as_str() {
            "--async" => wants_async = true,
            "--sync" => wants_sync = true,
            "--no-std" => wants_nostd = true,
            "--safe" => prefers_safe = true,
            "--light" => prefers_light = true,
            _ => {}
        }
    }

    if args.is_empty() {
        eprintln!("usage: cratefind octo-search [--async] [--sync] [--no-std] [--safe] [--light]");
        eprintln!();
        eprintln!("Flags:");
        eprintln!("  --async    Prefer async-ready crates");
        eprintln!("  --sync     Prefer Send+Sync crates");
        eprintln!("  --no-std   Prefer no_std compatible crates");
        eprintln!("  --safe     Avoid crates with lots of unsafe");
        eprintln!("  --light    Prefer crates with few dependencies");
        std::process::exit(1);
    }

    let query = profile::query_octonion(
        wants_async,
        wants_sync,
        wants_nostd,
        prefers_safe,
        prefers_light,
    );

    println!("Query octonion:");
    println!(
        "  async={}, sync={}, no_std={}, safe={}, light={}",
        wants_async, wants_sync, wants_nostd, prefers_safe, prefers_light
    );
    println!();

    // Profile all dependencies and rank them
    let mut scores: Vec<(String, String, f32, f32, f32)> = Vec::new();

    for dep in &project.deps {
        if let Ok(source_dir) = parse::find_crate_source(&dep.name, &dep.version) {
            if let Ok(p) = profile::CrateProfile::from_source(&dep.name, &dep.version, &source_dir)
            {
                let (sim, friction) = p.score(&query);
                let combined = p.combined_score(&query);
                scores.push((
                    dep.name.clone(),
                    dep.version.clone(),
                    sim,
                    friction,
                    combined,
                ));
            }
        }
    }

    // Sort by combined score
    scores.sort_by(|a, b| b.4.partial_cmp(&a.4).unwrap());

    println!("Top matches:");
    println!("{:<30} {:>8} {:>8} {:>8}", "CRATE", "SIM", "FRIC", "SCORE");
    println!("{}", "-".repeat(58));

    for (name, version, sim, friction, combined) in scores.iter().take(15) {
        println!(
            "{:<30} {:>8.3} {:>8.3} {:>8.3}",
            format!("{}@{}", name, version),
            sim,
            friction,
            combined
        );
    }
}

fn cmd_explain(project: &project::RustProject, symbol_query: &str) {
    let db = db::Database::open().expect("Failed to open database");
    let mut embedder = embed::Embedder::new().expect("Failed to load embedding model");

    // Get crate IDs for this project's deps
    let crate_ids: Vec<i64> = project
        .deps
        .iter()
        .filter_map(|dep| db.get_crate_id(&dep.name, &dep.version).ok().flatten())
        .collect();

    if crate_ids.is_empty() {
        eprintln!("No indexed dependencies. Run `cratefind index` first.");
        std::process::exit(1);
    }

    // Try exact match first (by path substring)
    let exact_results = db
        .search_by_path(symbol_query, &crate_ids, 5)
        .unwrap_or_default();

    if !exact_results.is_empty() {
        println!("Found symbol: {}", exact_results[0].path);
        println!();

        let result = &exact_results[0];
        println!("From: {}@{}", result.crate_name, result.crate_version);
        println!("Kind: {}", result.kind);
        if let Some(sig) = &result.signature {
            println!("Signature: {}", sig);
        }
        println!();

        // Show related symbols (same crate, similar path)
        println!("Related symbols:");
        for r in exact_results.iter().skip(1) {
            println!("  {} ({})", r.path, r.kind);
        }
        println!();

        // Show usage in this project
        let crate_name = symbol_query.split("::").next().unwrap_or(symbol_query);
        if let Ok(usage_summary) = usage::analyze_usage(&project.root, crate_name) {
            if !usage_summary.sites.is_empty() {
                println!("Used in this project:");
                for site in usage_summary.sites.iter().take(5) {
                    println!("  {}:{} - {:?}", site.file.display(), site.line, site.kind);
                }
            }
        }

        return;
    }

    // Fall back to semantic search
    let query_embedding = embedder
        .embed(&[symbol_query.to_string()])
        .expect("Embedding failed");
    let query_vec = &query_embedding[0];

    let results = db.search(query_vec, &crate_ids, 5).expect("Search failed");

    if results.is_empty() {
        println!("No symbols found matching: {}", symbol_query);
        return;
    }

    println!("Best matches for '{}':", symbol_query);
    println!();
    for (i, result) in results.iter().enumerate() {
        println!("{}. {} ({})", i + 1, result.path, result.kind);
        println!("   from: {}@{}", result.crate_name, result.crate_version);
        if let Some(sig) = &result.signature {
            println!("   {}", sig);
        }
        println!();
    }
}

fn cmd_octo_index(args: &[String]) {
    // Parse flags
    let mut wants_async = false;
    let mut wants_sync = false;
    let mut wants_nostd = false;
    let mut prefers_safe = false;
    let mut prefers_light = false;
    let mut index_file: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--async" => wants_async = true,
            "--sync" => wants_sync = true,
            "--no-std" => wants_nostd = true,
            "--safe" => prefers_safe = true,
            "--light" => prefers_light = true,
            "--file" | "-f" => {
                if i + 1 < args.len() {
                    index_file = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }

    // Load the index
    let index = match &index_file {
        Some(path) => match octo_index::OctoIndex::load(std::path::Path::new(path)) {
            Ok(idx) => idx,
            Err(e) => {
                eprintln!("Failed to load Octo-Index from {}: {}", path, e);
                std::process::exit(1);
            }
        },
        None => {
            eprintln!("No index file specified. Use --file <path> to specify the Octo-Index file.");
            eprintln!();
            eprintln!("To generate an index, run:");
            eprintln!(
                "  cargo run --bin octo-sleeper -- --db-dump ./db-dump/<date> -o octo-index.bin"
            );
            std::process::exit(1);
        }
    };

    if !wants_async && !wants_sync && !wants_nostd && !prefers_safe && !prefers_light {
        // No flags specified - show top crates by utility
        println!("Octo-Index: {} crates indexed", index.count);
        println!();
        println!("Top crates by utility score:");
        println!("{:<30} {:>10} {:>10}", "CRATE", "VERSION", "UTILITY");
        println!("{}", "-".repeat(52));

        for profile in index.top_by_utility(20) {
            println!(
                "{:<30} {:>10} {:>10.3}",
                profile.name, profile.version, profile.coeffs[0]
            );
        }
        return;
    }

    // Build query and search
    let query = octo_index::build_query(
        wants_async,
        wants_sync,
        wants_nostd,
        prefers_safe,
        prefers_light,
    );

    println!("Query:");
    println!(
        "  async={}, sync={}, no_std={}, safe={}, light={}",
        wants_async, wants_sync, wants_nostd, prefers_safe, prefers_light
    );
    println!();

    let results = index.search(&query, 20);

    println!("Top matches from {} crates:", index.count);
    println!(
        "{:<30} {:>10} {:>8} {:>8} {:>8} {:>8}",
        "CRATE", "VERSION", "SCORE", "e0", "e3", "e6"
    );
    println!("{}", "-".repeat(80));

    for (profile, score) in results {
        println!(
            "{:<30} {:>10} {:>8.3} {:>8.3} {:>8.3} {:>8.3}",
            profile.name,
            profile.version,
            score,
            profile.coeffs[0], // utility
            profile.coeffs[3], // async
            profile.coeffs[6], // no_std
        );
    }
}

fn cmd_octo_lookup(args: &[String]) {
    // Parse arguments
    let mut crate_name: Option<String> = None;
    let mut index_file: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--file" | "-f" => {
                if i + 1 < args.len() {
                    index_file = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            s if !s.starts_with('-') && crate_name.is_none() => {
                crate_name = Some(s.to_string());
            }
            _ => {}
        }
        i += 1;
    }

    let crate_name = match crate_name {
        Some(n) => n,
        None => {
            eprintln!("usage: cratefind octo-lookup <crate_name> --file <path>");
            std::process::exit(1);
        }
    };

    // Load the index
    let index = match &index_file {
        Some(path) => match octo_index::OctoIndex::load(std::path::Path::new(path)) {
            Ok(idx) => idx,
            Err(e) => {
                eprintln!("Failed to load Octo-Index from {}: {}", path, e);
                std::process::exit(1);
            }
        },
        None => {
            eprintln!("No index file specified. Use: cratefind octo-lookup <crate> --file <path>");
            std::process::exit(1);
        }
    };

    match index.get(&crate_name) {
        Some(profile) => {
            println!("Octonion Profile: {}@{}", profile.name, profile.version);
            println!();
            println!("Dimensions:");
            println!(
                "  e0 (utility):     {:.3}  (downloads/age)",
                profile.coeffs[0]
            );
            println!(
                "  e1 (concurrency): {:.3}  (Send/Sync impls)",
                profile.coeffs[1]
            );
            println!(
                "  e2 (safety):      {:.3}  (unsafe density)",
                profile.coeffs[2]
            );
            println!(
                "  e3 (async):       {:.3}  (async fn ratio)",
                profile.coeffs[3]
            );
            println!(
                "  e4 (memory):      {:.3}  (heap allocations)",
                profile.coeffs[4]
            );
            println!(
                "  e5 (friction):    {:.3}  (dependency count)",
                profile.coeffs[5]
            );
            println!("  e6 (environment): {:.3}  (no_std)", profile.coeffs[6]);
            println!(
                "  e7 (entropy):     {:.3}  (version volatility)",
                profile.coeffs[7]
            );
            println!();
            println!("Raw metrics:");
            println!("  Downloads:    {}", profile.raw.downloads);
            println!("  Age (days):   {}", profile.raw.age_days);
            println!("  Versions:     {}", profile.raw.version_count);
            println!("  LoC:          {}", profile.raw.total_loc);
            println!(
                "  Functions:    {} ({} async)",
                profile.raw.total_fns, profile.raw.async_fns
            );
            println!("  Unsafe:       {}", profile.raw.unsafe_blocks);
            println!("  Send/Sync:    {}", profile.raw.send_sync_count);
            println!("  Dependencies: {}", profile.raw.dep_count);
            println!("  no_std:       {}", profile.raw.is_no_std);
        }
        None => {
            eprintln!("Crate '{}' not found in Octo-Index.", crate_name);
            eprintln!();
            // Suggest similar names
            let matches: Vec<_> = index
                .profiles
                .keys()
                .filter(|k| k.contains(&crate_name) || crate_name.contains(k.as_str()))
                .take(5)
                .collect();
            if !matches.is_empty() {
                eprintln!("Did you mean one of these?");
                for name in matches {
                    eprintln!("  {}", name);
                }
            }
            std::process::exit(1);
        }
    }
}

fn cmd_train_mapper(args: &[String]) {
    let mut octo_index_path: Option<String> = None;
    let mut output_path = "contrastive-mapper.bin".to_string();
    let mut epochs = 1000;
    let mut learning_rate = 0.5;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--octo-index" | "-i" => {
                if i + 1 < args.len() {
                    octo_index_path = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            "-o" | "--output" => {
                if i + 1 < args.len() {
                    output_path = args[i + 1].clone();
                    i += 1;
                }
            }
            "--epochs" => {
                if i + 1 < args.len() {
                    epochs = args[i + 1].parse().unwrap_or(1000);
                    i += 1;
                }
            }
            "--lr" => {
                if i + 1 < args.len() {
                    learning_rate = args[i + 1].parse().unwrap_or(0.5);
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }

    let octo_index_path = match octo_index_path {
        Some(p) => p,
        None => {
            eprintln!("usage: cratefind train-mapper --octo-index <path> [-o <output>]");
            eprintln!();
            eprintln!("Options:");
            eprintln!("  --octo-index <path>  Path to the Octo-Index file (required)");
            eprintln!(
                "  -o <path>            Output path for the mapper (default: contrastive-mapper.bin)"
            );
            eprintln!("  --epochs <n>         Number of training epochs (default: 1000)");
            eprintln!("  --lr <rate>          Learning rate (default: 0.5)");
            std::process::exit(1);
        }
    };

    // Load the Octo-Index
    println!("Loading Octo-Index from {} ...", octo_index_path);
    let index = match octo_index::OctoIndex::load(std::path::Path::new(&octo_index_path)) {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("Failed to load Octo-Index: {}", e);
            std::process::exit(1);
        }
    };

    println!("  {} crates loaded", index.count);

    // Initialize embedder
    println!("Loading embedding model ...");
    let mut embedder = embed::Embedder::new().expect("Failed to load embedding model");

    // Prepare training data: embed crate descriptions
    println!("Generating embeddings for crate descriptions ...");
    let profiles: Vec<_> = index.profiles.values().collect();

    // Create description texts (crate name + metadata hints)
    let descriptions: Vec<String> = profiles
        .iter()
        .map(|p| {
            // Create a rich description from the profile
            let mut desc = format!("{} - Rust crate", p.name);

            // Add semantic hints based on octonion coefficients
            if p.coeffs[3] > 0.3 {
                desc.push_str(" async asynchronous");
            }
            if p.coeffs[1] > 0.3 {
                desc.push_str(" thread-safe Send Sync concurrent");
            }
            if p.coeffs[6] > 0.5 {
                desc.push_str(" no_std embedded bare-metal");
            }
            if p.coeffs[2] < 0.1 {
                desc.push_str(" safe memory-safe");
            }
            if p.coeffs[5] < 0.2 {
                desc.push_str(" lightweight minimal zero-dependency");
            }

            desc
        })
        .collect();

    // Batch embed
    let batch_size = 64;
    let mut all_embeddings: Vec<Vec<f32>> = Vec::with_capacity(profiles.len());

    for (batch_idx, batch) in descriptions.chunks(batch_size).enumerate() {
        let batch_strings: Vec<String> = batch.to_vec();
        match embedder.embed(&batch_strings) {
            Ok(embs) => {
                all_embeddings.extend(embs);
                print!(
                    "\r  Embedded {}/{} crates",
                    (batch_idx + 1) * batch_size.min(batch.len()),
                    profiles.len()
                );
                std::io::Write::flush(&mut std::io::stdout()).ok();
            }
            Err(e) => {
                eprintln!("\nFailed to embed batch: {}", e);
                std::process::exit(1);
            }
        }
    }
    println!();

    // Extract targets (8D coefficients)
    let targets: Vec<[f32; 8]> = profiles.iter().map(|p| p.coeffs).collect();

    // Train the mapper
    println!(
        "Training contrastive mapper (epochs={}, lr={}) ...",
        epochs, learning_rate
    );
    let mut mapper = contrastive::ContrastiveMapper::new_random();

    let initial_loss = mapper.compute_loss(&all_embeddings, &targets);
    println!("  Initial loss: {:.6}", initial_loss);

    let final_loss = mapper.train(&all_embeddings, &targets, learning_rate, epochs, true);

    println!("  Final loss: {:.6}", final_loss);
    println!(
        "  Improvement: {:.1}%",
        (1.0 - final_loss / initial_loss) * 100.0
    );

    // Save the mapper
    let output_path = std::path::Path::new(&output_path);
    mapper.save(output_path).expect("Failed to save mapper");

    let file_size = std::fs::metadata(output_path).map(|m| m.len()).unwrap_or(0);
    println!();
    println!(
        "Saved mapper to {} ({} bytes)",
        output_path.display(),
        file_size
    );
    println!();

    // Test a few predictions
    println!("Sample predictions:");
    for p in profiles.iter().take(5) {
        let idx = profiles.iter().position(|x| x.name == p.name).unwrap();
        let pred = mapper.forward(&all_embeddings[idx]);
        println!("  {}: target={:?}", p.name, &p.coeffs[..4]);
        println!("       pred  ={:?}", &pred[..4]);
    }
}

fn cmd_semantic_search(query: &str, args: &[String]) {
    let mut octo_index_path: Option<String> = None;
    let mut mapper_path: Option<String> = None;
    let mut limit = 10;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--octo-index" | "-i" => {
                if i + 1 < args.len() {
                    octo_index_path = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            "--mapper" | "-m" => {
                if i + 1 < args.len() {
                    mapper_path = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            "-n" | "--limit" => {
                if i + 1 < args.len() {
                    limit = args[i + 1].parse().unwrap_or(10);
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }

    let octo_index_path = match octo_index_path {
        Some(p) => p,
        None => {
            eprintln!(
                "usage: cratefind semantic-search <query> --mapper <path> --octo-index <path>"
            );
            eprintln!();
            eprintln!("Options:");
            eprintln!("  --octo-index <path>  Path to the Octo-Index file (required)");
            eprintln!("  --mapper <path>      Path to the trained mapper (required)");
            eprintln!("  -n <limit>           Number of results (default: 10)");
            std::process::exit(1);
        }
    };

    let mapper_path = match mapper_path {
        Some(p) => p,
        None => {
            eprintln!("No mapper file specified. Use --mapper <path>");
            std::process::exit(1);
        }
    };

    // Load mapper
    let mapper = match contrastive::ContrastiveMapper::load(std::path::Path::new(&mapper_path)) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Failed to load mapper: {}", e);
            std::process::exit(1);
        }
    };

    // Load index
    let index = match octo_index::OctoIndex::load(std::path::Path::new(&octo_index_path)) {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("Failed to load Octo-Index: {}", e);
            std::process::exit(1);
        }
    };

    // Embed query
    let mut embedder = embed::Embedder::new().expect("Failed to load embedding model");
    let query_embedding = embedder.embed_one(query).expect("Failed to embed query");

    // Project to 8D
    let query_8d = mapper.forward(&query_embedding);

    println!("Query: \"{}\"", query);
    println!();
    println!("Projected to 8D:");
    println!(
        "  e0={:.2} e1={:.2} e2={:.2} e3={:.2} e4={:.2} e5={:.2} e6={:.2} e7={:.2}",
        query_8d[0],
        query_8d[1],
        query_8d[2],
        query_8d[3],
        query_8d[4],
        query_8d[5],
        query_8d[6],
        query_8d[7]
    );
    println!();

    // Search using the projected query
    let results = index.search(&query_8d, limit);

    println!("Top {} results from {} crates:", limit, index.count);
    println!("{:<30} {:>10} {:>8}", "CRATE", "VERSION", "SCORE");
    println!("{}", "-".repeat(52));

    for (profile, score) in results {
        println!(
            "{:<30} {:>10} {:>8.3}",
            profile.name, profile.version, score
        );
    }
}

fn cmd_braid_check(project: &project::RustProject, _args: &[String]) {
    println!("Braid Analysis: {}", project.name);
    println!("{} dependencies", project.deps.len());
    println!();

    // Build braid word from dependencies
    let deps_with_profiles: Vec<(String, Option<octo_index::OctonionProfile>)> = project
        .deps
        .iter()
        .map(|dep| {
            let profile = parse::find_crate_source(&dep.name, &dep.version)
                .ok()
                .and_then(|source_dir| {
                    profile::CrateProfile::from_source(&dep.name, &dep.version, &source_dir).ok()
                })
                .map(|cp| {
                    let coeffs_f64 = profile::octonion_coeffs(&cp.octonion);
                    let coeffs: [f32; 8] = [
                        coeffs_f64[0] as f32,
                        coeffs_f64[1] as f32,
                        coeffs_f64[2] as f32,
                        coeffs_f64[3] as f32,
                        coeffs_f64[4] as f32,
                        coeffs_f64[5] as f32,
                        coeffs_f64[6] as f32,
                        coeffs_f64[7] as f32,
                    ];
                    octo_index::OctonionProfile {
                        name: dep.name.clone(),
                        version: dep.version.clone(),
                        coeffs,
                        raw: octo_index::RawMetrics::default(),
                    }
                });
            (dep.name.clone(), profile)
        })
        .collect();

    let word = braid::BraidWord::from_deps(&deps_with_profiles);

    println!("Braid word: {} generators", word.generators.len());
    if word.generators.len() <= 10 {
        println!("  {}", word.to_named_string());
    } else {
        // Show first 5 and last 5
        let first: Vec<_> = word
            .generators
            .iter()
            .take(5)
            .map(|g| g.name.clone())
            .collect();
        let last: Vec<_> = word
            .generators
            .iter()
            .rev()
            .take(5)
            .rev()
            .map(|g| g.name.clone())
            .collect();
        println!("  {} → ... → {}", first.join(" → "), last.join(" → "));
    }
    println!();

    // Find tangle points
    let tangles = word.find_tangle_points();
    if !tangles.is_empty() {
        println!("Potential tangle points detected: {}", tangles.len());
        for &pos in tangles.iter().take(5) {
            if pos + 2 < word.generators.len() {
                let a = &word.generators[pos];
                let b = &word.generators[pos + 1];
                let c = &word.generators[pos + 2];
                println!("  Position {}: {} ↔ {} ↔ {}", pos, a.name, b.name, c.name);
            }
        }
        println!();
    }

    // Extract trait bounds and check for crossings
    println!("Analyzing trait bounds...");
    let mut bounds_cache: std::collections::HashMap<String, braid::crossing::ExtractedBounds> =
        std::collections::HashMap::new();

    // Extract bounds for each dependency (limited to first 20 for speed)
    for dep in project.deps.iter().take(20) {
        if let Ok(bounds) = parse::extract_bounds(&dep.name, &dep.version) {
            bounds_cache.insert(dep.name.clone(), bounds);
        }
    }

    println!("  Extracted bounds from {} crates", bounds_cache.len());
    println!();

    // Check for crossings between async crates
    let async_crates: Vec<_> = bounds_cache
        .iter()
        .filter(|(_, b)| b.is_async())
        .map(|(name, _)| name.clone())
        .collect();

    if async_crates.len() > 1 {
        println!("Async crates detected ({}):", async_crates.len());
        for name in &async_crates {
            println!("  {}", name);
        }
        println!();

        // Check for conflicts between async crates
        println!("Checking for async crossing conflicts...");
        let mut conflicts_found = 0;

        for i in 0..async_crates.len() {
            for j in (i + 1)..async_crates.len() {
                let name_a = &async_crates[i];
                let name_b = &async_crates[j];

                let bounds_a = &bounds_cache[name_a];
                let bounds_b = &bounds_cache[name_b];

                let crossings =
                    braid::crossing::detect_crossings_heuristic(name_a, bounds_a, name_b, bounds_b);

                if !crossings.is_empty() {
                    conflicts_found += 1;
                    println!();
                    println!("  Crossing: {} ↔ {}", name_a, name_b);
                    for crossing in &crossings {
                        println!("    {}", crossing.describe());
                    }

                    // Get profiles for deeper analysis
                    let profile_a = deps_with_profiles
                        .iter()
                        .find(|(n, _)| n == name_a)
                        .and_then(|(_, p)| p.as_ref());
                    let profile_b = deps_with_profiles
                        .iter()
                        .find(|(n, _)| n == name_b)
                        .and_then(|(_, p)| p.as_ref());

                    if let (Some(pa), Some(pb)) = (profile_a, profile_b) {
                        // Check Fano compatibility
                        let fano_score = braid::fano::check_all_lines(&pa.coeffs, &pb.coeffs);
                        println!("    Fano compatibility: {:.2}", fano_score);

                        // Check octonion parity
                        let parity = braid::fano::octonion_parity(&pa.coeffs, &pb.coeffs);
                        match parity {
                            braid::fano::OctonionParity::Real { confidence } => {
                                println!(
                                    "    Octonion parity: REAL (confidence: {:.2})",
                                    confidence
                                );
                                println!("    → Crossing may be resolvable with a shim");
                            }
                            braid::fano::OctonionParity::Imaginary { residue } => {
                                println!(
                                    "    Octonion parity: IMAGINARY (residue: {:.2})",
                                    residue
                                );
                                println!("    → Essential tangle - may need architectural change");
                            }
                        }

                        // Analyze crossing
                        for crossing in &crossings {
                            let analysis = braid::analyze_crossing(pa, pb, crossing);
                            match analysis {
                                braid::CrossingAnalysis::Clean => {
                                    println!("    Analysis: Clean crossing");
                                }
                                braid::CrossingAnalysis::Resolvable {
                                    template,
                                    fano_match,
                                } => {
                                    println!(
                                        "    Analysis: Resolvable via '{}' template (fano={:.2})",
                                        template, fano_match
                                    );
                                }
                                braid::CrossingAnalysis::Essential {
                                    conflict,
                                    suggestion,
                                } => {
                                    println!("    Analysis: ESSENTIAL TANGLE");
                                    println!("      Conflict: {}", conflict.description);
                                    println!("      Suggestion: {}", suggestion);
                                }
                            }
                        }
                    }
                }
            }
        }

        if conflicts_found == 0 {
            println!("  No crossing conflicts detected between async crates.");
        }
    } else if async_crates.len() == 1 {
        println!(
            "Single async crate: {} - no runtime conflicts possible",
            async_crates[0]
        );
    } else {
        println!("No async crates detected in dependencies.");
    }

    println!();
    println!("Braid analysis complete.");
}
