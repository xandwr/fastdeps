//! Global SQLite database for crate symbol embeddings.
//!
//! Stored at ~/.cache/cratefind/index.sqlite

use rusqlite::{Connection, params};
use std::path::PathBuf;

use crate::embed::Embedding;

/// A symbol extracted from a crate
#[derive(Debug, Clone)]
pub struct Symbol {
    pub path: String, // e.g. "serde::Serialize"
    pub kind: String, // e.g. "trait", "struct", "fn"
    pub signature: Option<String>,
}

/// A search result
#[derive(Debug)]
#[allow(dead_code)]
pub struct SearchResult {
    pub crate_name: String,
    pub crate_version: String,
    pub path: String,
    pub kind: String,
    pub signature: Option<String>,
    pub score: f32,
}

/// Database statistics
#[derive(Debug)]
pub struct Stats {
    pub crate_count: usize,
    pub symbol_count: usize,
    pub db_size_bytes: u64,
}

pub struct Database {
    conn: Connection,
}

impl Database {
    /// Get the path to the global database
    pub fn path() -> PathBuf {
        dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("cratefind")
            .join("index.sqlite")
    }

    /// Open or create the database
    pub fn open() -> Result<Self, rusqlite::Error> {
        let path = Self::path();

        // Ensure directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        let conn = Connection::open(&path)?;

        // Enable WAL mode for concurrent access
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA cache_size = -64000;",
        )?;

        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    fn init_schema(&self) -> Result<(), rusqlite::Error> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS crates (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                version TEXT NOT NULL,
                indexed_at INTEGER NOT NULL,
                UNIQUE(name, version)
            );

            CREATE TABLE IF NOT EXISTS symbols (
                id INTEGER PRIMARY KEY,
                crate_id INTEGER NOT NULL REFERENCES crates(id) ON DELETE CASCADE,
                path TEXT NOT NULL,
                kind TEXT NOT NULL,
                signature TEXT,
                embedding BLOB NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_crates_name_version ON crates(name, version);
            CREATE INDEX IF NOT EXISTS idx_symbols_crate ON symbols(crate_id);",
        )?;
        Ok(())
    }

    /// Check if a crate version is already indexed
    pub fn is_indexed(&self, name: &str, version: &str) -> Result<bool, rusqlite::Error> {
        let count: i32 = self.conn.query_row(
            "SELECT COUNT(*) FROM crates WHERE name = ? AND version = ?",
            params![name, version],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Get crate ID if it exists
    pub fn get_crate_id(&self, name: &str, version: &str) -> Result<Option<i64>, rusqlite::Error> {
        let result = self.conn.query_row(
            "SELECT id FROM crates WHERE name = ? AND version = ?",
            params![name, version],
            |row| row.get(0),
        );

        match result {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Index a crate's symbols with their embeddings
    pub fn index_crate(
        &self,
        name: &str,
        version: &str,
        symbols: &[Symbol],
        embeddings: &[Embedding],
    ) -> Result<(), rusqlite::Error> {
        assert_eq!(symbols.len(), embeddings.len());

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        // Insert crate
        self.conn.execute(
            "INSERT OR REPLACE INTO crates (name, version, indexed_at) VALUES (?, ?, ?)",
            params![name, version, now],
        )?;

        let crate_id = self.conn.last_insert_rowid();

        // Delete old symbols (if re-indexing)
        self.conn
            .execute("DELETE FROM symbols WHERE crate_id = ?", params![crate_id])?;

        // Insert symbols with embeddings
        let mut stmt = self.conn.prepare(
            "INSERT INTO symbols (crate_id, path, kind, signature, embedding) VALUES (?, ?, ?, ?, ?)",
        )?;

        for (symbol, embedding) in symbols.iter().zip(embeddings.iter()) {
            let embedding_bytes = embedding_to_bytes(embedding);
            stmt.execute(params![
                crate_id,
                symbol.path,
                symbol.kind,
                symbol.signature,
                embedding_bytes
            ])?;
        }

        Ok(())
    }

    /// Search for symbols similar to query embedding, scoped to given crate IDs
    pub fn search(
        &self,
        query: &Embedding,
        crate_ids: &[i64],
        limit: usize,
    ) -> Result<Vec<SearchResult>, rusqlite::Error> {
        if crate_ids.is_empty() {
            return Ok(vec![]);
        }

        // Build IN clause
        let placeholders: Vec<&str> = crate_ids.iter().map(|_| "?").collect();
        let in_clause = placeholders.join(",");

        let sql = format!(
            "SELECT s.path, s.kind, s.signature, s.embedding, c.name, c.version
             FROM symbols s
             JOIN crates c ON s.crate_id = c.id
             WHERE s.crate_id IN ({in_clause})"
        );

        let mut stmt = self.conn.prepare(&sql)?;

        // Bind crate IDs
        let params: Vec<&dyn rusqlite::ToSql> = crate_ids
            .iter()
            .map(|id| id as &dyn rusqlite::ToSql)
            .collect();

        let rows = stmt.query_map(params.as_slice(), |row| {
            let path: String = row.get(0)?;
            let kind: String = row.get(1)?;
            let signature: Option<String> = row.get(2)?;
            let embedding_bytes: Vec<u8> = row.get(3)?;
            let crate_name: String = row.get(4)?;
            let crate_version: String = row.get(5)?;

            let embedding = bytes_to_embedding(&embedding_bytes);
            let score = cosine_similarity(query, &embedding);

            Ok(SearchResult {
                crate_name,
                crate_version,
                path,
                kind,
                signature,
                score,
            })
        })?;

        let mut results: Vec<SearchResult> = rows.filter_map(|r| r.ok()).collect();

        // Sort by score descending
        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());
        results.truncate(limit);

        Ok(results)
    }

    /// Get database statistics
    pub fn stats(&self) -> Result<Stats, rusqlite::Error> {
        let crate_count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM crates", [], |row| row.get(0))?;

        let symbol_count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))?;

        let path = Self::path();
        let db_size_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

        Ok(Stats {
            crate_count: crate_count as usize,
            symbol_count: symbol_count as usize,
            db_size_bytes,
        })
    }
}

/// Convert embedding to bytes for storage
fn embedding_to_bytes(embedding: &Embedding) -> Vec<u8> {
    embedding.iter().flat_map(|f| f.to_le_bytes()).collect()
}

/// Convert bytes back to embedding
fn bytes_to_embedding(bytes: &[u8]) -> Embedding {
    bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

/// Cosine similarity between two embeddings
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}
