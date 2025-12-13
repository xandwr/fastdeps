//! MCP (Model Context Protocol) server for fastdeps.
//!
//! Provides smart search with fuzzy matching, pagination, and crate-aware results.

use crate::cache::Cache;
use crate::cargo::{RegistryCrate, resolve_project_deps};
use crate::languages::rust::RustParser;
use crate::schema::Item;
use crate::search::{CrateRelationship, SearchEngine, SearchOptions, SearchResponse};
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

    fn list_impl(&self, params: ListParams) -> Result<String, String> {
        let mut crates =
            resolve_project_deps(&Utf8PathBuf::from("."), false).map_err(|e| e.to_string())?;

        if let Some(ref f) = params.filter {
            crates.retain(|c| c.name.contains(f));
        }

        if params.latest.unwrap_or(false) {
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

        let total = crates.len();
        let offset = params.offset.unwrap_or(0);
        let limit = params.limit.unwrap_or(50);

        let paginated: Vec<_> = crates.iter().skip(offset).take(limit).collect();

        let mut output = String::new();
        for c in &paginated {
            output.push_str(&format!("{}@{}\n", c.name, c.version));
        }

        output.push_str(&format!(
            "\n{} crates (showing {}-{} of {})",
            paginated.len(),
            offset + 1,
            (offset + paginated.len()).min(total),
            total
        ));

        if offset + limit < total {
            output.push_str(&format!("\nUse offset={} for next page", offset + limit));
        }

        Ok(output)
    }

    fn deps_impl(&self, path: Option<String>) -> Result<String, String> {
        let project_dir = Utf8PathBuf::from(path.unwrap_or_else(|| ".".to_string()));
        let deps = resolve_project_deps(&project_dir, false).map_err(|e| e.to_string())?;

        let result: Vec<String> = deps
            .iter()
            .map(|d| format!("{}@{}", d.name, d.version))
            .collect();

        Ok(format!(
            "{}\n\n{} dependencies",
            result.join("\n"),
            result.len()
        ))
    }

    fn peek_impl(&self, params: PeekParams) -> Result<String, String> {
        let (crate_name, version) = parse_crate_spec(&params.name);

        // Use search engine for smart crate lookup
        let engine = SearchEngine::new(&Utf8PathBuf::from(".")).map_err(|e| e.to_string())?;
        let crate_info = engine.get_crate_info(crate_name)?;

        let mut output = String::new();

        // Header with crate info
        output.push_str(&format!("# {}@{}\n", crate_info.name, crate_info.version));

        if crate_info.is_direct_dep {
            output.push_str("(direct dependency)\n");
        } else {
            output.push_str("(transitive dependency)\n");
        }

        // Handle re-export crates specially
        if crate_info.is_reexport && crate_info.item_count == 0 {
            output.push_str("\nThis is a re-export crate with no direct API.\n");

            if !crate_info.related_crates.is_empty() {
                output.push_str("\nRelated crates:\n");
                for related in &crate_info.related_crates {
                    let marker = match related.relationship {
                        CrateRelationship::Direct => "→",
                        CrateRelationship::Prefix => "├",
                        CrateRelationship::ReExport => "↪",
                    };
                    output.push_str(&format!(
                        "  {} {}@{} ({} items)\n",
                        marker, related.name, related.version, related.item_count
                    ));
                }
                output.push_str("\nUse: peek <crate_name> to explore a specific crate\n");
            }

            return Ok(output);
        }

        // Try cache first
        if Cache::exists() {
            if let Ok(cache) = Cache::open_existing() {
                let items = cache
                    .search_crate(crate_name, version)
                    .map_err(|e| e.to_string())?;

                if !items.is_empty() {
                    let offset = params.offset.unwrap_or(0);
                    let limit = params.limit.unwrap_or(30);
                    let total = items.len();

                    // Apply kind filter
                    let filtered: Vec<_> = if let Some(ref kind) = params.kind {
                        items
                            .into_iter()
                            .filter(|i| i.kind.eq_ignore_ascii_case(kind))
                            .collect()
                    } else {
                        items
                    };

                    let paginated: Vec<_> = filtered.iter().skip(offset).take(limit).collect();

                    output.push_str(&format!("\n{} items total", total));
                    if params.kind.is_some() {
                        output.push_str(&format!(" ({} after filter)", filtered.len()));
                    }
                    output.push_str("\n\n");

                    if params.full.unwrap_or(false) {
                        output.push_str(
                            &serde_json::to_string_pretty(&paginated).unwrap_or_default(),
                        );
                    } else {
                        for item in &paginated {
                            if let Some(sig) = &item.signature {
                                // Truncate long signatures
                                let sig_short = if sig.len() > 80 {
                                    format!("{}...", &sig[..77])
                                } else {
                                    sig.clone()
                                };
                                output.push_str(&format!(
                                    "{} ({}) - {}\n",
                                    item.path, item.kind, sig_short
                                ));
                            } else {
                                output.push_str(&format!("{} ({})\n", item.path, item.kind));
                            }
                        }
                    }

                    let shown = paginated.len();
                    if offset + limit < filtered.len() {
                        output.push_str(&format!(
                            "\nShowing {}-{} of {}. Use offset={} for next page",
                            offset + 1,
                            offset + shown,
                            filtered.len(),
                            offset + limit
                        ));
                    }

                    // Show related crates if this might be a namespace crate
                    if !crate_info.related_crates.is_empty() && crate_info.related_crates.len() > 1
                    {
                        output.push_str("\n\nRelated crates: ");
                        let names: Vec<_> = crate_info
                            .related_crates
                            .iter()
                            .filter(|r| r.name != crate_name)
                            .take(5)
                            .map(|r| r.name.as_str())
                            .collect();
                        output.push_str(&names.join(", "));
                    }

                    return Ok(output);
                }
            }
        }

        // Fall back to parsing
        let krate = find_specific_crate(crate_name, version)?;
        output.push_str("\n(parsed fresh - not cached)\n\n");

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

        let offset = params.offset.unwrap_or(0);
        let limit = params.limit.unwrap_or(30);

        let paginated: Vec<_> = all_items.iter().skip(offset).take(limit).collect();

        if params.full.unwrap_or(false) {
            output.push_str(&serde_json::to_string_pretty(&paginated).unwrap_or_default());
        } else {
            for item in &paginated {
                let kind = format!("{:?}", item.kind).to_lowercase();
                if let Some(sig) = &item.signature {
                    let sig_short = if sig.len() > 80 {
                        format!("{}...", &sig[..77])
                    } else {
                        sig.clone()
                    };
                    output.push_str(&format!("{} ({}) - {}\n", item.path, kind, sig_short));
                } else {
                    output.push_str(&format!("{} ({})\n", item.path, kind));
                }
            }
        }

        output.push_str(&format!("\n{} items total", all_items.len()));

        Ok(output)
    }

    fn find_impl(&self, params: FindParams) -> Result<String, String> {
        let engine = SearchEngine::new(&Utf8PathBuf::from(".")).map_err(|e| e.to_string())?;

        let mut options = SearchOptions::new()
            .with_limit(params.limit.unwrap_or(25))
            .with_offset(params.offset.unwrap_or(0));

        if let Some(ref crate_name) = params.crate_filter {
            options = options.with_crate(crate_name);
        }

        if params.direct_only.unwrap_or(false) {
            options = options.direct_only();
        }

        options.kind_filter = params.kind.clone();
        options.fuzzy = params.fuzzy.unwrap_or(true);

        // Ensure cache exists
        if !Cache::exists() {
            let deps =
                resolve_project_deps(&Utf8PathBuf::from("."), false).map_err(|e| e.to_string())?;
            crate::cache::parallel_index(&deps, false).map_err(|e| e.to_string())?;
        }

        let response = engine.search(&params.query, &options)?;

        let mut output = String::new();

        // Show related crates for crate-level queries
        if !response.related_crates.is_empty() {
            output.push_str("Related crates:\n");
            for crate_info in response.related_crates.iter().take(5) {
                let marker = if crate_info.item_count > 0 {
                    "●"
                } else {
                    "○"
                };
                output.push_str(&format!(
                    "  {} {}@{} ({} items, {})\n",
                    marker,
                    crate_info.name,
                    crate_info.version,
                    crate_info.item_count,
                    crate_info.relationship
                ));
            }
            output.push('\n');
        }

        // Show results
        if response.results.is_empty() {
            output.push_str(&format!("No results for '{}'\n", params.query));

            if !response.suggestions.is_empty() {
                output.push_str("\nDid you mean?\n");
                for suggestion in &response.suggestions {
                    output.push_str(&format!("  - {}\n", suggestion));
                }
            }
        } else {
            output.push_str(&format!(
                "Results for '{}' (showing {}-{} of {}):\n\n",
                params.query,
                response.pagination.offset + 1,
                response.pagination.offset + response.results.len(),
                response.pagination.total
            ));

            for result in &response.results {
                let direct_marker = if result.is_direct_dep { "●" } else { "○" };
                let score_info = if params.show_scores.unwrap_or(false) {
                    format!(" [{}:{}]", result.match_type, result.score)
                } else {
                    String::new()
                };

                output.push_str(&format!(
                    "{} {}@{}: {} ({}){}\n",
                    direct_marker,
                    result.crate_name,
                    result.crate_version,
                    result.path,
                    result.kind,
                    score_info
                ));
            }

            if response.pagination.has_more() {
                output.push_str(&format!(
                    "\nUse offset={} for next page",
                    response.pagination.next_offset()
                ));
            }
        }

        // Show filter hints
        let hint_parts: Vec<String> = [
            params.crate_filter.as_ref().map(|c| format!("crate={}", c)),
            params.kind.as_ref().map(|k| format!("kind={}", k)),
            if params.direct_only.unwrap_or(false) {
                Some("direct_only=true".to_string())
            } else {
                None
            },
        ]
        .into_iter()
        .flatten()
        .collect();

        if !hint_parts.is_empty() {
            output.push_str(&format!("\nFilters: {}", hint_parts.join(", ")));
        }

        Ok(output)
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

    fn expand_impl(&self, params: ExpandParams) -> Result<String, String> {
        let engine = SearchEngine::new(&Utf8PathBuf::from(".")).map_err(|e| e.to_string())?;
        let crate_info = engine.get_crate_info(&params.name)?;

        let mut output = String::new();

        output.push_str(&format!("# {}@{}\n\n", crate_info.name, crate_info.version));

        if crate_info.is_reexport {
            output.push_str("This is a re-export/umbrella crate.\n\n");
        }

        output.push_str(&format!("Items: {}\n", crate_info.item_count));
        output.push_str(&format!("Path: {}\n", crate_info.path));
        output.push_str(&format!(
            "Dependency type: {}\n\n",
            if crate_info.is_direct_dep {
                "direct"
            } else {
                "transitive"
            }
        ));

        if !crate_info.related_crates.is_empty() {
            output.push_str("Related crates:\n");
            for related in &crate_info.related_crates {
                let marker = match related.relationship {
                    CrateRelationship::Direct => "  →",
                    CrateRelationship::Prefix => "  ├",
                    CrateRelationship::ReExport => "  ↪",
                };
                output.push_str(&format!(
                    "{} {}@{} ({} items)\n",
                    marker, related.name, related.version, related.item_count
                ));
            }
        }

        Ok(output)
    }
}

// === Parameter structs ===

#[derive(Debug, Deserialize, JsonSchema)]
struct ListParams {
    /// Filter by crate name (substring match)
    #[schemars(description = "Filter crates by name (substring match)")]
    filter: Option<String>,
    /// Show only the latest version of each crate
    #[schemars(description = "Show only latest version of each crate")]
    latest: Option<bool>,
    /// Maximum results to return (default: 50)
    #[schemars(description = "Maximum results to return")]
    limit: Option<usize>,
    /// Offset for pagination (default: 0)
    #[schemars(description = "Offset for pagination")]
    offset: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct DepsParams {
    /// Path to project directory (defaults to current dir)
    path: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PeekParams {
    /// Crate name (e.g., "serde" or "serde@1.0.200")
    #[schemars(
        description = "Crate name, optionally with version (e.g., 'serde' or 'serde@1.0.200')"
    )]
    name: String,
    /// Show full details including methods and fields as JSON
    #[schemars(description = "Return full JSON with all details")]
    full: Option<bool>,
    /// Maximum items to return (default: 30)
    #[schemars(description = "Maximum items to return")]
    limit: Option<usize>,
    /// Offset for pagination
    #[schemars(description = "Offset for pagination")]
    offset: Option<usize>,
    /// Filter by item kind (struct, trait, function, enum, etc.)
    #[schemars(
        description = "Filter by item kind: struct, trait, function, enum, macro, constant, module"
    )]
    kind: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct FindParams {
    /// Symbol to search for (e.g., "Serialize", "spawn", "Component")
    #[schemars(description = "Symbol name to search for. Supports fuzzy matching.")]
    query: String,
    /// Filter to a specific crate
    #[schemars(description = "Filter results to a specific crate")]
    crate_filter: Option<String>,
    /// Maximum results to return (default: 25)
    #[schemars(description = "Maximum results to return")]
    limit: Option<usize>,
    /// Offset for pagination
    #[schemars(description = "Offset for pagination")]
    offset: Option<usize>,
    /// Only show results from direct dependencies
    #[schemars(description = "Only show direct dependencies (not transitive)")]
    direct_only: Option<bool>,
    /// Filter by item kind
    #[schemars(description = "Filter by kind: struct, trait, function, enum, etc.")]
    kind: Option<String>,
    /// Enable fuzzy matching (default: true)
    #[schemars(description = "Enable fuzzy/typo-tolerant matching")]
    fuzzy: Option<bool>,
    /// Show match scores in output
    #[schemars(description = "Show relevance scores in output")]
    show_scores: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WhereParams {
    /// Crate name (e.g., "serde" or "serde@1.0.200")
    name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ExpandParams {
    /// Crate name to expand (e.g., "bevy" shows bevy_* crates)
    #[schemars(description = "Crate name to expand and show related crates")]
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
                "Fastdeps: Smart Rust dependency explorer with fuzzy search.\n\n\
                 Tools:\n\
                 - list: List project dependencies (with pagination)\n\
                 - deps: Show Cargo.lock dependencies\n\
                 - peek: View a crate's API (structs, traits, functions)\n\
                 - find: Search symbols with fuzzy matching and scoring\n\
                 - expand: Show related crates (e.g., bevy → bevy_ecs, bevy_app)\n\
                 - where: Locate crate source on disk\n\n\
                 Tips:\n\
                 - Use crate_filter to narrow search to one crate\n\
                 - Use kind filter for struct/trait/function/etc.\n\
                 - ● = direct dependency, ○ = transitive\n\
                 - Pagination: use limit/offset to navigate large results"
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
                        "List project dependencies",
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
                        "Search for a symbol across project dependencies",
                        cached_schema_for_type::<FindParams>(),
                    ),
                    Tool::new(
                        "expand",
                        "Expand a crate to show related crates (e.g., bevy → bevy_ecs)",
                        cached_schema_for_type::<ExpandParams>(),
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
                            limit: None,
                            offset: None,
                        });

                    match this.list_impl(params) {
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

                    match this.peek_impl(params) {
                        Ok(output) => Ok(CallToolResult::success(vec![Content::text(output)])),
                        Err(e) => Ok(CallToolResult::error(vec![Content::text(e)])),
                    }
                }
                "find" => {
                    let params: FindParams = serde_json::from_value(args_value).map_err(|e| {
                        McpError::invalid_params(format!("Invalid parameters: {}", e), None)
                    })?;

                    match this.find_impl(params) {
                        Ok(output) => Ok(CallToolResult::success(vec![Content::text(output)])),
                        Err(e) => Ok(CallToolResult::error(vec![Content::text(e)])),
                    }
                }
                "expand" => {
                    let params: ExpandParams = serde_json::from_value(args_value).map_err(|e| {
                        McpError::invalid_params(format!("Invalid parameters: {}", e), None)
                    })?;

                    match this.expand_impl(params) {
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

fn find_specific_crate(name: &str, version: Option<&str>) -> Result<RegistryCrate, String> {
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
