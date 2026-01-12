mod db;
mod embed;
mod project;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Must be in a Rust project
    let project = match project::RustProject::discover() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!("hint: run this from a Rust project directory (with Cargo.toml)");
            std::process::exit(1);
        }
    };

    match args.get(1).map(|s| s.as_str()) {
        Some("index") => cmd_index(&project),
        Some("search") => {
            let query = args[2..].join(" ");
            if query.is_empty() {
                eprintln!("usage: cratefind search <query>");
                std::process::exit(1);
            }
            cmd_search(&project, &query);
        }
        Some("stats") => cmd_stats(&project),
        _ => {
            eprintln!("usage: cratefind <command>");
            eprintln!();
            eprintln!("commands:");
            eprintln!("  index   Index dependencies for this project");
            eprintln!("  search  Semantic search across indexed deps");
            eprintln!("  stats   Show index statistics");
            std::process::exit(1);
        }
    }
}

fn cmd_index(project: &project::RustProject) {
    println!("Indexing dependencies for: {}", project.name);
    println!("Found {} dependencies in Cargo.lock", project.deps.len());

    let db = db::Database::open().expect("Failed to open database");
    let mut embedder = embed::Embedder::new().expect("Failed to load embedding model");

    let mut indexed = 0;
    let mut skipped = 0;

    for dep in &project.deps {
        if db.is_indexed(&dep.name, &dep.version).unwrap_or(false) {
            skipped += 1;
            continue;
        }

        print!("  {}@{} ... ", dep.name, dep.version);

        // TODO: Actually parse the crate and extract symbols
        // For now, just index the crate name as a placeholder
        let symbols = vec![db::Symbol {
            path: format!("{}::lib", dep.name),
            kind: "module".to_string(),
            signature: None,
        }];

        // Generate embeddings for symbols
        let texts: Vec<String> = symbols.iter().map(|s| s.path.clone()).collect();
        let embeddings = embedder.embed(&texts).expect("Embedding failed");

        db.index_crate(&dep.name, &dep.version, &symbols, &embeddings)
            .expect("Failed to index crate");

        println!("{} symbols", symbols.len());
        indexed += 1;
    }

    println!();
    println!("Done: {indexed} indexed, {skipped} already cached");
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
        println!(
            "{}. {} ({}) [{:.2}]",
            i + 1,
            result.path,
            result.kind,
            result.score
        );
        if let Some(sig) = &result.signature {
            println!("   {sig}");
        }
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
