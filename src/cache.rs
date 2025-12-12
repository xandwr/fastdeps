//! SQLite cache for indexed crate data with WAL mode for parallel writes.

use crate::cargo::RegistryCrate;
use crate::languages::rust::RustParser;
use crate::schema::Item;
use camino::Utf8PathBuf;
use rayon::prelude::*;
use rusqlite::{Connection, params};
use std::fs;
use std::sync::Mutex;
use thiserror::Error;

const CACHE_DIR: &str = ".fastdeps";
const DB_FILE: &str = "cache.sqlite";
const SCHEMA_VERSION: i32 = 1;

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Cache not initialized. Run `fastdeps cache build` first.")]
    NotInitialized,
}

pub struct Cache {
    conn: Connection,
}

impl Cache {
    /// Open or create the cache database in the current directory.
    pub fn open() -> Result<Self, CacheError> {
        let cache_dir = Utf8PathBuf::from(CACHE_DIR);
        if !cache_dir.exists() {
            fs::create_dir_all(&cache_dir)?;
        }

        let db_path = cache_dir.join(DB_FILE);
        let conn = Connection::open(&db_path)?;

        // Enable WAL mode for better concurrent access
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA cache_size = -64000;
            PRAGMA busy_timeout = 5000;
            "#,
        )?;

        let cache = Self { conn };
        cache.init_schema()?;
        Ok(cache)
    }

    /// Open existing cache, error if it doesn't exist.
    pub fn open_existing() -> Result<Self, CacheError> {
        let db_path = Utf8PathBuf::from(CACHE_DIR).join(DB_FILE);
        if !db_path.exists() {
            return Err(CacheError::NotInitialized);
        }

        let conn = Connection::open(&db_path)?;

        // Enable WAL mode for reads too
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA busy_timeout = 5000;
            "#,
        )?;

        Ok(Self { conn })
    }

    /// Check if cache exists.
    pub fn exists() -> bool {
        Utf8PathBuf::from(CACHE_DIR).join(DB_FILE).exists()
    }

    /// Get the database path.
    pub fn db_path() -> Utf8PathBuf {
        Utf8PathBuf::from(CACHE_DIR).join(DB_FILE)
    }

    fn init_schema(&self) -> Result<(), CacheError> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY,
                value TEXT
            );

            CREATE TABLE IF NOT EXISTS crates (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                version TEXT NOT NULL,
                path TEXT NOT NULL,
                indexed_at INTEGER NOT NULL,
                UNIQUE(name, version)
            );

            CREATE TABLE IF NOT EXISTS items (
                id INTEGER PRIMARY KEY,
                crate_id INTEGER NOT NULL REFERENCES crates(id) ON DELETE CASCADE,
                path TEXT NOT NULL,
                kind TEXT NOT NULL,
                signature TEXT,
                doc TEXT,
                visibility TEXT NOT NULL,
                UNIQUE(crate_id, path)
            );

            CREATE INDEX IF NOT EXISTS idx_items_path ON items(path);
            CREATE INDEX IF NOT EXISTS idx_items_kind ON items(kind);
            CREATE INDEX IF NOT EXISTS idx_crates_name ON crates(name);
            "#,
        )?;

        // Set schema version if not present
        self.conn.execute(
            "INSERT OR IGNORE INTO meta (key, value) VALUES ('schema_version', ?)",
            params![SCHEMA_VERSION.to_string()],
        )?;

        Ok(())
    }

    /// Clear all cached data.
    pub fn clear(&self) -> Result<(), CacheError> {
        self.conn.execute_batch(
            r#"
            DELETE FROM items;
            DELETE FROM crates;
            "#,
        )?;
        Ok(())
    }

    /// Check if a crate version is already indexed.
    pub fn is_indexed(&self, name: &str, version: &str) -> Result<bool, CacheError> {
        let count: i32 = self.conn.query_row(
            "SELECT COUNT(*) FROM crates WHERE name = ? AND version = ?",
            params![name, version],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Get list of already indexed crate name@version pairs.
    pub fn get_indexed_set(&self) -> Result<std::collections::HashSet<String>, CacheError> {
        let mut stmt = self.conn.prepare("SELECT name, version FROM crates")?;
        let results = stmt
            .query_map([], |row| {
                let name: String = row.get(0)?;
                let version: String = row.get(1)?;
                Ok(format!("{}@{}", name, version))
            })?
            .collect::<Result<std::collections::HashSet<_>, _>>()?;
        Ok(results)
    }

    /// Index a single crate (used for batch inserts).
    pub fn index_crate(&self, krate: &RegistryCrate, items: &[Item]) -> Result<(), CacheError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        // Use a transaction for atomicity
        self.conn.execute("BEGIN IMMEDIATE", [])?;

        // Insert or replace crate
        self.conn.execute(
            r#"
            INSERT OR REPLACE INTO crates (name, version, path, indexed_at)
            VALUES (?, ?, ?, ?)
            "#,
            params![krate.name, krate.version, krate.path.as_str(), now],
        )?;

        let crate_id: i64 = self.conn.query_row(
            "SELECT id FROM crates WHERE name = ? AND version = ?",
            params![krate.name, krate.version],
            |row| row.get(0),
        )?;

        // Delete old items for this crate
        self.conn
            .execute("DELETE FROM items WHERE crate_id = ?", params![crate_id])?;

        // Insert items
        let mut stmt = self.conn.prepare(
            r#"
            INSERT INTO items (crate_id, path, kind, signature, doc, visibility)
            VALUES (?, ?, ?, ?, ?, ?)
            "#,
        )?;

        for item in items {
            let kind = format!("{:?}", item.kind).to_lowercase();
            let vis = format!("{:?}", item.visibility).to_lowercase();
            stmt.execute(params![
                crate_id,
                item.path,
                kind,
                item.signature,
                item.doc,
                vis
            ])?;
        }

        self.conn.execute("COMMIT", [])?;
        Ok(())
    }

    /// Batch insert multiple crates' data.
    pub fn batch_index(&self, batch: &[(RegistryCrate, Vec<Item>)]) -> Result<(), CacheError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        self.conn.execute("BEGIN IMMEDIATE", [])?;

        for (krate, items) in batch {
            // Insert or replace crate
            self.conn.execute(
                r#"
                INSERT OR REPLACE INTO crates (name, version, path, indexed_at)
                VALUES (?, ?, ?, ?)
                "#,
                params![krate.name, krate.version, krate.path.as_str(), now],
            )?;

            let crate_id: i64 = self.conn.query_row(
                "SELECT id FROM crates WHERE name = ? AND version = ?",
                params![krate.name, krate.version],
                |row| row.get(0),
            )?;

            // Delete old items for this crate
            self.conn
                .execute("DELETE FROM items WHERE crate_id = ?", params![crate_id])?;

            // Insert items (use OR REPLACE in case of duplicate paths from re-exports)
            let mut stmt = self.conn.prepare_cached(
                r#"
                INSERT OR REPLACE INTO items (crate_id, path, kind, signature, doc, visibility)
                VALUES (?, ?, ?, ?, ?, ?)
                "#,
            )?;

            for item in items {
                let kind = format!("{:?}", item.kind).to_lowercase();
                let vis = format!("{:?}", item.visibility).to_lowercase();
                stmt.execute(params![
                    crate_id,
                    item.path,
                    kind,
                    item.signature,
                    item.doc,
                    vis
                ])?;
            }
        }

        self.conn.execute("COMMIT", [])?;
        Ok(())
    }

    /// Search for items matching a query.
    pub fn search(&self, query: &str) -> Result<Vec<SearchResult>, CacheError> {
        let pattern = format!("%{}%", query.to_lowercase());

        let mut stmt = self.conn.prepare(
            r#"
            SELECT c.name, c.version, i.path, i.kind, i.signature
            FROM items i
            JOIN crates c ON i.crate_id = c.id
            WHERE LOWER(i.path) LIKE ?
            ORDER BY c.name, c.version, i.path
            "#,
        )?;

        let results = stmt
            .query_map(params![pattern], |row| {
                Ok(SearchResult {
                    crate_name: row.get(0)?,
                    crate_version: row.get(1)?,
                    path: row.get(2)?,
                    kind: row.get(3)?,
                    signature: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(results)
    }

    /// Search within a specific crate.
    pub fn search_crate(
        &self,
        crate_name: &str,
        crate_version: Option<&str>,
    ) -> Result<Vec<CachedItem>, CacheError> {
        let mut query = String::from(
            r#"
            SELECT i.path, i.kind, i.signature, i.doc, i.visibility
            FROM items i
            JOIN crates c ON i.crate_id = c.id
            WHERE c.name = ?
            "#,
        );

        if crate_version.is_some() {
            query.push_str(" AND c.version = ?");
        } else {
            // Get latest version
            query.push_str(" AND c.version = (SELECT MAX(version) FROM crates WHERE name = ?)");
        }
        query.push_str(" ORDER BY i.path");

        let mut stmt = self.conn.prepare(&query)?;

        let version_param = crate_version.unwrap_or(crate_name);
        let results = stmt
            .query_map(params![crate_name, version_param], |row| {
                Ok(CachedItem {
                    path: row.get(0)?,
                    kind: row.get(1)?,
                    signature: row.get(2)?,
                    doc: row.get(3)?,
                    visibility: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(results)
    }

    /// Get all indexed crates.
    pub fn list_indexed(&self) -> Result<Vec<(String, String)>, CacheError> {
        let mut stmt = self
            .conn
            .prepare("SELECT name, version FROM crates ORDER BY name, version")?;

        let results = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(results)
    }

    /// Get stats about the cache.
    pub fn stats(&self) -> Result<CacheStats, CacheError> {
        let crate_count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM crates", [], |row| row.get(0))?;

        let item_count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))?;

        let db_path = Utf8PathBuf::from(CACHE_DIR).join(DB_FILE);
        let db_size = fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);

        Ok(CacheStats {
            crate_count: crate_count as usize,
            item_count: item_count as usize,
            db_size_bytes: db_size,
        })
    }
}

#[derive(Debug)]
pub struct SearchResult {
    pub crate_name: String,
    pub crate_version: String,
    pub path: String,
    pub kind: String,
    pub signature: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct CachedItem {
    pub path: String,
    pub kind: String,
    pub signature: Option<String>,
    pub doc: Option<String>,
    pub visibility: String,
}

#[derive(Debug)]
pub struct CacheStats {
    pub crate_count: usize,
    pub item_count: usize,
    pub db_size_bytes: u64,
}

/// Parsed crate data ready for insertion.
#[derive(Clone)]
pub struct ParsedCrate {
    pub krate: RegistryCrate,
    pub items: Vec<Item>,
}

/// Parse a single crate (CPU-bound, parallelizable).
pub fn parse_crate(krate: &RegistryCrate) -> Result<ParsedCrate, String> {
    let mut parser = RustParser::new().map_err(|e| e.to_string())?;
    let mut all_items: Vec<Item> = Vec::new();

    for source_file in krate.source_files() {
        let relative = source_file
            .strip_prefix(&krate.path)
            .unwrap_or(&source_file);
        let module_path = crate::path_to_module(&krate.name, relative);

        if let Ok(source) = fs::read_to_string(&source_file) {
            if let Ok(items) = parser.parse_source(&source, &module_path) {
                all_items.extend(items);
            }
        }
    }

    Ok(ParsedCrate {
        krate: krate.clone(),
        items: all_items,
    })
}

/// Index multiple crates in parallel using rayon for parsing,
/// then batch-insert into SQLite.
pub fn parallel_index(
    crates: &[RegistryCrate],
    force: bool,
) -> Result<IndexStats, Box<dyn std::error::Error + Send + Sync>> {
    let cache = Cache::open()?;

    // Get already indexed set if not forcing
    let indexed_set = if force {
        std::collections::HashSet::new()
    } else {
        cache.get_indexed_set()?
    };

    // Filter to crates that need indexing
    let to_index: Vec<_> = crates
        .iter()
        .filter(|k| force || !indexed_set.contains(&format!("{}@{}", k.name, k.version)))
        .collect();

    let skipped = crates.len() - to_index.len();

    if to_index.is_empty() {
        return Ok(IndexStats {
            indexed: 0,
            skipped,
            failed: 0,
            total_items: 0,
        });
    }

    eprintln!("Parsing {} crates in parallel...", to_index.len());

    // Parse in parallel using rayon
    let results: Vec<_> = to_index
        .par_iter()
        .map(|krate| {
            let result = parse_crate(krate);
            // Print progress
            match &result {
                Ok(parsed) => eprintln!(
                    "  {}@{} - {} items",
                    krate.name,
                    krate.version,
                    parsed.items.len()
                ),
                Err(e) => eprintln!("  {}@{} - error: {}", krate.name, krate.version, e),
            }
            result
        })
        .collect();

    // Separate successes and failures
    let mut successes: Vec<(RegistryCrate, Vec<Item>)> = Vec::new();
    let mut failed = 0;

    for result in results {
        match result {
            Ok(parsed) => successes.push((parsed.krate, parsed.items)),
            Err(_) => failed += 1,
        }
    }

    let total_items: usize = successes.iter().map(|(_, items)| items.len()).sum();
    let indexed = successes.len();

    // Batch insert into database
    eprintln!("Writing {} crates to cache...", indexed);

    // Insert in chunks to avoid holding transactions too long
    const CHUNK_SIZE: usize = 50;
    for chunk in successes.chunks(CHUNK_SIZE) {
        cache.batch_index(chunk)?;
    }

    Ok(IndexStats {
        indexed,
        skipped,
        failed,
        total_items,
    })
}

#[derive(Debug)]
pub struct IndexStats {
    pub indexed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub total_items: usize,
}
