//! Tree-sitter based TypeScript/JavaScript source parser.
//!
//! Extracts API surface from TypeScript/JavaScript source files, producing the universal
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TsLanguage {
    TypeScript,
    Tsx,
    JavaScript,
}

pub struct TypeScriptParser {
    parser: Parser,
}

impl TypeScriptParser {
    pub fn new(language: TsLanguage) -> Result<Self, ParseError> {
        let mut parser = Parser::new();
        let ts_language = match language {
            TsLanguage::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
            TsLanguage::Tsx => tree_sitter_typescript::LANGUAGE_TSX,
            TsLanguage::JavaScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT, // JS is valid TS
        };
        parser
            .set_language(&ts_language.into())
            .map_err(|_| ParseError::TreeSitterInit)?;
        Ok(Self { parser })
    }

    /// Parse a TypeScript source file and extract all exported API items.
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
        self.collect_definitions(root, source, module_path, items);
    }

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
                // Export statements - only handle these at top level
                "export_statement" => {
                    self.parse_export_statement(child, source, module_path, items);
                }
                // Module/namespace declaration
                "module" | "internal_module" => {
                    self.parse_module(child, source, module_path, items);
                }
                // Skip standalone declarations - we only care about exports for TypeScript
                // (non-exported items are private implementation details)
                _ => {}
            }
        }
    }

    fn parse_export_statement(
        &self,
        node: Node,
        source: &str,
        module_path: &str,
        items: &mut Vec<Item>,
    ) {
        // Get the doc comment from the export statement itself
        let export_doc = self.get_doc_comment(node, source);

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "class_declaration" | "abstract_class_declaration" => {
                    if let Some(mut item) = self.parse_class(child, source, module_path, true) {
                        // Use export doc if the item doesn't have its own doc
                        if item.doc.is_none() {
                            item.doc = export_doc.clone();
                        }
                        items.push(item);
                    }
                }
                "interface_declaration" => {
                    if let Some(mut item) = self.parse_interface(child, source, module_path, true) {
                        if item.doc.is_none() {
                            item.doc = export_doc.clone();
                        }
                        items.push(item);
                    }
                }
                "type_alias_declaration" => {
                    if let Some(mut item) = self.parse_type_alias(child, source, module_path, true)
                    {
                        if item.doc.is_none() {
                            item.doc = export_doc.clone();
                        }
                        items.push(item);
                    }
                }
                "function_declaration" => {
                    if let Some(mut item) = self.parse_function(child, source, module_path, true) {
                        if item.doc.is_none() {
                            item.doc = export_doc.clone();
                        }
                        items.push(item);
                    }
                }
                "lexical_declaration" => {
                    self.parse_lexical_declaration(child, source, module_path, true, items);
                }
                "enum_declaration" => {
                    if let Some(mut item) = self.parse_enum(child, source, module_path, true) {
                        if item.doc.is_none() {
                            item.doc = export_doc.clone();
                        }
                        items.push(item);
                    }
                }
                _ => {}
            }
        }
    }

    fn parse_class(
        &self,
        node: Node,
        source: &str,
        module_path: &str,
        exported: bool,
    ) -> Option<Item> {
        let name = self.get_child_text(node, "type_identifier", source)?;
        let doc = self.get_doc_comment(node, source);
        let signature = self.get_class_signature(node, source);
        let is_abstract = node.kind() == "abstract_class_declaration";

        // Parse class body for methods and fields
        let (methods, fields) = self.parse_class_body(node, source);

        // Parse extends/implements
        let traits = self.parse_class_heritage(node, source);

        Some(Item {
            path: format_path(module_path, &name),
            kind: ItemKind::Struct,
            signature: Some(if is_abstract {
                format!("abstract class {}", signature)
            } else {
                format!("class {}", signature)
            }),
            doc,
            visibility: if exported {
                Visibility::Public
            } else {
                Visibility::Private
            },
            fields,
            methods,
            traits,
            variants: vec![],
            related: vec![],
            since: None,
            until: None,
            moved_from: None,
            deprecated: None,
        })
    }

    fn parse_class_body(&self, node: Node, source: &str) -> (Vec<Method>, Vec<Field>) {
        let mut methods = Vec::new();
        let mut fields = Vec::new();

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "class_body" {
                let mut body_cursor = child.walk();
                for member in child.children(&mut body_cursor) {
                    match member.kind() {
                        "method_definition" => {
                            if let Some(method) = self.parse_method(member, source) {
                                methods.push(method);
                            }
                        }
                        "property_signature" | "public_field_definition" => {
                            if let Some(field) = self.parse_class_field(member, source) {
                                fields.push(field);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        (methods, fields)
    }

    fn parse_method(&self, node: Node, source: &str) -> Option<Method> {
        let name = self.get_method_name(node, source)?;
        let vis = self.get_member_visibility(node, source);
        let doc = self.get_doc_comment(node, source);
        let signature = self.get_method_signature(node, source);

        Some(Method {
            name,
            signature: Some(signature),
            doc,
            visibility: vis,
        })
    }

    fn parse_class_field(&self, node: Node, source: &str) -> Option<Field> {
        let name = self.get_property_name(node, source)?;
        let vis = self.get_member_visibility(node, source);
        let ty = self.get_type_annotation(node, source);
        let doc = self.get_doc_comment(node, source);

        Some(Field {
            name,
            ty,
            doc,
            visibility: vis,
        })
    }

    fn parse_class_heritage(&self, node: Node, source: &str) -> Vec<String> {
        let mut traits = Vec::new();

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "class_heritage" => {
                    let mut heritage_cursor = child.walk();
                    for heritage in child.children(&mut heritage_cursor) {
                        if heritage.kind() == "extends_clause"
                            || heritage.kind() == "implements_clause"
                        {
                            let mut type_cursor = heritage.walk();
                            for type_node in heritage.children(&mut type_cursor) {
                                // Handle type_identifier (Foo), generic_type (Foo<T>),
                                // and member_expression (vscode.TreeItem)
                                if type_node.kind() == "type_identifier"
                                    || type_node.kind() == "generic_type"
                                    || type_node.kind() == "member_expression"
                                    || type_node.kind() == "nested_type_identifier"
                                {
                                    traits.push(self.get_node_text(type_node, source));
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        traits
    }

    fn parse_interface(
        &self,
        node: Node,
        source: &str,
        module_path: &str,
        exported: bool,
    ) -> Option<Item> {
        let name = self.get_child_text(node, "type_identifier", source)?;
        let doc = self.get_doc_comment(node, source);
        let signature = self.get_interface_signature(node, source);

        // Parse interface body for methods and properties
        let (methods, fields) = self.parse_interface_body(node, source);

        // Parse extends
        let traits = self.parse_interface_extends(node, source);

        Some(Item {
            path: format_path(module_path, &name),
            kind: ItemKind::Trait,
            signature: Some(format!("interface {}", signature)),
            doc,
            visibility: if exported {
                Visibility::Public
            } else {
                Visibility::Private
            },
            fields,
            methods,
            traits,
            variants: vec![],
            related: vec![],
            since: None,
            until: None,
            moved_from: None,
            deprecated: None,
        })
    }

    fn parse_interface_body(&self, node: Node, source: &str) -> (Vec<Method>, Vec<Field>) {
        let mut methods = Vec::new();
        let mut fields = Vec::new();

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "object_type" || child.kind() == "interface_body" {
                let mut body_cursor = child.walk();
                for member in child.children(&mut body_cursor) {
                    match member.kind() {
                        "method_signature" | "call_signature" => {
                            if let Some(method) = self.parse_interface_method(member, source) {
                                methods.push(method);
                            }
                        }
                        "property_signature" => {
                            if let Some(field) = self.parse_interface_property(member, source) {
                                fields.push(field);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        (methods, fields)
    }

    fn parse_interface_method(&self, node: Node, source: &str) -> Option<Method> {
        let name = self
            .get_property_name(node, source)
            .unwrap_or_else(|| "call".to_string());
        let doc = self.get_doc_comment(node, source);
        let signature = self.get_node_text(node, source);

        Some(Method {
            name,
            signature: Some(signature),
            doc,
            visibility: Visibility::Public,
        })
    }

    fn parse_interface_property(&self, node: Node, source: &str) -> Option<Field> {
        let name = self.get_property_name(node, source)?;
        let ty = self.get_type_annotation(node, source);
        let doc = self.get_doc_comment(node, source);

        Some(Field {
            name,
            ty,
            doc,
            visibility: Visibility::Public,
        })
    }

    fn parse_interface_extends(&self, node: Node, source: &str) -> Vec<String> {
        let mut extends = Vec::new();

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "extends_type_clause" {
                let mut extends_cursor = child.walk();
                for type_node in child.children(&mut extends_cursor) {
                    if type_node.kind() == "type_identifier" || type_node.kind() == "generic_type" {
                        extends.push(self.get_node_text(type_node, source));
                    }
                }
            }
        }

        extends
    }

    fn parse_type_alias(
        &self,
        node: Node,
        source: &str,
        module_path: &str,
        exported: bool,
    ) -> Option<Item> {
        let name = self.get_child_text(node, "type_identifier", source)?;
        let doc = self.get_doc_comment(node, source);
        let signature = self.get_node_text(node, source);

        Some(Item {
            path: format_path(module_path, &name),
            kind: ItemKind::TypeAlias,
            signature: Some(signature),
            doc,
            visibility: if exported {
                Visibility::Public
            } else {
                Visibility::Private
            },
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

    fn parse_function(
        &self,
        node: Node,
        source: &str,
        module_path: &str,
        exported: bool,
    ) -> Option<Item> {
        let name = self.get_child_text(node, "identifier", source)?;
        let doc = self.get_doc_comment(node, source);
        let signature = self.get_function_signature(node, source);

        Some(Item {
            path: format_path(module_path, &name),
            kind: ItemKind::Function,
            signature: Some(signature),
            doc,
            visibility: if exported {
                Visibility::Public
            } else {
                Visibility::Private
            },
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

    fn parse_lexical_declaration(
        &self,
        node: Node,
        source: &str,
        module_path: &str,
        exported: bool,
        items: &mut Vec<Item>,
    ) {
        self.parse_lexical_declaration_with_doc(node, source, module_path, exported, None, items);
    }

    fn parse_lexical_declaration_with_doc(
        &self,
        node: Node,
        source: &str,
        module_path: &str,
        exported: bool,
        export_doc: Option<String>,
        items: &mut Vec<Item>,
    ) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "variable_declarator" {
                if let Some(mut item) =
                    self.parse_variable_declarator(child, source, module_path, exported)
                {
                    if item.doc.is_none() {
                        item.doc = export_doc.clone();
                    }
                    items.push(item);
                }
            }
        }
    }

    fn parse_variable_declarator(
        &self,
        node: Node,
        source: &str,
        module_path: &str,
        exported: bool,
    ) -> Option<Item> {
        let name = self.get_child_text(node, "identifier", source)?;
        let doc = self.get_doc_comment(node.parent()?, source);

        // Check if it's an arrow function or regular function expression
        let mut cursor = node.walk();
        let mut is_function = false;
        let mut signature = None;

        for child in node.children(&mut cursor) {
            match child.kind() {
                "arrow_function" | "function" | "function_expression" => {
                    is_function = true;
                    signature = Some(self.get_arrow_function_signature(child, &name, source));
                }
                "type_annotation" => {
                    if !is_function {
                        signature = Some(format!(
                            "const {}: {}",
                            name,
                            self.get_type_from_annotation(child, source)
                        ));
                    }
                }
                _ => {}
            }
        }

        let kind = if is_function {
            ItemKind::Function
        } else {
            ItemKind::Constant
        };

        Some(Item {
            path: format_path(module_path, &name),
            kind,
            signature: signature.or_else(|| Some(format!("const {}", name))),
            doc,
            visibility: if exported {
                Visibility::Public
            } else {
                Visibility::Private
            },
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

    fn parse_enum(
        &self,
        node: Node,
        source: &str,
        module_path: &str,
        exported: bool,
    ) -> Option<Item> {
        let name = self.get_child_text(node, "identifier", source)?;
        let doc = self.get_doc_comment(node, source);
        let variants = self.parse_enum_variants(node, source);

        Some(Item {
            path: format_path(module_path, &name),
            kind: ItemKind::Enum,
            signature: Some(format!("enum {}", name)),
            doc,
            visibility: if exported {
                Visibility::Public
            } else {
                Visibility::Private
            },
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
            if child.kind() == "enum_body" {
                let mut body_cursor = child.walk();
                for member in child.children(&mut body_cursor) {
                    if member.kind() == "enum_assignment" || member.kind() == "property_identifier"
                    {
                        let name = if member.kind() == "enum_assignment" {
                            self.get_child_text(member, "property_identifier", source)
                        } else {
                            Some(self.get_node_text(member, source))
                        };

                        if let Some(name) = name {
                            variants.push(Variant {
                                name,
                                doc: self.get_doc_comment(member, source),
                                fields: vec![],
                            });
                        }
                    }
                }
            }
        }

        variants
    }

    fn parse_module(&self, node: Node, source: &str, module_path: &str, items: &mut Vec<Item>) {
        let name = self.get_module_name(node, source);
        if let Some(name) = name {
            let new_path = format_path(module_path, &name);
            let doc = self.get_doc_comment(node, source);

            items.push(Item {
                path: new_path.clone(),
                kind: ItemKind::Module,
                signature: Some(format!("namespace {}", name)),
                doc,
                visibility: Visibility::Public,
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

            // Parse module body
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "statement_block" {
                    self.collect_definitions(child, source, &new_path, items);
                }
            }
        }
    }

    // Helper methods

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

    fn get_doc_comment(&self, node: Node, source: &str) -> Option<String> {
        let mut current = node.prev_sibling();
        let mut docs = Vec::new();

        while let Some(sibling) = current {
            match sibling.kind() {
                "comment" => {
                    let text = self.get_node_text(sibling, source);
                    if text.starts_with("/**") {
                        // JSDoc comment
                        let doc = text
                            .strip_prefix("/**")
                            .and_then(|s| s.strip_suffix("*/"))
                            .map(|s| {
                                s.lines()
                                    .map(|line| line.trim().trim_start_matches('*').trim())
                                    .filter(|line| !line.is_empty())
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            });
                        if let Some(doc) = doc {
                            docs.push(doc);
                        }
                        break;
                    } else if text.starts_with("//") {
                        docs.push(text.trim_start_matches("//").trim().to_string());
                    } else {
                        break;
                    }
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

    fn get_member_visibility(&self, node: Node, source: &str) -> Visibility {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "accessibility_modifier" {
                let text = self.get_node_text(child, source);
                return match text.as_str() {
                    "public" => Visibility::Public,
                    "protected" => Visibility::Crate, // Map protected to Crate visibility
                    "private" => Visibility::Private,
                    _ => Visibility::Public,
                };
            }
        }
        Visibility::Public // Default is public in TS/JS
    }

    fn get_class_signature(&self, node: Node, source: &str) -> String {
        let name = self
            .get_child_text(node, "type_identifier", source)
            .unwrap_or_else(|| "Unknown".to_string());

        let mut type_params = String::new();
        let mut heritage = String::new();

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "type_parameters" => {
                    type_params = self.get_node_text(child, source);
                }
                "class_heritage" => {
                    heritage = format!(" {}", self.get_node_text(child, source));
                }
                _ => {}
            }
        }

        format!("{}{}{}", name, type_params, heritage)
    }

    fn get_interface_signature(&self, node: Node, source: &str) -> String {
        let name = self
            .get_child_text(node, "type_identifier", source)
            .unwrap_or_else(|| "Unknown".to_string());

        let mut type_params = String::new();
        let mut extends = String::new();

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "type_parameters" => {
                    type_params = self.get_node_text(child, source);
                }
                "extends_type_clause" => {
                    extends = format!(" {}", self.get_node_text(child, source));
                }
                _ => {}
            }
        }

        format!("{}{}{}", name, type_params, extends)
    }

    fn get_function_signature(&self, node: Node, source: &str) -> String {
        let text = self.get_node_text(node, source);
        // Get just the signature without the body
        if let Some(brace) = text.find('{') {
            text[..brace].trim().to_string()
        } else {
            text
        }
    }

    fn get_method_signature(&self, node: Node, source: &str) -> String {
        let text = self.get_node_text(node, source);
        if let Some(brace) = text.find('{') {
            text[..brace].trim().to_string()
        } else {
            text
        }
    }

    fn get_method_name(&self, node: Node, source: &str) -> Option<String> {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "property_identifier" | "identifier" => {
                    return Some(self.get_node_text(child, source));
                }
                "computed_property_name" => {
                    return Some(self.get_node_text(child, source));
                }
                _ => {}
            }
        }
        None
    }

    fn get_property_name(&self, node: Node, source: &str) -> Option<String> {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "property_identifier" | "identifier" => {
                    return Some(self.get_node_text(child, source));
                }
                _ => {}
            }
        }
        None
    }

    fn get_type_annotation(&self, node: Node, source: &str) -> Option<String> {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "type_annotation" {
                return Some(self.get_type_from_annotation(child, source));
            }
        }
        None
    }

    fn get_type_from_annotation(&self, node: Node, source: &str) -> String {
        let text = self.get_node_text(node, source);
        text.trim_start_matches(':').trim().to_string()
    }

    fn get_arrow_function_signature(&self, node: Node, name: &str, source: &str) -> String {
        let mut params = String::new();
        let mut return_type = String::new();

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "formal_parameters" => {
                    params = self.get_node_text(child, source);
                }
                "type_annotation" => {
                    return_type = self.get_type_from_annotation(child, source);
                }
                _ => {}
            }
        }

        if return_type.is_empty() {
            format!("const {} = {}", name, params)
        } else {
            format!("const {} = {}: {}", name, params, return_type)
        }
    }

    fn get_module_name(&self, node: Node, source: &str) -> Option<String> {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "identifier" | "string" => {
                    let text = self.get_node_text(child, source);
                    return Some(text.trim_matches('"').trim_matches('\'').to_string());
                }
                _ => {}
            }
        }
        None
    }
}

fn format_path(module_path: &str, name: &str) -> String {
    if module_path.is_empty() {
        name.to_string()
    } else {
        format!("{}.{}", module_path, name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_interface() {
        let source = r#"
/** A license key from the API. */
export interface LemonSqueezyLicenseKey {
    id: number;
    status: 'active' | 'inactive' | 'expired';
    key: string;
}
"#;
        let mut parser = TypeScriptParser::new(TsLanguage::TypeScript).unwrap();
        let items = parser.parse_source(source, "license").unwrap();

        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.path, "license.LemonSqueezyLicenseKey");
        assert_eq!(item.kind, ItemKind::Trait);
        assert_eq!(item.visibility, Visibility::Public);
        assert_eq!(item.fields.len(), 3);
    }

    #[test]
    fn test_parse_class() {
        let source = r#"
export class MenuItem {
    constructor(
        public readonly label: string,
        public readonly commandId: string
    ) {}

    public activate(): void {}
}
"#;
        let mut parser = TypeScriptParser::new(TsLanguage::TypeScript).unwrap();
        let items = parser.parse_source(source, "extension").unwrap();

        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.path, "extension.MenuItem");
        assert_eq!(item.kind, ItemKind::Struct);
    }

    #[test]
    fn test_parse_function() {
        let source = r#"
export async function checkToolExists(tool: string): Promise<boolean> {
    return true;
}
"#;
        let mut parser = TypeScriptParser::new(TsLanguage::TypeScript).unwrap();
        let items = parser.parse_source(source, "utils").unwrap();

        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.path, "utils.checkToolExists");
        assert_eq!(item.kind, ItemKind::Function);
    }

    #[test]
    fn test_parse_type_alias() {
        let source = r#"
export type OutputFormat = 'markdown' | 'json' | 'xml';
"#;
        let mut parser = TypeScriptParser::new(TsLanguage::TypeScript).unwrap();
        let items = parser.parse_source(source, "types").unwrap();

        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.path, "types.OutputFormat");
        assert_eq!(item.kind, ItemKind::TypeAlias);
    }

    #[test]
    fn test_parse_const_arrow_function() {
        let source = r#"
export const execFileAsync = promisify(execFile);

export const runArchmapAi = async (root: string): Promise<string> => {
    return "";
};
"#;
        let mut parser = TypeScriptParser::new(TsLanguage::TypeScript).unwrap();
        let items = parser.parse_source(source, "extension").unwrap();

        assert_eq!(items.len(), 2);
    }
}
