//! Rust project discovery and dependency resolution.

use std::path::{Path, PathBuf};

/// A resolved dependency from Cargo.lock
#[derive(Debug, Clone)]
pub struct Dependency {
    pub name: String,
    pub version: String,
}

/// A discovered Rust project
#[derive(Debug)]
pub struct RustProject {
    pub root: PathBuf,
    pub name: String,
    pub deps: Vec<Dependency>,
}

impl RustProject {
    /// Discover a Rust project from the current directory (or parent dirs)
    pub fn discover() -> Result<Self, String> {
        let cwd = std::env::current_dir().map_err(|e| format!("can't get cwd: {e}"))?;
        Self::discover_from(&cwd)
    }

    /// Discover a Rust project starting from a given path
    pub fn discover_from(start: &Path) -> Result<Self, String> {
        let mut dir = start.to_path_buf();

        loop {
            let cargo_toml = dir.join("Cargo.toml");
            if cargo_toml.exists() {
                return Self::load(&dir);
            }

            if !dir.pop() {
                return Err("not a Rust project (no Cargo.toml found)".to_string());
            }
        }
    }

    fn load(root: &Path) -> Result<Self, String> {
        let cargo_toml = root.join("Cargo.toml");
        let cargo_lock = root.join("Cargo.lock");

        // Parse Cargo.toml for project name
        let toml_content = std::fs::read_to_string(&cargo_toml)
            .map_err(|e| format!("can't read Cargo.toml: {e}"))?;

        let name = parse_package_name(&toml_content).unwrap_or_else(|| "unknown".to_string());

        // Parse Cargo.lock for dependencies
        let deps = if cargo_lock.exists() {
            let lock_content = std::fs::read_to_string(&cargo_lock)
                .map_err(|e| format!("can't read Cargo.lock: {e}"))?;
            parse_cargo_lock(&lock_content)
        } else {
            eprintln!("warning: no Cargo.lock found, run `cargo build` first");
            vec![]
        };

        Ok(Self {
            root: root.to_path_buf(),
            name,
            deps,
        })
    }
}

/// Parse package name from Cargo.toml (simple approach)
fn parse_package_name(content: &str) -> Option<String> {
    // Look for: name = "foo"
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with("name") {
            if let Some(value) = line.split('=').nth(1) {
                let value = value.trim().trim_matches('"');
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Parse dependencies from Cargo.lock
fn parse_cargo_lock(content: &str) -> Vec<Dependency> {
    let mut deps = Vec::new();
    let mut current_name: Option<String> = None;
    let mut current_version: Option<String> = None;

    for line in content.lines() {
        let line = line.trim();

        if line == "[[package]]" {
            // Save previous package if complete
            if let (Some(name), Some(version)) = (current_name.take(), current_version.take()) {
                deps.push(Dependency { name, version });
            }
        } else if let Some(name) = line.strip_prefix("name = ") {
            current_name = Some(name.trim_matches('"').to_string());
        } else if let Some(version) = line.strip_prefix("version = ") {
            current_version = Some(version.trim_matches('"').to_string());
        }
    }

    // Don't forget the last package
    if let (Some(name), Some(version)) = (current_name, current_version) {
        deps.push(Dependency { name, version });
    }

    deps
}
