//! SQLite cache for indexed crate data with WAL mode for parallel writes.

use crate::cargo::RegistryCrate;
use crate::languages::rust::RustParser;
use crate::schema::Item;
use camino::Utf8PathBuf;
use rayon::prelude::*;
use rusqlite::{Connection, params};
use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use thiserror::Error;

const CACHE_DIR: &str = ".fastdeps";
const DB_FILE: &str = "cache.sqlite";
const SCHEMA_VERSION: i32 = 2;

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
        // Create base tables
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

        // Check current schema version and migrate if needed
        let current_version: i32 = self
            .conn
            .query_row(
                "SELECT COALESCE((SELECT value FROM meta WHERE key = 'schema_version'), '0')",
                [],
                |row| {
                    let v: String = row.get(0)?;
                    Ok(v.parse().unwrap_or(0))
                },
            )
            .unwrap_or(0);

        if current_version < 2 {
            self.migrate_to_v2()?;
        }

        // Update schema version
        self.conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', ?)",
            params![SCHEMA_VERSION.to_string()],
        )?;

        Ok(())
    }

    /// Migrate schema from v1 to v2: Add FTS5 full-text search
    fn migrate_to_v2(&self) -> Result<(), CacheError> {
        eprintln!("Migrating cache to v2 (adding FTS5 search)...");

        // Create FTS5 virtual table for fast text search
        // Using trigram tokenizer for substring matching
        self.conn.execute_batch(
            r#"
            CREATE VIRTUAL TABLE IF NOT EXISTS items_fts USING fts5(
                path,
                content='items',
                content_rowid='id',
                tokenize='trigram'
            );

            -- Triggers to keep FTS index in sync with items table
            CREATE TRIGGER IF NOT EXISTS items_fts_insert AFTER INSERT ON items BEGIN
                INSERT INTO items_fts(rowid, path) VALUES (new.id, new.path);
            END;

            CREATE TRIGGER IF NOT EXISTS items_fts_delete AFTER DELETE ON items BEGIN
                INSERT INTO items_fts(items_fts, rowid, path) VALUES('delete', old.id, old.path);
            END;

            CREATE TRIGGER IF NOT EXISTS items_fts_update AFTER UPDATE ON items BEGIN
                INSERT INTO items_fts(items_fts, rowid, path) VALUES('delete', old.id, old.path);
                INSERT INTO items_fts(rowid, path) VALUES (new.id, new.path);
            END;
            "#,
        )?;

        // Rebuild FTS index from existing data
        let item_count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))?;

        if item_count > 0 {
            eprintln!("Rebuilding FTS index for {} items...", item_count);
            self.conn
                .execute("INSERT INTO items_fts(items_fts) VALUES('rebuild')", [])?;
        }

        eprintln!("Migration to v2 complete.");
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

        // Pre-prepare statements for better performance
        let mut crate_stmt = self.conn.prepare_cached(
            "INSERT OR REPLACE INTO crates (name, version, path, indexed_at) VALUES (?, ?, ?, ?)",
        )?;
        let mut delete_stmt = self
            .conn
            .prepare_cached("DELETE FROM items WHERE crate_id = ?")?;
        let mut item_stmt = self.conn.prepare_cached(
            "INSERT OR REPLACE INTO items (crate_id, path, kind, signature, doc, visibility) VALUES (?, ?, ?, ?, ?, ?)",
        )?;

        for (krate, items) in batch {
            // Insert or replace crate and get ID via last_insert_rowid
            crate_stmt.execute(params![krate.name, krate.version, krate.path.as_str(), now])?;
            let crate_id = self.conn.last_insert_rowid();

            // Delete old items for this crate
            delete_stmt.execute(params![crate_id])?;

            // Insert items
            for item in items {
                let kind = format!("{:?}", item.kind).to_lowercase();
                let vis = format!("{:?}", item.visibility).to_lowercase();
                item_stmt.execute(params![
                    crate_id,
                    item.path,
                    kind,
                    item.signature,
                    item.doc,
                    vis
                ])?;
            }
        }

        // Drop statements before commit to release borrows
        drop(crate_stmt);
        drop(delete_stmt);
        drop(item_stmt);

        self.conn.execute("COMMIT", [])?;
        Ok(())
    }

    /// Search for items matching a query using FTS5 full-text search.
    pub fn search(&self, query: &str) -> Result<Vec<SearchResult>, CacheError> {
        // Escape special FTS5 characters and prepare for trigram search
        let escaped_query = query.replace('"', "\"\"").to_lowercase();

        // Use FTS5 with trigram tokenizer for fast substring matching
        let mut stmt = self.conn.prepare(
            r#"
            SELECT c.name, c.version, i.path, i.kind, i.signature
            FROM items i
            JOIN crates c ON i.crate_id = c.id
            WHERE i.id IN (SELECT rowid FROM items_fts WHERE items_fts MATCH ?)
            ORDER BY c.name, c.version, i.path
            "#,
        )?;

        let results = stmt
            .query_map(params![format!("\"{}\"", escaped_query)], |row| {
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
/// with streaming writes to SQLite as parsing completes.
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
        .cloned()
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

    let total_to_index = to_index.len();
    eprintln!("Indexing {} crates (parsing + writing)...", total_to_index);

    // Shared counters for progress tracking
    let indexed_count = Arc::new(AtomicUsize::new(0));
    let failed_count = Arc::new(AtomicUsize::new(0));
    let total_items = Arc::new(AtomicUsize::new(0));

    // Channel for streaming parsed crates to writer thread
    let (tx, rx) = mpsc::channel::<ParsedCrate>();

    // Clone counters for writer thread
    let writer_indexed = Arc::clone(&indexed_count);
    let writer_items = Arc::clone(&total_items);

    // Spawn writer thread that batches and writes to SQLite
    let writer_handle = thread::spawn(
        move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            let cache = Cache::open()?;
            let mut batch: Vec<(RegistryCrate, Vec<Item>)> = Vec::new();
            const BATCH_SIZE: usize = 50;

            for parsed in rx {
                batch.push((parsed.krate, parsed.items));

                // Write batch when full
                if batch.len() >= BATCH_SIZE {
                    cache.batch_index(&batch)?;
                    writer_indexed.fetch_add(batch.len(), Ordering::Relaxed);
                    writer_items.fetch_add(
                        batch.iter().map(|(_, items)| items.len()).sum::<usize>(),
                        Ordering::Relaxed,
                    );
                    batch.clear();
                }
            }

            // Flush remaining batch
            if !batch.is_empty() {
                cache.batch_index(&batch)?;
                writer_indexed.fetch_add(batch.len(), Ordering::Relaxed);
                writer_items.fetch_add(
                    batch.iter().map(|(_, items)| items.len()).sum::<usize>(),
                    Ordering::Relaxed,
                );
            }

            Ok(())
        },
    );

    // Clone counter for parser threads
    let parser_failed = Arc::clone(&failed_count);

    // Parse in parallel using rayon, streaming results to writer
    to_index.par_iter().for_each(|krate| {
        match parse_crate(krate) {
            Ok(parsed) => {
                eprintln!(
                    "  {}@{} - {} items",
                    krate.name,
                    krate.version,
                    parsed.items.len()
                );
                // Send to writer (ignore error if receiver dropped)
                let _ = tx.send(parsed);
            }
            Err(e) => {
                eprintln!("  {}@{} - error: {}", krate.name, krate.version, e);
                parser_failed.fetch_add(1, Ordering::Relaxed);
            }
        }
    });

    // Drop sender to signal writer thread to finish
    drop(tx);

    // Wait for writer to complete
    writer_handle
        .join()
        .map_err(|_| "Writer thread panicked")??;

    let indexed = indexed_count.load(Ordering::Relaxed);
    let failed = failed_count.load(Ordering::Relaxed);
    let items = total_items.load(Ordering::Relaxed);

    eprintln!(
        "Done: {} indexed, {} failed, {} items total",
        indexed, failed, items
    );

    Ok(IndexStats {
        indexed,
        skipped,
        failed,
        total_items: items,
    })
}

#[derive(Debug)]
pub struct IndexStats {
    pub indexed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub total_items: usize,
}
