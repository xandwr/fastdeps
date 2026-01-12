mod db;
mod embed;
mod parse;
mod project;
mod usage;

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
            eprintln!("EXAMPLES:");
            eprintln!("  cratefind index");
            eprintln!("  cratefind search \"serialize to json\"");
            eprintln!("  cratefind learn serde");
            eprintln!("  cratefind usage anyhow");
            eprintln!("  cratefind explain serde::de::DeserializeOwned");
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
