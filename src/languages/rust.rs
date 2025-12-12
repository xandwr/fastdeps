//! Tree-sitter based Rust source parser.
//!
//! Extracts API surface from Rust source files, producing the universal
//! `schema::Item` format.

use crate::schema::{Field, Item, ItemKind, Method, Variant, Visibility};
use thiserror::Error;
use tree_sitter::{Node, Parser, Tree};

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("tree-sitter initialization failed")]
    TreeSitterInit,
    #[error("failed to parse source")]
    ParseFailed,
}

pub struct RustParser {
    parser: Parser,
}

impl RustParser {
    pub fn new() -> Result<Self, ParseError> {
        let mut parser = Parser::new();
        let language = tree_sitter_rust::LANGUAGE;
        parser
            .set_language(&language.into())
            .map_err(|_| ParseError::TreeSitterInit)?;
        Ok(Self { parser })
    }

    /// Parse a Rust source file and extract all public API items.
    pub fn parse_source(
        &mut self,
        source: &str,
        module_path: &str,
    ) -> Result<Vec<Item>, ParseError> {
        let tree = self
            .parser
            .parse(source, None)
            .ok_or(ParseError::ParseFailed)?;

        let mut items = Vec::new();
        self.extract_items(&tree, source, module_path, &mut items);
        Ok(items)
    }

    fn extract_items(&self, tree: &Tree, source: &str, module_path: &str, items: &mut Vec<Item>) {
        let root = tree.root_node();
        // Two-pass: first collect type definitions, then process impl blocks
        self.collect_definitions(root, source, module_path, items);
        self.process_impls(root, source, module_path, items);
    }

    /// First pass: collect struct, enum, trait, function, etc. definitions.
    fn collect_definitions(
        &self,
        node: Node,
        source: &str,
        module_path: &str,
        items: &mut Vec<Item>,
    ) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "struct_item" => {
                    if let Some(item) = self.parse_struct(child, source, module_path) {
                        items.push(item);
                    }
                }
                "enum_item" => {
                    if let Some(item) = self.parse_enum(child, source, module_path) {
                        items.push(item);
                    }
                }
                "trait_item" => {
                    if let Some(item) = self.parse_trait(child, source, module_path) {
                        items.push(item);
                    }
                }
                "function_item" => {
                    if let Some(item) = self.parse_function(child, source, module_path) {
                        items.push(item);
                    }
                }
                "type_item" => {
                    if let Some(item) = self.parse_type_alias(child, source, module_path) {
                        items.push(item);
                    }
                }
                "const_item" | "static_item" => {
                    if let Some(item) = self.parse_const(child, source, module_path) {
                        items.push(item);
                    }
                }
                "mod_item" => {
                    self.parse_mod(child, source, module_path, items);
                }
                "macro_definition" => {
                    if let Some(item) = self.parse_macro(child, source, module_path) {
                        items.push(item);
                    }
                }
                _ => {}
            }
        }
    }

    /// Second pass: process impl blocks and attach methods/traits to their types.
    fn process_impls(&self, node: Node, source: &str, module_path: &str, items: &mut Vec<Item>) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "impl_item" {
                self.parse_impl(child, source, module_path, items);
            }
        }
    }

    fn parse_struct(&self, node: Node, source: &str, module_path: &str) -> Option<Item> {
        let vis = self.get_visibility(node, source);
        let name = self.get_child_text(node, "type_identifier", source)?;
        let doc = self.get_doc_comment(node, source);
        let signature = self.get_node_text(node, source);

        let fields = self.parse_struct_fields(node, source);

        Some(Item {
            path: format_path(module_path, &name),
            kind: ItemKind::Struct,
            signature: Some(signature),
            doc,
            visibility: vis,
            fields,
            methods: vec![],
            traits: vec![],
            variants: vec![],
            related: vec![],
            since: None,
            until: None,
            moved_from: None,
            deprecated: None,
        })
    }

    fn parse_struct_fields(&self, node: Node, source: &str) -> Vec<Field> {
        let mut fields = Vec::new();

        // Look for field_declaration_list
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "field_declaration_list" {
                let mut field_cursor = child.walk();
                for field_node in child.children(&mut field_cursor) {
                    if field_node.kind() == "field_declaration" {
                        if let Some(field) = self.parse_field(field_node, source) {
                            fields.push(field);
                        }
                    }
                }
            }
        }
        fields
    }

    fn parse_field(&self, node: Node, source: &str) -> Option<Field> {
        let vis = self.get_visibility(node, source);
        let name = self.get_child_text(node, "field_identifier", source)?;
        let ty = self
            .find_child_by_field(node, "type")
            .map(|n| self.get_node_text(n, source));
        let doc = self.get_doc_comment(node, source);

        Some(Field {
            name,
            ty,
            doc,
            visibility: vis,
        })
    }

    fn parse_enum(&self, node: Node, source: &str, module_path: &str) -> Option<Item> {
        let vis = self.get_visibility(node, source);
        let name = self.get_child_text(node, "type_identifier", source)?;
        let doc = self.get_doc_comment(node, source);
        let signature = self.get_node_text(node, source);

        let variants = self.parse_enum_variants(node, source);

        Some(Item {
            path: format_path(module_path, &name),
            kind: ItemKind::Enum,
            signature: Some(signature),
            doc,
            visibility: vis,
            fields: vec![],
            methods: vec![],
            traits: vec![],
            variants,
            related: vec![],
            since: None,
            until: None,
            moved_from: None,
            deprecated: None,
        })
    }

    fn parse_enum_variants(&self, node: Node, source: &str) -> Vec<Variant> {
        let mut variants = Vec::new();

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "enum_variant_list" {
                let mut var_cursor = child.walk();
                for var_node in child.children(&mut var_cursor) {
                    if var_node.kind() == "enum_variant" {
                        if let Some(var) = self.parse_variant(var_node, source) {
                            variants.push(var);
                        }
                    }
                }
            }
        }
        variants
    }

    fn parse_variant(&self, node: Node, source: &str) -> Option<Variant> {
        let name = self.get_child_text(node, "identifier", source)?;
        let doc = self.get_doc_comment(node, source);

        // Parse variant fields if it's a struct variant
        let mut fields = Vec::new();
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "field_declaration_list" {
                let mut field_cursor = child.walk();
                for field_node in child.children(&mut field_cursor) {
                    if field_node.kind() == "field_declaration" {
                        if let Some(field) = self.parse_field(field_node, source) {
                            fields.push(field);
                        }
                    }
                }
            }
        }

        Some(Variant { name, doc, fields })
    }

    fn parse_trait(&self, node: Node, source: &str, module_path: &str) -> Option<Item> {
        let vis = self.get_visibility(node, source);
        let name = self.get_child_text(node, "type_identifier", source)?;
        let doc = self.get_doc_comment(node, source);
        let signature = self.get_signature_line(node, source);

        let methods = self.parse_trait_methods(node, source);

        Some(Item {
            path: format_path(module_path, &name),
            kind: ItemKind::Trait,
            signature: Some(signature),
            doc,
            visibility: vis,
            fields: vec![],
            methods,
            traits: vec![],
            variants: vec![],
            related: vec![],
            since: None,
            until: None,
            moved_from: None,
            deprecated: None,
        })
    }

    fn parse_trait_methods(&self, node: Node, source: &str) -> Vec<Method> {
        let mut methods = Vec::new();

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "declaration_list" {
                let mut method_cursor = child.walk();
                for method_node in child.children(&mut method_cursor) {
                    if method_node.kind() == "function_signature_item"
                        || method_node.kind() == "function_item"
                    {
                        if let Some(method) = self.parse_method(method_node, source) {
                            methods.push(method);
                        }
                    }
                }
            }
        }
        methods
    }

    fn parse_method(&self, node: Node, source: &str) -> Option<Method> {
        let name = self.get_child_text(node, "identifier", source)?;
        let vis = self.get_visibility(node, source);
        let doc = self.get_doc_comment(node, source);
        let signature = self.get_signature_line(node, source);

        Some(Method {
            name,
            signature: Some(signature),
            doc,
            visibility: vis,
        })
    }

    fn parse_function(&self, node: Node, source: &str, module_path: &str) -> Option<Item> {
        let vis = self.get_visibility(node, source);
        let name = self.get_child_text(node, "identifier", source)?;
        let doc = self.get_doc_comment(node, source);
        let signature = self.get_signature_line(node, source);

        Some(Item {
            path: format_path(module_path, &name),
            kind: ItemKind::Function,
            signature: Some(signature),
            doc,
            visibility: vis,
            fields: vec![],
            methods: vec![],
            traits: vec![],
            variants: vec![],
            related: vec![],
            since: None,
            until: None,
            moved_from: None,
            deprecated: None,
        })
    }

    fn parse_type_alias(&self, node: Node, source: &str, module_path: &str) -> Option<Item> {
        let vis = self.get_visibility(node, source);
        let name = self.get_child_text(node, "type_identifier", source)?;
        let doc = self.get_doc_comment(node, source);
        let signature = self.get_node_text(node, source);

        Some(Item {
            path: format_path(module_path, &name),
            kind: ItemKind::TypeAlias,
            signature: Some(signature),
            doc,
            visibility: vis,
            fields: vec![],
            methods: vec![],
            traits: vec![],
            variants: vec![],
            related: vec![],
            since: None,
            until: None,
            moved_from: None,
            deprecated: None,
        })
    }

    fn parse_const(&self, node: Node, source: &str, module_path: &str) -> Option<Item> {
        let vis = self.get_visibility(node, source);
        let name = self.get_child_text(node, "identifier", source)?;
        let doc = self.get_doc_comment(node, source);
        let signature = self.get_node_text(node, source);

        Some(Item {
            path: format_path(module_path, &name),
            kind: ItemKind::Constant,
            signature: Some(signature),
            doc,
            visibility: vis,
            fields: vec![],
            methods: vec![],
            traits: vec![],
            variants: vec![],
            related: vec![],
            since: None,
            until: None,
            moved_from: None,
            deprecated: None,
        })
    }

    fn parse_macro(&self, node: Node, source: &str, module_path: &str) -> Option<Item> {
        // macro_rules! macros
        let name = self.get_child_text(node, "identifier", source)?;
        let doc = self.get_doc_comment(node, source);

        Some(Item {
            path: format_path(module_path, &name),
            kind: ItemKind::Macro,
            signature: Some(format!("macro_rules! {name}")),
            doc,
            visibility: Visibility::Public, // macro_rules! are pub by default if exported
            fields: vec![],
            methods: vec![],
            traits: vec![],
            variants: vec![],
            related: vec![],
            since: None,
            until: None,
            moved_from: None,
            deprecated: None,
        })
    }

    fn parse_impl(&self, node: Node, source: &str, module_path: &str, items: &mut Vec<Item>) {
        // Get the type being implemented
        let type_name = self.find_impl_type(node, source);

        // Check if this is a trait impl
        let trait_name = self.find_impl_trait(node, source);

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "declaration_list" {
                let mut method_cursor = child.walk();
                for method_node in child.children(&mut method_cursor) {
                    if method_node.kind() == "function_item" {
                        // Only extract pub methods as standalone items
                        let vis = self.get_visibility(method_node, source);
                        if vis == Visibility::Public {
                            // For trait impls, we typically don't need to add as separate items
                            // For inherent impls, add methods to the type's methods list
                            // This is a simplification - real impl would merge with the struct item
                            if trait_name.is_none() {
                                if let (Some(type_name), Some(method)) =
                                    (&type_name, self.parse_method(method_node, source))
                                {
                                    // Find the existing struct and add the method
                                    let type_path = format_path(module_path, type_name);
                                    if let Some(item) =
                                        items.iter_mut().find(|i| i.path == type_path)
                                    {
                                        item.methods.push(method);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // If it's a trait impl, record the trait in the struct's traits list
        if let (Some(type_name), Some(trait_name)) = (&type_name, &trait_name) {
            let type_path = format_path(module_path, type_name);
            if let Some(item) = items.iter_mut().find(|i| i.path == type_path) {
                if !item.traits.contains(trait_name) {
                    item.traits.push(trait_name.clone());
                }
            }
        }
    }

    fn parse_mod(&self, node: Node, source: &str, module_path: &str, items: &mut Vec<Item>) {
        let vis = self.get_visibility(node, source);
        let name = self.get_child_text(node, "identifier", source);

        if let Some(name) = name {
            let new_path = format_path(module_path, &name);
            let doc = self.get_doc_comment(node, source);

            // Add the module itself as an item
            items.push(Item {
                path: new_path.clone(),
                kind: ItemKind::Module,
                signature: None,
                doc,
                visibility: vis,
                fields: vec![],
                methods: vec![],
                traits: vec![],
                variants: vec![],
                related: vec![],
                since: None,
                until: None,
                moved_from: None,
                deprecated: None,
            });

            // Parse items inside the module (two-pass for nested modules too)
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "declaration_list" {
                    self.collect_definitions(child, source, &new_path, items);
                    self.process_impls(child, source, &new_path, items);
                }
            }
        }
    }

    fn find_impl_type(&self, node: Node, source: &str) -> Option<String> {
        // The type is usually after "for" in trait impl, or the direct type in inherent impl
        // In `impl Trait for Type`, we want Type (after "for")
        // In `impl Type`, we want Type (the only type)
        let mut cursor = node.walk();
        let mut found_for = false;
        let mut first_type = None;

        for child in node.children(&mut cursor) {
            if child.kind() == "for" {
                found_for = true;
            } else if child.kind() == "type_identifier" || child.kind() == "generic_type" {
                if found_for {
                    // This is the type after "for" - this is what we want for trait impls
                    return Some(self.get_type_name(child, source));
                } else if first_type.is_none() {
                    // Save the first type we see (might be trait or type in inherent impl)
                    first_type = Some(self.get_type_name(child, source));
                }
            }
        }

        // If we didn't find "for", the first_type is the impl target (inherent impl)
        // If we did find "for" but no type after it, something's wrong
        if !found_for { first_type } else { None }
    }

    fn find_impl_trait(&self, node: Node, source: &str) -> Option<String> {
        let mut cursor = node.walk();
        let mut has_for = false;

        // First pass: check if this is a trait impl (has "for")
        for child in node.children(&mut cursor) {
            if child.kind() == "for" {
                has_for = true;
                break;
            }
        }

        if !has_for {
            return None;
        }

        // Second pass: get the trait name (first type before "for")
        cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "for" {
                break;
            }
            if child.kind() == "type_identifier"
                || child.kind() == "generic_type"
                || child.kind() == "scoped_type_identifier"
            {
                return Some(self.get_type_name(child, source));
            }
        }
        None
    }

    fn get_type_name(&self, node: Node, source: &str) -> String {
        match node.kind() {
            "type_identifier" => self.get_node_text(node, source),
            "generic_type" => {
                // Get just the base type name without generics
                self.get_child_text(node, "type_identifier", source)
                    .unwrap_or_else(|| self.get_node_text(node, source))
            }
            "scoped_type_identifier" => {
                // Get the full scoped name
                self.get_node_text(node, source)
            }
            _ => self.get_node_text(node, source),
        }
    }

    // Helper methods

    fn get_visibility(&self, node: Node, source: &str) -> Visibility {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "visibility_modifier" {
                let text = self.get_node_text(child, source);
                if text == "pub" {
                    return Visibility::Public;
                } else if text.starts_with("pub(crate)") {
                    return Visibility::Crate;
                } else if text.starts_with("pub(super)") || text.starts_with("pub(self)") {
                    return Visibility::Private;
                }
            }
        }
        Visibility::Private
    }

    fn get_doc_comment(&self, node: Node, source: &str) -> Option<String> {
        // Look for preceding line_comment or block_comment nodes that are doc comments
        let mut docs = Vec::new();
        let mut current = node.prev_sibling();

        while let Some(sibling) = current {
            match sibling.kind() {
                "line_comment" => {
                    let text = self.get_node_text(sibling, source);
                    if let Some(doc) = text.strip_prefix("///") {
                        docs.push(doc.trim().to_string());
                    } else if text.starts_with("//!") {
                        // Inner doc comment, skip
                        break;
                    } else {
                        // Regular comment, stop
                        break;
                    }
                }
                "block_comment" => {
                    let text = self.get_node_text(sibling, source);
                    if let Some(doc) = text.strip_prefix("/**") {
                        if let Some(doc) = doc.strip_suffix("*/") {
                            docs.push(doc.trim().to_string());
                        }
                    }
                    break;
                }
                "attribute_item" => {
                    // Skip attributes, keep looking for doc comments
                }
                _ => break,
            }
            current = sibling.prev_sibling();
        }

        if docs.is_empty() {
            None
        } else {
            docs.reverse();
            Some(docs.join("\n"))
        }
    }

    fn get_node_text(&self, node: Node, source: &str) -> String {
        source[node.byte_range()].to_string()
    }

    fn get_child_text(&self, node: Node, kind: &str, source: &str) -> Option<String> {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == kind {
                return Some(self.get_node_text(child, source));
            }
        }
        None
    }

    fn find_child_by_field<'a>(&self, node: Node<'a>, field: &str) -> Option<Node<'a>> {
        node.child_by_field_name(field)
    }

    fn get_signature_line(&self, node: Node, source: &str) -> String {
        // Get just the signature without the body
        let text = self.get_node_text(node, source);
        if let Some(brace) = text.find('{') {
            text[..brace].trim().to_string()
        } else {
            text
        }
    }
}

fn format_path(module_path: &str, name: &str) -> String {
    if module_path.is_empty() {
        name.to_string()
    } else {
        format!("{module_path}::{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_struct() {
        let source = r#"
/// A simple struct.
pub struct Foo {
    /// The value.
    pub value: i32,
    private: String,
}
"#;
        let mut parser = RustParser::new().unwrap();
        let items = parser.parse_source(source, "crate").unwrap();

        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.path, "crate::Foo");
        assert_eq!(item.kind, ItemKind::Struct);
        assert_eq!(item.doc, Some("A simple struct.".into()));
        assert_eq!(item.visibility, Visibility::Public);
        assert_eq!(item.fields.len(), 2);
        assert_eq!(item.fields[0].name, "value");
        assert_eq!(item.fields[0].visibility, Visibility::Public);
        assert_eq!(item.fields[1].name, "private");
        assert_eq!(item.fields[1].visibility, Visibility::Private);
    }

    #[test]
    fn test_parse_enum() {
        let source = r#"
/// Result type.
pub enum MyResult<T, E> {
    /// Success.
    Ok(T),
    /// Error.
    Err(E),
}
"#;
        let mut parser = RustParser::new().unwrap();
        let items = parser.parse_source(source, "crate").unwrap();

        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.path, "crate::MyResult");
        assert_eq!(item.kind, ItemKind::Enum);
        assert_eq!(item.variants.len(), 2);
        assert_eq!(item.variants[0].name, "Ok");
        assert_eq!(item.variants[1].name, "Err");
    }

    #[test]
    fn test_parse_impl_methods() {
        let source = r#"
pub struct Foo;

impl Foo {
    pub fn new() -> Self {
        Foo
    }

    fn private_method(&self) {}
}
"#;
        let mut parser = RustParser::new().unwrap();
        let items = parser.parse_source(source, "crate").unwrap();

        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.methods.len(), 1); // Only pub methods
        assert_eq!(item.methods[0].name, "new");
    }

    #[test]
    fn test_parse_trait_impl() {
        let source = r#"
pub struct Foo;

impl Default for Foo {
    fn default() -> Self {
        Foo
    }
}

impl Clone for Foo {
    fn clone(&self) -> Self {
        Foo
    }
}
"#;
        let mut parser = RustParser::new().unwrap();
        let items = parser.parse_source(source, "crate").unwrap();

        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert!(item.traits.contains(&"Default".to_string()));
        assert!(item.traits.contains(&"Clone".to_string()));
    }
}
