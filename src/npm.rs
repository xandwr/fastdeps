//! npm/Node.js package discovery and parsing.
//!
//! Handles finding TypeScript/JavaScript packages and their source files.

use camino::{Utf8Path, Utf8PathBuf};
use serde::Deserialize;
use std::fs;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum NpmError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("package.json not found")]
    PackageJsonNotFound,
    #[error("node_modules not found")]
    NodeModulesNotFound,
}

/// A discovered npm package.
#[derive(Debug, Clone)]
pub struct NpmPackage {
    pub name: String,
    pub version: String,
    pub path: Utf8PathBuf,
    pub main: Option<String>,
    pub types: Option<String>,
}

impl NpmPackage {
    /// Get all TypeScript/JavaScript source files in this package.
    pub fn source_files(&self) -> Vec<Utf8PathBuf> {
        let mut files = Vec::new();

        // Check src/ directory first (most common)
        let src_dir = self.path.join("src");
        if src_dir.exists() && src_dir.is_dir() {
            collect_ts_files(&src_dir, &mut files);
        }

        // If no files in src/, try lib/ or root
        if files.is_empty() {
            let lib_dir = self.path.join("lib");
            if lib_dir.exists() && lib_dir.is_dir() {
                collect_ts_files(&lib_dir, &mut files);
            }
        }

        // If still empty, check root (but exclude src/lib/dist/node_modules)
        if files.is_empty() {
            collect_ts_files(&self.path, &mut files);
        }

        // If we have a types field, make sure to include those
        if let Some(types) = &self.types {
            let types_path = self.path.join(types);
            if types_path.exists() && !files.contains(&types_path) {
                files.push(types_path);
            }
        }

        files
    }

    /// Get the main entry point file.
    pub fn entry_point(&self) -> Option<Utf8PathBuf> {
        // Try types first (for TS definitions)
        if let Some(types) = &self.types {
            let path = self.path.join(types);
            if path.exists() {
                return Some(path);
            }
        }

        // Then try main
        if let Some(main) = &self.main {
            let path = self.path.join(main);
            if path.exists() {
                return Some(path);
            }
        }

        // Common defaults
        let defaults = [
            "src/index.ts",
            "src/index.tsx",
            "src/index.js",
            "lib/index.ts",
            "lib/index.js",
            "index.ts",
            "index.js",
        ];

        for default in defaults {
            let path = self.path.join(default);
            if path.exists() {
                return Some(path);
            }
        }

        None
    }
}

fn collect_ts_files(dir: &Utf8Path, files: &mut Vec<Utf8PathBuf>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = Utf8PathBuf::from_path_buf(entry.path())
                .unwrap_or_else(|p| Utf8PathBuf::from(p.to_string_lossy().to_string()));

            if path.is_dir() {
                let name = path.file_name().unwrap_or("");
                // Skip common non-source directories
                if ![
                    "node_modules",
                    ".git",
                    "dist",
                    "build",
                    "coverage",
                    "__tests__",
                    "test",
                    "tests",
                ]
                .contains(&name)
                {
                    collect_ts_files(&path, files);
                }
            } else if let Some(ext) = path.extension() {
                if ["ts", "tsx", "js", "jsx", "mts", "cts", "mjs", "cjs"].contains(&ext) {
                    // Skip test files and declaration files for now
                    let file_name = path.file_name().unwrap_or("");
                    if !file_name.ends_with(".test.ts")
                        && !file_name.ends_with(".spec.ts")
                        && !file_name.ends_with(".test.tsx")
                        && !file_name.ends_with(".spec.tsx")
                        && !file_name.ends_with(".d.ts")
                    {
                        files.push(path);
                    }
                }
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct PackageJson {
    name: Option<String>,
    version: Option<String>,
    main: Option<String>,
    types: Option<String>,
    typings: Option<String>,
    dependencies: Option<std::collections::BTreeMap<String, String>>,
    #[serde(rename = "devDependencies")]
    dev_dependencies: Option<std::collections::BTreeMap<String, String>>,
}

/// A locked dependency from package-lock.json.
#[derive(Debug, Clone)]
pub struct LockedDep {
    pub name: String,
    pub version: String,
}

/// Parse package.json to get project info.
pub fn parse_package_json(project_dir: &Utf8Path) -> Result<NpmPackage, NpmError> {
    let package_json_path = project_dir.join("package.json");
    if !package_json_path.exists() {
        return Err(NpmError::PackageJsonNotFound);
    }

    let content = fs::read_to_string(&package_json_path)?;
    let pkg: PackageJson = serde_json::from_str(&content)?;

    Ok(NpmPackage {
        name: pkg.name.unwrap_or_else(|| "unknown".to_string()),
        version: pkg.version.unwrap_or_else(|| "0.0.0".to_string()),
        path: project_dir.to_owned(),
        main: pkg.main,
        types: pkg.types.or(pkg.typings),
    })
}

/// Get all dependencies from a project's package.json.
pub fn get_project_deps(project_dir: &Utf8Path) -> Result<Vec<LockedDep>, NpmError> {
    let package_json_path = project_dir.join("package.json");
    if !package_json_path.exists() {
        return Err(NpmError::PackageJsonNotFound);
    }

    let content = fs::read_to_string(&package_json_path)?;
    let pkg: PackageJson = serde_json::from_str(&content)?;

    let mut deps = Vec::new();

    if let Some(dependencies) = pkg.dependencies {
        for (name, version) in dependencies {
            deps.push(LockedDep {
                name,
                version: version
                    .trim_start_matches('^')
                    .trim_start_matches('~')
                    .to_string(),
            });
        }
    }

    if let Some(dev_dependencies) = pkg.dev_dependencies {
        for (name, version) in dev_dependencies {
            deps.push(LockedDep {
                name,
                version: version
                    .trim_start_matches('^')
                    .trim_start_matches('~')
                    .to_string(),
            });
        }
    }

    Ok(deps)
}

/// Find a package in node_modules.
pub fn find_package(project_dir: &Utf8Path, name: &str) -> Result<NpmPackage, NpmError> {
    let node_modules = project_dir.join("node_modules");
    if !node_modules.exists() {
        return Err(NpmError::NodeModulesNotFound);
    }

    // Handle scoped packages (@scope/name)
    let package_path = if name.starts_with('@') {
        node_modules.join(name)
    } else {
        node_modules.join(name)
    };

    parse_package_json(&package_path)
}

/// List all packages in node_modules.
pub fn list_packages(project_dir: &Utf8Path) -> Result<Vec<NpmPackage>, NpmError> {
    let node_modules = project_dir.join("node_modules");
    if !node_modules.exists() {
        return Err(NpmError::NodeModulesNotFound);
    }

    let mut packages = Vec::new();

    if let Ok(entries) = fs::read_dir(&node_modules) {
        for entry in entries.flatten() {
            let path = Utf8PathBuf::from_path_buf(entry.path())
                .unwrap_or_else(|p| Utf8PathBuf::from(p.to_string_lossy().to_string()));

            if path.is_dir() {
                let name = path.file_name().unwrap_or("");

                if name.starts_with('@') {
                    // Scoped packages
                    if let Ok(scoped_entries) = fs::read_dir(&path) {
                        for scoped_entry in scoped_entries.flatten() {
                            let scoped_path = Utf8PathBuf::from_path_buf(scoped_entry.path())
                                .unwrap_or_else(|p| {
                                    Utf8PathBuf::from(p.to_string_lossy().to_string())
                                });

                            if scoped_path.is_dir() {
                                if let Ok(pkg) = parse_package_json(&scoped_path) {
                                    packages.push(pkg);
                                }
                            }
                        }
                    }
                } else if !name.starts_with('.') {
                    if let Ok(pkg) = parse_package_json(&path) {
                        packages.push(pkg);
                    }
                }
            }
        }
    }

    Ok(packages)
}

/// Convert a file path to a module path.
/// e.g., "src/license/api.ts" -> "package.license.api"
pub fn path_to_module(package_name: &str, path: &Utf8Path) -> String {
    let path_str = path.as_str();

    // Strip src/ or lib/ prefix
    let path_str = path_str
        .strip_prefix("src/")
        .or_else(|| path_str.strip_prefix("lib/"))
        .unwrap_or(path_str);

    // Strip extension
    let path_str = path_str
        .strip_suffix(".ts")
        .or_else(|| path_str.strip_suffix(".tsx"))
        .or_else(|| path_str.strip_suffix(".js"))
        .or_else(|| path_str.strip_suffix(".jsx"))
        .or_else(|| path_str.strip_suffix(".mts"))
        .or_else(|| path_str.strip_suffix(".cts"))
        .unwrap_or(path_str);

    // Handle index files -> parent module
    let path_str = path_str.strip_suffix("/index").unwrap_or(path_str);
    let path_str = if path_str == "index" {
        return package_name.to_string();
    } else {
        path_str
    };

    // Convert path separators to dots
    let module_part = path_str.replace('/', ".");

    format!("{}.{}", package_name, module_part)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_to_module() {
        assert_eq!(
            path_to_module("lesstokens", &Utf8PathBuf::from("src/index.ts")),
            "lesstokens"
        );
        assert_eq!(
            path_to_module("lesstokens", &Utf8PathBuf::from("src/license/api.ts")),
            "lesstokens.license.api"
        );
        assert_eq!(
            path_to_module("lesstokens", &Utf8PathBuf::from("src/license/index.ts")),
            "lesstokens.license"
        );
    }
}
