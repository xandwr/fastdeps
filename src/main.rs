mod languages;
mod schema;

use crate::languages::rust::RustParser;
use crate::schema::{Ecosystem, Index, PackageItems, PackageMeta};
use std::collections::BTreeMap;
use std::fs;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: fastdeps <rust-source-file> [module-path]");
        eprintln!("       fastdeps --example");
        std::process::exit(1);
    }

    if args[1] == "--example" {
        print_example();
        return;
    }

    let source_path = &args[1];
    let module_path = args.get(2).map(|s| s.as_str()).unwrap_or("crate");

    let source = match fs::read_to_string(source_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error reading {source_path}: {e}");
            std::process::exit(1);
        }
    };

    let mut parser = match RustParser::new() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error initializing parser: {e}");
            std::process::exit(1);
        }
    };

    match parser.parse_source(&source, module_path) {
        Ok(items) => {
            let package = PackageItems { items };
            let json = serde_json::to_string_pretty(&package).unwrap();
            println!("{json}");
        }
        Err(e) => {
            eprintln!("Parse error: {e}");
            std::process::exit(1);
        }
    }
}

fn print_example() {
    // Show example output format
    let index = Index::new();
    println!("=== index.json ===");
    println!("{}", serde_json::to_string_pretty(&index).unwrap());
    println!();

    let meta = PackageMeta {
        name: "example".into(),
        version: "0.1.0".into(),
        ecosystem: Ecosystem::Rust,
        repository: Some("https://github.com/example/example".into()),
        description: Some("An example package".into()),
        modules: vec!["example::prelude".into()],
        dependencies: BTreeMap::new(),
    };
    println!("=== meta.json ===");
    println!("{}", serde_json::to_string_pretty(&meta).unwrap());
    println!();

    // Parse a sample source
    let sample_source = r#"
/// A configuration builder.
///
/// Use this to construct configurations with a fluent API.
pub struct ConfigBuilder {
    /// The name of the config.
    pub name: String,
    /// Optional timeout in seconds.
    pub timeout: Option<u64>,
}

impl ConfigBuilder {
    /// Create a new builder with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            timeout: None,
        }
    }

    /// Set the timeout.
    pub fn timeout(mut self, secs: u64) -> Self {
        self.timeout = Some(secs);
        self
    }

    /// Build the final config.
    pub fn build(self) -> Config {
        Config {
            name: self.name,
            timeout: self.timeout.unwrap_or(30),
        }
    }
}

impl Default for ConfigBuilder {
    fn default() -> Self {
        Self::new("default")
    }
}

/// The final configuration.
pub struct Config {
    pub name: String,
    pub timeout: u64,
}

/// Errors that can occur.
pub enum ConfigError {
    /// Name was empty.
    EmptyName,
    /// Timeout was invalid.
    InvalidTimeout { value: u64, max: u64 },
}

/// A trait for things that can be configured.
pub trait Configurable {
    /// Apply a configuration.
    fn configure(&mut self, config: &Config);

    /// Get the current configuration.
    fn config(&self) -> Option<&Config>;
}

/// Initialize the system with defaults.
pub fn init() -> Config {
    ConfigBuilder::default().build()
}

/// Maximum allowed timeout.
pub const MAX_TIMEOUT: u64 = 3600;
"#;

    let mut parser = RustParser::new().unwrap();
    let items = parser.parse_source(sample_source, "example").unwrap();
    let package = PackageItems { items };

    println!("=== items.json ===");
    println!("{}", serde_json::to_string_pretty(&package).unwrap());
}
