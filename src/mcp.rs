//! MCP (Model Context Protocol) server for fastdeps.

use crate::cache::Cache;
use crate::cargo::{list_registry_crates, resolve_project_deps, RegistryCrate};
use crate::languages::rust::RustParser;
use crate::schema::Item;
use camino::Utf8PathBuf;
use rmcp::handler::server::tool::cached_schema_for_type;
use rmcp::model::{
    CallToolRequestParam, CallToolResult, Content, Implementation, ListToolsResult,
    PaginatedRequestParam, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::schemars;
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::Deserialize;
use std::fs;
use std::sync::Arc;

pub fn cmd_mcp() -> i32 {
    let rt = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");
    rt.block_on(run_mcp_server())
}

async fn run_mcp_server() -> i32 {
    let service = FastdepsService::new();
    let transport = rmcp::transport::io::stdio();

    let running_service = match service.serve(transport).await {
        Ok(service) => service,
        Err(e) => {
            eprintln!("MCP server error: {}", e);
            return 1;
        }
    };

    if let Err(e) = running_service.waiting().await {
        eprintln!("MCP server task error: {}", e);
        return 1;
    }

    0
}

#[derive(Clone)]
struct FastdepsService {
    _marker: Arc<()>,
}

impl FastdepsService {
    fn new() -> Self {
        Self {
            _marker: Arc::new(()),
        }
    }

    fn list_impl(&self, filter: Option<String>, latest: bool) -> Result<String, String> {
        let mut crates = list_registry_crates().map_err(|e| e.to_string())?;

        if let Some(ref f) = filter {
            crates.retain(|c| c.name.contains(f));
        }

        if latest {
            let mut latest_map: std::collections::BTreeMap<String, RegistryCrate> =
                std::collections::BTreeMap::new();
            for krate in crates {
                latest_map
                    .entry(krate.name.clone())
                    .and_modify(|existing| {
                        if version_cmp(&krate.version, &existing.version)
                            == std::cmp::Ordering::Greater
                        {
                            *existing = krate.clone();
                        }
                    })
                    .or_insert(krate);
            }
            crates = latest_map.into_values().collect();
        }

        let result: Vec<String> = crates
            .iter()
            .map(|c| format!("{}@{}", c.name, c.version))
            .collect();
        Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
    }

    fn deps_impl(&self, path: Option<String>) -> Result<String, String> {
        let project_dir = Utf8PathBuf::from(path.unwrap_or_else(|| ".".to_string()));
        let deps = resolve_project_deps(&project_dir).map_err(|e| e.to_string())?;

        let result: Vec<String> = deps
            .iter()
            .map(|d| format!("{}@{}", d.name, d.version))
            .collect();
        Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
    }

    fn peek_impl(&self, name: String, full: bool) -> Result<String, String> {
        let (crate_name, version) = parse_crate_spec(&name);

        // Try cache first
        if Cache::exists() {
            if let Ok(cache) = Cache::open_existing() {
                let items = cache
                    .search_crate(crate_name, version)
                    .map_err(|e| e.to_string())?;
                if !items.is_empty() {
                    if full {
                        return Ok(serde_json::to_string_pretty(&items).unwrap_or_default());
                    } else {
                        let compact: Vec<String> = items
                            .iter()
                            .map(|item| {
                                if let Some(sig) = &item.signature {
                                    format!("{} ({}) - {}", item.path, item.kind, sig)
                                } else {
                                    format!("{} ({})", item.path, item.kind)
                                }
                            })
                            .collect();
                        return Ok(compact.join("\n"));
                    }
                }
            }
        }

        // Fall back to parsing
        let krate = find_specific_crate(crate_name, version)?;
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

        all_items.sort_by(|a, b| a.path.cmp(&b.path));

        if full {
            Ok(serde_json::to_string_pretty(&all_items).unwrap_or_default())
        } else {
            let compact: Vec<String> = all_items
                .iter()
                .map(|item| {
                    let kind = format!("{:?}", item.kind).to_lowercase();
                    if let Some(sig) = &item.signature {
                        format!("{} ({}) - {}", item.path, kind, sig)
                    } else {
                        format!("{} ({})", item.path, kind)
                    }
                })
                .collect();
            Ok(compact.join("\n"))
        }
    }

    fn find_impl(&self, query: String, project: bool) -> Result<String, String> {
        // Try cache first
        if Cache::exists() {
            if let Ok(cache) = Cache::open_existing() {
                let results = cache.search(&query).map_err(|e| e.to_string())?;
                if !results.is_empty() {
                    let results = if project {
                        let deps = resolve_project_deps(&Utf8PathBuf::from("."))
                            .map_err(|e| e.to_string())?;
                        let dep_set: std::collections::HashSet<_> = deps
                            .iter()
                            .map(|d| (d.name.as_str(), d.version.as_str()))
                            .collect();
                        results
                            .into_iter()
                            .filter(|r| {
                                dep_set
                                    .contains(&(r.crate_name.as_str(), r.crate_version.as_str()))
                            })
                            .collect()
                    } else {
                        results
                    };

                    let formatted: Vec<String> = results
                        .iter()
                        .map(|r| {
                            format!(
                                "{}@{}: {} ({})",
                                r.crate_name, r.crate_version, r.path, r.kind
                            )
                        })
                        .collect();
                    return Ok(formatted.join("\n"));
                }
            }
        }

        // Fall back to parsing
        let crates = if project {
            resolve_project_deps(&Utf8PathBuf::from(".")).map_err(|e| e.to_string())?
        } else {
            list_registry_crates().map_err(|e| e.to_string())?
        };

        let query_lower = query.to_lowercase();
        let mut parser = RustParser::new().map_err(|e| e.to_string())?;
        let mut found: Vec<String> = Vec::new();

        for krate in crates {
            for source_file in krate.source_files() {
                let relative = source_file
                    .strip_prefix(&krate.path)
                    .unwrap_or(&source_file);
                let module_path = crate::path_to_module(&krate.name, relative);

                if let Ok(source) = fs::read_to_string(&source_file) {
                    if let Ok(items) = parser.parse_source(&source, &module_path) {
                        for item in items {
                            if item.path.to_lowercase().contains(&query_lower) {
                                let kind = format!("{:?}", item.kind).to_lowercase();
                                found.push(format!(
                                    "{}@{}: {} ({})",
                                    krate.name, krate.version, item.path, kind
                                ));
                            }
                        }
                    }
                }
            }
        }

        Ok(found.join("\n"))
    }

    fn where_impl(&self, name: String) -> Result<String, String> {
        let (crate_name, version) = parse_crate_spec(&name);
        let krate = find_specific_crate(crate_name, version)?;

        let mut result = krate.path.to_string();
        if let Some(lib) = krate.lib_path() {
            result.push_str(&format!("\nEntry point: {}", lib));
        }
        Ok(result)
    }
}

// === Parameter structs ===

#[derive(Debug, Deserialize, JsonSchema)]
struct ListParams {
    /// Filter by crate name (substring match)
    filter: Option<String>,
    /// Show only the latest version of each crate
    latest: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct DepsParams {
    /// Path to project directory (defaults to current dir)
    path: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PeekParams {
    /// Crate name (e.g., "serde" or "serde@1.0.200")
    name: String,
    /// Show full details including methods and fields as JSON
    full: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct FindParams {
    /// Symbol to search for (e.g., "Serialize", "spawn")
    query: String,
    /// Only search in project dependencies (requires Cargo.lock)
    project: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WhereParams {
    /// Crate name (e.g., "serde" or "serde@1.0.200")
    name: String,
}

impl ServerHandler for FastdepsService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: rmcp::model::ProtocolVersion::LATEST,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: "fastdeps".to_string(),
                title: Some("Fastdeps".to_string()),
                version: env!("CARGO_PKG_VERSION").to_string(),
                icons: None,
                website_url: None,
            },
            instructions: Some(
                "Fastdeps provides tools to explore Rust dependency source code. \
                 Use 'list' to see available crates, 'deps' for project dependencies, \
                 'peek' to view a crate's API surface, 'find' to search for symbols, \
                 and 'where' to locate crate source files."
                    .to_string(),
            ),
        }
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, McpError>> + Send {
        async move {
            Ok(ListToolsResult {
                tools: vec![
                    Tool::new(
                        "list",
                        "List all crates in your cargo registry",
                        cached_schema_for_type::<ListParams>(),
                    ),
                    Tool::new(
                        "deps",
                        "List dependencies of a project from its Cargo.lock",
                        cached_schema_for_type::<DepsParams>(),
                    ),
                    Tool::new(
                        "peek",
                        "View a crate's API surface (structs, functions, traits, etc.)",
                        cached_schema_for_type::<PeekParams>(),
                    ),
                    Tool::new(
                        "find",
                        "Search for a symbol across dependencies",
                        cached_schema_for_type::<FindParams>(),
                    ),
                    Tool::new(
                        "where",
                        "Show the source path for a crate on disk",
                        cached_schema_for_type::<WhereParams>(),
                    ),
                ],
                next_cursor: None,
            })
        }
    }

    fn call_tool(
        &self,
        request: CallToolRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<CallToolResult, McpError>> + Send {
        let this = self.clone();
        async move {
            let args_value = request
                .arguments
                .map(serde_json::Value::Object)
                .unwrap_or(serde_json::Value::Null);

            match request.name.as_ref() {
                "list" => {
                    let params: ListParams =
                        serde_json::from_value(args_value).unwrap_or(ListParams {
                            filter: None,
                            latest: None,
                        });

                    match this.list_impl(params.filter, params.latest.unwrap_or(false)) {
                        Ok(output) => Ok(CallToolResult::success(vec![Content::text(output)])),
                        Err(e) => Ok(CallToolResult::error(vec![Content::text(e)])),
                    }
                }
                "deps" => {
                    let params: DepsParams =
                        serde_json::from_value(args_value).unwrap_or(DepsParams { path: None });

                    match this.deps_impl(params.path) {
                        Ok(output) => Ok(CallToolResult::success(vec![Content::text(output)])),
                        Err(e) => Ok(CallToolResult::error(vec![Content::text(e)])),
                    }
                }
                "peek" => {
                    let params: PeekParams = serde_json::from_value(args_value).map_err(|e| {
                        McpError::invalid_params(format!("Invalid parameters: {}", e), None)
                    })?;

                    match this.peek_impl(params.name, params.full.unwrap_or(false)) {
                        Ok(output) => Ok(CallToolResult::success(vec![Content::text(output)])),
                        Err(e) => Ok(CallToolResult::error(vec![Content::text(e)])),
                    }
                }
                "find" => {
                    let params: FindParams = serde_json::from_value(args_value).map_err(|e| {
                        McpError::invalid_params(format!("Invalid parameters: {}", e), None)
                    })?;

                    match this.find_impl(params.query, params.project.unwrap_or(false)) {
                        Ok(output) => Ok(CallToolResult::success(vec![Content::text(output)])),
                        Err(e) => Ok(CallToolResult::error(vec![Content::text(e)])),
                    }
                }
                "where" => {
                    let params: WhereParams = serde_json::from_value(args_value).map_err(|e| {
                        McpError::invalid_params(format!("Invalid parameters: {}", e), None)
                    })?;

                    match this.where_impl(params.name) {
                        Ok(output) => Ok(CallToolResult::success(vec![Content::text(output)])),
                        Err(e) => Ok(CallToolResult::error(vec![Content::text(e)])),
                    }
                }
                _ => Err(McpError::invalid_params(
                    format!("Unknown tool: {}", request.name),
                    None,
                )),
            }
        }
    }
}

// === Helpers ===

fn parse_crate_spec(spec: &str) -> (&str, Option<&str>) {
    if let Some((name, version)) = spec.split_once('@') {
        (name, Some(version))
    } else {
        (spec, None)
    }
}

fn find_specific_crate(
    name: &str,
    version: Option<&str>,
) -> Result<RegistryCrate, String> {
    use crate::cargo::find_crate;

    let crates = find_crate(name).map_err(|e| e.to_string())?;

    if crates.is_empty() {
        return Err(format!("Crate '{}' not found in registry", name));
    }

    if let Some(v) = version {
        crates
            .into_iter()
            .find(|c| c.version == v)
            .ok_or_else(|| format!("Version {} of '{}' not found", v, name))
    } else {
        crates
            .into_iter()
            .max_by(|a, b| version_cmp(&a.version, &b.version))
            .ok_or_else(|| format!("No versions found for '{}'", name))
    }
}

fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let parse = |v: &str| -> Vec<u64> {
        v.split(|c: char| !c.is_ascii_digit())
            .filter_map(|s| s.parse().ok())
            .collect()
    };

    let a_parts = parse(a);
    let b_parts = parse(b);
    a_parts.cmp(&b_parts)
}
