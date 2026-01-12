//! Parse Rust crate source files to extract public API symbols.
//!
//! Locates crate sources in ~/.cargo/registry/src and extracts:
//! - Structs, enums, unions
//! - Traits and their methods
//! - Functions
//! - Type aliases
//! - Constants and statics
//! - Macros

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use syn::{FnArg, GenericParam, Item, ReturnType, Type, Visibility};

use crate::db::Symbol;

/// Extracted information about a crate's public API
#[derive(Debug)]
pub struct CrateApi {
    pub name: String,
    pub version: String,
    pub symbols: Vec<Symbol>,
    /// Doc comments extracted for richer embeddings
    pub docs: Vec<SymbolDoc>,
}

/// Symbol with its documentation for embedding
#[derive(Debug, Clone)]
pub struct SymbolDoc {
    pub path: String,
    pub kind: String,
    pub signature: Option<String>,
    pub doc: Option<String>,
}

impl SymbolDoc {
    /// Create text suitable for embedding - combines path, signature, and docs
    pub fn embedding_text(&self) -> String {
        let mut parts = vec![self.path.clone()];
        if let Some(sig) = &self.signature {
            parts.push(sig.clone());
        }
        if let Some(doc) = &self.doc {
            // Take first paragraph of docs
            let first_para = doc.split("\n\n").next().unwrap_or(doc);
            if !first_para.is_empty() {
                parts.push(first_para.to_string());
            }
        }
        parts.join(" ")
    }

    pub fn to_symbol(&self) -> Symbol {
        Symbol {
            path: self.path.clone(),
            kind: self.kind.clone(),
            signature: self.signature.clone(),
        }
    }
}

/// Find the source directory for a crate in the cargo registry
pub fn find_crate_source(name: &str, version: &str) -> Result<PathBuf> {
    let home = dirs::home_dir().context("No home directory")?;
    let registry_src = home.join(".cargo/registry/src");

    if !registry_src.exists() {
        bail!("Cargo registry not found at {}", registry_src.display());
    }

    // Find the registry index directory (varies by registry)
    // Usually something like: index.crates.io-6f17d22bba15001f
    for entry in std::fs::read_dir(&registry_src)? {
        let entry = entry?;
        let index_dir = entry.path();
        if !index_dir.is_dir() {
            continue;
        }

        // Look for crate-version directory
        let crate_dir = index_dir.join(format!("{}-{}", name, version));
        if crate_dir.exists() {
            return Ok(crate_dir);
        }
    }

    bail!(
        "Crate {}@{} not found in cargo registry. Try `cargo fetch` first.",
        name,
        version
    )
}

/// Parse a crate's source and extract its public API
pub fn parse_crate(name: &str, version: &str) -> Result<CrateApi> {
    let source_dir = find_crate_source(name, version)?;
    let mut docs = Vec::new();

    // Find the lib.rs or main entry point
    let lib_rs = source_dir.join("src/lib.rs");
    let main_entry = if lib_rs.exists() {
        lib_rs
    } else {
        // Some crates use lib.rs in root
        let root_lib = source_dir.join("lib.rs");
        if root_lib.exists() {
            root_lib
        } else {
            // Try src/main.rs for binary crates
            source_dir.join("src/main.rs")
        }
    };

    if main_entry.exists() {
        parse_file(&main_entry, name, &mut docs)?;
    }

    // Also parse any modules declared at top level
    let src_dir = source_dir.join("src");
    if src_dir.exists() {
        parse_directory(&src_dir, name, &mut docs)?;
    }

    let symbols = docs.iter().map(|d| d.to_symbol()).collect();

    Ok(CrateApi {
        name: name.to_string(),
        version: version.to_string(),
        symbols,
        docs,
    })
}

/// Parse all .rs files in a directory
fn parse_directory(dir: &Path, crate_name: &str, docs: &mut Vec<SymbolDoc>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_file() && path.extension().map(|e| e == "rs").unwrap_or(false) {
            // Skip lib.rs as we handle it specially
            if path.file_name().map(|n| n == "lib.rs").unwrap_or(false) {
                continue;
            }
            let _ = parse_file(&path, crate_name, docs);
        } else if path.is_dir() {
            // Recurse into subdirectories
            let _ = parse_directory(&path, crate_name, docs);
        }
    }
    Ok(())
}

/// Parse a single .rs file and extract public symbols
fn parse_file(path: &Path, crate_name: &str, docs: &mut Vec<SymbolDoc>) -> Result<()> {
    let content = std::fs::read_to_string(path)?;
    let syntax = syn::parse_file(&content).context("Failed to parse Rust file")?;

    // Derive module path from file path
    let module_path = file_to_module_path(path, crate_name);

    for item in &syntax.items {
        extract_item(item, &module_path, docs);
    }

    Ok(())
}

/// Convert a file path to a module path
fn file_to_module_path(path: &Path, crate_name: &str) -> String {
    let file_name = path.file_stem().unwrap_or_default().to_string_lossy();

    // lib.rs and main.rs are the crate root
    if file_name == "lib" || file_name == "main" {
        return crate_name.to_string();
    }

    // mod.rs uses parent directory name
    if file_name == "mod" {
        if let Some(parent) = path.parent() {
            let parent_name = parent.file_name().unwrap_or_default().to_string_lossy();
            if parent_name != "src" {
                return format!("{}::{}", crate_name, parent_name);
            }
        }
        return crate_name.to_string();
    }

    // Regular file.rs -> crate::file
    format!("{}::{}", crate_name, file_name)
}

/// Extract symbols from an item
fn extract_item(item: &Item, module_path: &str, docs: &mut Vec<SymbolDoc>) {
    match item {
        Item::Fn(f) if is_public(&f.vis) => {
            let name = f.sig.ident.to_string();
            let sig = format_fn_signature(&f.sig);
            let doc = extract_doc_attrs(&f.attrs);
            docs.push(SymbolDoc {
                path: format!("{}::{}", module_path, name),
                kind: "fn".to_string(),
                signature: Some(sig),
                doc,
            });
        }

        Item::Struct(s) if is_public(&s.vis) => {
            let name = s.ident.to_string();
            let doc = extract_doc_attrs(&s.attrs);
            let sig = format_struct_signature(s);
            docs.push(SymbolDoc {
                path: format!("{}::{}", module_path, name),
                kind: "struct".to_string(),
                signature: Some(sig),
                doc,
            });

            // Extract impl methods for this struct (if in same file)
            // This is handled by Item::Impl below
        }

        Item::Enum(e) if is_public(&e.vis) => {
            let name = e.ident.to_string();
            let doc = extract_doc_attrs(&e.attrs);
            let variants: Vec<String> = e.variants.iter().map(|v| v.ident.to_string()).collect();
            let sig = format!("enum {} {{ {} }}", name, variants.join(", "));
            docs.push(SymbolDoc {
                path: format!("{}::{}", module_path, name),
                kind: "enum".to_string(),
                signature: Some(sig),
                doc,
            });

            // Add each variant as a symbol too
            for variant in &e.variants {
                let variant_name = variant.ident.to_string();
                let variant_doc = extract_doc_attrs(&variant.attrs);
                docs.push(SymbolDoc {
                    path: format!("{}::{}::{}", module_path, name, variant_name),
                    kind: "variant".to_string(),
                    signature: None,
                    doc: variant_doc,
                });
            }
        }

        Item::Trait(t) if is_public(&t.vis) => {
            let name = t.ident.to_string();
            let doc = extract_doc_attrs(&t.attrs);
            let generics = format_generics(&t.generics);
            let sig = format!("trait {}{}", name, generics);
            docs.push(SymbolDoc {
                path: format!("{}::{}", module_path, name),
                kind: "trait".to_string(),
                signature: Some(sig),
                doc,
            });

            // Extract trait methods
            for item in &t.items {
                if let syn::TraitItem::Fn(method) = item {
                    let method_name = method.sig.ident.to_string();
                    let method_sig = format_fn_signature(&method.sig);
                    let method_doc = extract_doc_attrs(&method.attrs);
                    docs.push(SymbolDoc {
                        path: format!("{}::{}::{}", module_path, name, method_name),
                        kind: "trait_method".to_string(),
                        signature: Some(method_sig),
                        doc: method_doc,
                    });
                }
            }
        }

        Item::Type(t) if is_public(&t.vis) => {
            let name = t.ident.to_string();
            let doc = extract_doc_attrs(&t.attrs);
            let sig = format!("type {} = {}", name, type_to_string(&t.ty));
            docs.push(SymbolDoc {
                path: format!("{}::{}", module_path, name),
                kind: "type".to_string(),
                signature: Some(sig),
                doc,
            });
        }

        Item::Const(c) if is_public(&c.vis) => {
            let name = c.ident.to_string();
            let doc = extract_doc_attrs(&c.attrs);
            let sig = format!("const {}: {}", name, type_to_string(&c.ty));
            docs.push(SymbolDoc {
                path: format!("{}::{}", module_path, name),
                kind: "const".to_string(),
                signature: Some(sig),
                doc,
            });
        }

        Item::Static(s) if is_public(&s.vis) => {
            let name = s.ident.to_string();
            let doc = extract_doc_attrs(&s.attrs);
            let mutability = if matches!(s.mutability, syn::StaticMutability::Mut(_)) {
                "mut "
            } else {
                ""
            };
            let sig = format!("static {}{}: {}", mutability, name, type_to_string(&s.ty));
            docs.push(SymbolDoc {
                path: format!("{}::{}", module_path, name),
                kind: "static".to_string(),
                signature: Some(sig),
                doc,
            });
        }

        Item::Impl(i) => {
            // Extract public methods from impl blocks
            if let Type::Path(type_path) = &*i.self_ty {
                let type_name = type_path
                    .path
                    .segments
                    .last()
                    .map(|s| s.ident.to_string())
                    .unwrap_or_default();

                for impl_item in &i.items {
                    if let syn::ImplItem::Fn(method) = impl_item {
                        if is_public(&method.vis) {
                            let method_name = method.sig.ident.to_string();
                            let method_sig = format_fn_signature(&method.sig);
                            let method_doc = extract_doc_attrs(&method.attrs);
                            docs.push(SymbolDoc {
                                path: format!("{}::{}::{}", module_path, type_name, method_name),
                                kind: "method".to_string(),
                                signature: Some(method_sig),
                                doc: method_doc,
                            });
                        }
                    }
                }
            }
        }

        Item::Mod(m) if is_public(&m.vis) => {
            let mod_name = m.ident.to_string();
            let new_module_path = format!("{}::{}", module_path, mod_name);

            // If the module has inline content, parse it
            if let Some((_, items)) = &m.content {
                for item in items {
                    extract_item(item, &new_module_path, docs);
                }
            }

            // Add the module itself as a symbol
            let doc = extract_doc_attrs(&m.attrs);
            docs.push(SymbolDoc {
                path: new_module_path,
                kind: "mod".to_string(),
                signature: None,
                doc,
            });
        }

        Item::Macro(m) => {
            // macro_rules! macros
            if let Some(ident) = &m.ident {
                let name = ident.to_string();
                let doc = extract_doc_attrs(&m.attrs);
                docs.push(SymbolDoc {
                    path: format!("{}::{}!", module_path, name),
                    kind: "macro".to_string(),
                    signature: None,
                    doc,
                });
            }
        }

        _ => {}
    }
}

/// Check if visibility is public
fn is_public(vis: &Visibility) -> bool {
    matches!(vis, Visibility::Public(_))
}

/// Extract doc comments from attributes
fn extract_doc_attrs(attrs: &[syn::Attribute]) -> Option<String> {
    let docs: Vec<String> = attrs
        .iter()
        .filter_map(|attr| {
            if attr.path().is_ident("doc") {
                if let syn::Meta::NameValue(nv) = &attr.meta {
                    if let syn::Expr::Lit(syn::ExprLit {
                        lit: syn::Lit::Str(s),
                        ..
                    }) = &nv.value
                    {
                        return Some(s.value().trim().to_string());
                    }
                }
            }
            None
        })
        .collect();

    if docs.is_empty() {
        None
    } else {
        Some(docs.join("\n"))
    }
}

/// Format a function signature
fn format_fn_signature(sig: &syn::Signature) -> String {
    let mut parts = Vec::new();

    // Async
    if sig.asyncness.is_some() {
        parts.push("async ".to_string());
    }

    // Unsafe
    if sig.unsafety.is_some() {
        parts.push("unsafe ".to_string());
    }

    parts.push("fn ".to_string());
    parts.push(sig.ident.to_string());

    // Generics
    parts.push(format_generics(&sig.generics));

    // Parameters
    let params: Vec<String> = sig
        .inputs
        .iter()
        .map(|arg| match arg {
            FnArg::Receiver(r) => {
                let mut s = String::new();
                if r.reference.is_some() {
                    s.push('&');
                    if r.mutability.is_some() {
                        s.push_str("mut ");
                    }
                }
                s.push_str("self");
                s
            }
            FnArg::Typed(t) => {
                format!("{}: {}", pat_to_string(&t.pat), type_to_string(&t.ty))
            }
        })
        .collect();
    parts.push(format!("({})", params.join(", ")));

    // Return type
    if let ReturnType::Type(_, ty) = &sig.output {
        parts.push(format!(" -> {}", type_to_string(ty)));
    }

    parts.concat()
}

/// Format generics
fn format_generics(generics: &syn::Generics) -> String {
    if generics.params.is_empty() {
        return String::new();
    }

    let params: Vec<String> = generics
        .params
        .iter()
        .map(|p| match p {
            GenericParam::Type(t) => t.ident.to_string(),
            GenericParam::Lifetime(l) => format!("'{}", l.lifetime.ident),
            GenericParam::Const(c) => format!("const {}", c.ident),
        })
        .collect();

    format!("<{}>", params.join(", "))
}

/// Format a struct signature (showing fields for tuple/unit structs)
fn format_struct_signature(s: &syn::ItemStruct) -> String {
    let name = s.ident.to_string();
    let generics = format_generics(&s.generics);

    match &s.fields {
        syn::Fields::Unit => format!("struct {}{}", name, generics),
        syn::Fields::Unnamed(fields) => {
            let types: Vec<String> = fields
                .unnamed
                .iter()
                .map(|f| type_to_string(&f.ty))
                .collect();
            format!("struct {}{}({})", name, generics, types.join(", "))
        }
        syn::Fields::Named(fields) => {
            let field_count = fields.named.len();
            format!("struct {}{} {{ {} fields }}", name, generics, field_count)
        }
    }
}

/// Convert a pattern to string (simplified)
fn pat_to_string(pat: &syn::Pat) -> String {
    match pat {
        syn::Pat::Ident(i) => i.ident.to_string(),
        syn::Pat::Wild(_) => "_".to_string(),
        _ => "_".to_string(),
    }
}

/// Convert a type to string (simplified)
fn type_to_string(ty: &Type) -> String {
    match ty {
        Type::Path(p) => p
            .path
            .segments
            .iter()
            .map(|s| {
                let name = s.ident.to_string();
                match &s.arguments {
                    syn::PathArguments::None => name,
                    syn::PathArguments::AngleBracketed(args) => {
                        let args_str: Vec<String> = args
                            .args
                            .iter()
                            .map(|a| match a {
                                syn::GenericArgument::Type(t) => type_to_string(t),
                                syn::GenericArgument::Lifetime(l) => format!("'{}", l.ident),
                                _ => "_".to_string(),
                            })
                            .collect();
                        format!("{}<{}>", name, args_str.join(", "))
                    }
                    syn::PathArguments::Parenthesized(args) => {
                        let inputs: Vec<String> = args.inputs.iter().map(type_to_string).collect();
                        let output = match &args.output {
                            ReturnType::Default => String::new(),
                            ReturnType::Type(_, t) => format!(" -> {}", type_to_string(t)),
                        };
                        format!("{}({}){}", name, inputs.join(", "), output)
                    }
                }
            })
            .collect::<Vec<_>>()
            .join("::"),
        Type::Reference(r) => {
            let mut s = String::from("&");
            if let Some(lt) = &r.lifetime {
                s.push_str(&format!("'{} ", lt.ident));
            }
            if r.mutability.is_some() {
                s.push_str("mut ");
            }
            s.push_str(&type_to_string(&r.elem));
            s
        }
        Type::Slice(s) => format!("[{}]", type_to_string(&s.elem)),
        Type::Array(a) => format!("[{}; _]", type_to_string(&a.elem)),
        Type::Tuple(t) => {
            let types: Vec<String> = t.elems.iter().map(type_to_string).collect();
            format!("({})", types.join(", "))
        }
        Type::Ptr(p) => {
            let mutability = if p.mutability.is_some() {
                "mut "
            } else {
                "const "
            };
            format!("*{}{}", mutability, type_to_string(&p.elem))
        }
        Type::ImplTrait(i) => {
            let bounds: Vec<String> = i
                .bounds
                .iter()
                .map(|b| match b {
                    syn::TypeParamBound::Trait(t) => t
                        .path
                        .segments
                        .iter()
                        .map(|s| s.ident.to_string())
                        .collect::<Vec<_>>()
                        .join("::"),
                    _ => "_".to_string(),
                })
                .collect();
            format!("impl {}", bounds.join(" + "))
        }
        Type::TraitObject(t) => {
            let bounds: Vec<String> = t
                .bounds
                .iter()
                .map(|b| match b {
                    syn::TypeParamBound::Trait(tr) => tr
                        .path
                        .segments
                        .iter()
                        .map(|s| s.ident.to_string())
                        .collect::<Vec<_>>()
                        .join("::"),
                    _ => "_".to_string(),
                })
                .collect();
            format!("dyn {}", bounds.join(" + "))
        }
        Type::Never(_) => "!".to_string(),
        Type::Infer(_) => "_".to_string(),
        _ => "_".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_crate_source() {
        // This test depends on having anyhow in your cargo cache
        let result = find_crate_source("anyhow", "1.0.95");
        println!("{:?}", result);
        // May fail if exact version not cached
    }

    #[test]
    fn test_parse_crate() {
        let result = parse_crate("anyhow", "1.0.95");
        if let Ok(api) = result {
            println!("Found {} symbols", api.docs.len());
            for doc in api.docs.iter().take(20) {
                println!("  {} ({})", doc.path, doc.kind);
                if let Some(sig) = &doc.signature {
                    println!("    {}", sig);
                }
            }
        }
    }
}
