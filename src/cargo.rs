//! Cargo registry and project dependency walking.

use camino::{Utf8Path, Utf8PathBuf};
use serde::Deserialize;
use std::fs;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CargoError {
    #[error("Home directory not found")]
    NoHomeDir,
    #[error("Cargo registry not found at {0}")]
    NoRegistry(Utf8PathBuf),
    #[error("Failed to read {path}: {source}")]
    ReadError {
        path: Utf8PathBuf,
        source: std::io::Error,
    },
    #[error("Failed to parse TOML in {path}: {source}")]
    TomlError {
        path: Utf8PathBuf,
        source: toml::de::Error,
    },
    #[error("Cargo.lock not found at {0}")]
    NoLockFile(Utf8PathBuf),
}

/// A discovered crate in the cargo registry.
#[derive(Debug, Clone)]
pub struct RegistryCrate {
    pub name: String,
    pub version: String,
    pub path: Utf8PathBuf,
}

impl RegistryCrate {
    /// Get the src/lib.rs or src/main.rs path if it exists.
    pub fn lib_path(&self) -> Option<Utf8PathBuf> {
        let lib_rs = self.path.join("src/lib.rs");
        if lib_rs.exists() {
            return Some(lib_rs);
        }
        let main_rs = self.path.join("src/main.rs");
        if main_rs.exists() {
            return Some(main_rs);
        }
        None
    }

    /// Get all .rs files in the crate's src directory.
    pub fn source_files(&self) -> Vec<Utf8PathBuf> {
        let src_dir = self.path.join("src");
        if !src_dir.exists() {
            return vec![];
        }
        collect_rs_files(&src_dir)
    }
}

fn collect_rs_files(dir: &Utf8Path) -> Vec<Utf8PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if let Ok(utf8_path) = Utf8PathBuf::try_from(path.clone()) {
                if path.is_dir() {
                    files.extend(collect_rs_files(&utf8_path));
                } else if path.extension().is_some_and(|e| e == "rs") {
                    files.push(utf8_path);
                }
            }
        }
    }
    files
}

/// Finds the cargo registry source directory.
pub fn find_registry_src() -> Result<Utf8PathBuf, CargoError> {
    let home = home::home_dir().ok_or(CargoError::NoHomeDir)?;
    let home = Utf8PathBuf::try_from(home).map_err(|_| CargoError::NoHomeDir)?;

    let registry_src = home.join(".cargo/registry/src");
    if !registry_src.exists() {
        return Err(CargoError::NoRegistry(registry_src));
    }

    Ok(registry_src)
}

/// Lists all crates in the cargo registry.
pub fn list_registry_crates() -> Result<Vec<RegistryCrate>, CargoError> {
    let registry_src = find_registry_src()?;
    let mut crates = Vec::new();

    // Registry src contains subdirs like "index.crates.io-6f17d22bba15001f"
    if let Ok(registries) = fs::read_dir(&registry_src) {
        for registry in registries.filter_map(Result::ok) {
            let registry_path = registry.path();
            if !registry_path.is_dir() {
                continue;
            }

            if let Ok(packages) = fs::read_dir(&registry_path) {
                for package in packages.filter_map(Result::ok) {
                    let package_path = package.path();
                    if !package_path.is_dir() {
                        continue;
                    }

                    if let Some(krate) = parse_crate_dir(&package_path) {
                        crates.push(krate);
                    }
                }
            }
        }
    }

    crates.sort_by(|a, b| (&a.name, &a.version).cmp(&(&b.name, &b.version)));
    Ok(crates)
}

/// Parse a crate directory name like "serde-1.0.200" into a RegistryCrate.
fn parse_crate_dir(path: &std::path::Path) -> Option<RegistryCrate> {
    let utf8_path = Utf8PathBuf::try_from(path.to_path_buf()).ok()?;
    let dir_name = utf8_path.file_name()?;

    // Find the last hyphen followed by a version number
    let mut last_hyphen = None;
    for (i, c) in dir_name.char_indices() {
        if c == '-' {
            // Check if what follows looks like a version (starts with digit)
            if dir_name[i + 1..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit())
            {
                last_hyphen = Some(i);
            }
        }
    }

    let hyphen_pos = last_hyphen?;
    let name = &dir_name[..hyphen_pos];
    let version = &dir_name[hyphen_pos + 1..];

    Some(RegistryCrate {
        name: name.to_string(),
        version: version.to_string(),
        path: utf8_path,
    })
}

/// Find a specific crate by name (returns all versions).
pub fn find_crate(name: &str) -> Result<Vec<RegistryCrate>, CargoError> {
    let all = list_registry_crates()?;
    Ok(all.into_iter().filter(|c| c.name == name).collect())
}

/// Find a specific crate by name and version.
pub fn find_crate_version(name: &str, version: &str) -> Result<Option<RegistryCrate>, CargoError> {
    let all = list_registry_crates()?;
    Ok(all
        .into_iter()
        .find(|c| c.name == name && c.version == version))
}

// === Cargo.lock parsing ===

#[derive(Debug, Deserialize)]
struct CargoLock {
    package: Option<Vec<LockPackage>>,
}

#[derive(Debug, Deserialize)]
struct LockPackage {
    name: String,
    version: String,
    source: Option<String>,
}

/// Locked dependency from Cargo.lock.
#[derive(Debug, Clone)]
pub struct LockedDep {
    pub name: String,
    pub version: String,
}

/// Parse Cargo.lock to get exact dependency versions.
pub fn parse_cargo_lock(project_dir: &Utf8Path) -> Result<Vec<LockedDep>, CargoError> {
    let lock_path = project_dir.join("Cargo.lock");
    if !lock_path.exists() {
        return Err(CargoError::NoLockFile(lock_path));
    }

    let contents = fs::read_to_string(&lock_path).map_err(|e| CargoError::ReadError {
        path: lock_path.clone(),
        source: e,
    })?;

    let lock: CargoLock = toml::from_str(&contents).map_err(|e| CargoError::TomlError {
        path: lock_path,
        source: e,
    })?;

    let deps = lock
        .package
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.source.is_some()) // Only external deps have a source
        .map(|p| LockedDep {
            name: p.name,
            version: p.version,
        })
        .collect();

    Ok(deps)
}

/// Get all dependencies for a project with their registry paths.
pub fn resolve_project_deps(project_dir: &Utf8Path) -> Result<Vec<RegistryCrate>, CargoError> {
    let locked = parse_cargo_lock(project_dir)?;
    let registry = list_registry_crates()?;

    let mut resolved = Vec::new();
    for dep in locked {
        if let Some(krate) = registry
            .iter()
            .find(|c| c.name == dep.name && c.version == dep.version)
        {
            resolved.push(krate.clone());
        }
    }

    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_crate_dir() {
        let path = std::path::Path::new("/home/user/.cargo/registry/src/index/serde-1.0.200");
        let krate = parse_crate_dir(path).unwrap();
        assert_eq!(krate.name, "serde");
        assert_eq!(krate.version, "1.0.200");
    }

    #[test]
    fn test_parse_crate_dir_with_hyphen() {
        let path = std::path::Path::new("/home/user/.cargo/registry/src/index/proc-macro2-1.0.86");
        let krate = parse_crate_dir(path).unwrap();
        assert_eq!(krate.name, "proc-macro2");
        assert_eq!(krate.version, "1.0.86");
    }
}
