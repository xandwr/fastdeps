//! Universal documentation interchange format.
//!
//! Designed to be language-agnostic while capturing the essential API surface
//! information needed for documentation, migration tracking, and code intelligence.

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Top-level index mapping packages to their data locations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Index {
    /// Schema version for forward compatibility.
    pub format_version: u32,
    /// When this index was generated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<String>,
    /// Map of "name@version" -> relative path to package directory.
    pub packages: BTreeMap<String, Utf8PathBuf>,
}

impl Index {
    pub const CURRENT_VERSION: u32 = 1;

    pub fn new() -> Self {
        Self {
            format_version: Self::CURRENT_VERSION,
            generated_at: None,
            packages: BTreeMap::new(),
        }
    }
}

impl Default for Index {
    fn default() -> Self {
        Self::new()
    }
}

/// Package-level metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageMeta {
    pub name: String,
    pub version: String,
    pub ecosystem: Ecosystem,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Top-level modules/namespaces in this package.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modules: Vec<String>,
    /// Direct dependencies: name -> version requirement.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, String>,
}

/// Supported language ecosystems.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Ecosystem {
    Rust,
    TypeScript,
    Python,
    Go,
}

/// The API surface of a package - a flat list of items.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageItems {
    pub items: Vec<Item>,
}

/// A single API item (struct, function, trait, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    /// Fully qualified path: `crate::module::Item` for Rust.
    pub path: String,
    /// What kind of item this is.
    pub kind: ItemKind,
    /// The signature in native syntax.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Documentation string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Visibility level.
    #[serde(default, skip_serializing_if = "Visibility::is_public")]
    pub visibility: Visibility,

    // === Struct/Enum specific ===
    /// Fields for structs/variants.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<Field>,
    /// Methods defined on this type.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub methods: Vec<Method>,
    /// Traits implemented (Rust) or interfaces extended (TS).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub traits: Vec<String>,
    /// Enum variants (only for kind == Enum).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<Variant>,

    // === Relationships ===
    /// Related items with their relationship type.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<Relation>,

    // === Lifecycle ===
    /// Version when this item was introduced.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    /// Version when this item was/will be removed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    /// Previous path if this item was moved/renamed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moved_from: Option<String>,
    /// Deprecation message if deprecated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<String>,
}

/// Universal item kinds across languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    /// Rust struct, Go struct, Python class, TS class.
    Struct,
    /// Rust/TS/Python/Go enum.
    Enum,
    /// Rust trait, Go interface, TS interface, Python Protocol.
    Trait,
    /// Standalone function.
    Function,
    /// Type alias.
    TypeAlias,
    /// Constant value.
    Constant,
    /// Module/namespace.
    Module,
    /// Macro (Rust-specific but useful to track).
    Macro,
}

/// Visibility levels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    #[default]
    Public,
    /// Rust: pub(crate), Python: leading underscore convention.
    Crate,
    /// Rust: pub(super) or private.
    Private,
}

impl Visibility {
    pub fn is_public(&self) -> bool {
        matches!(self, Visibility::Public)
    }
}

/// A struct field or similar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Field {
    pub name: String,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub ty: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    #[serde(default, skip_serializing_if = "Visibility::is_public")]
    pub visibility: Visibility,
}

/// A method on a type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Method {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    #[serde(default, skip_serializing_if = "Visibility::is_public")]
    pub visibility: Visibility,
}

/// An enum variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Variant {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Fields if this is a struct variant.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<Field>,
}

/// A relationship to another item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relation {
    /// Path to the related item.
    pub path: String,
    /// Type of relationship.
    pub kind: RelationKind,
}

/// Types of relationships between items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    /// Bevy: required component.
    RequiredComponent,
    /// This item replaces the target.
    Replaces,
    /// This item is replaced by the target.
    ReplacedBy,
    /// This item implements the target trait.
    Implements,
    /// This item extends/inherits from the target.
    Extends,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialize_item() {
        let item = Item {
            path: "bevy::light::DirectionalLight".into(),
            kind: ItemKind::Struct,
            signature: Some("pub struct DirectionalLight { ... }".into()),
            doc: Some("A directional light source.".into()),
            visibility: Visibility::Public,
            fields: vec![Field {
                name: "intensity".into(),
                ty: Some("f32".into()),
                doc: Some("Light intensity.".into()),
                visibility: Visibility::Public,
            }],
            methods: vec![],
            traits: vec!["Component".into(), "Default".into()],
            variants: vec![],
            related: vec![Relation {
                path: "Transform".into(),
                kind: RelationKind::RequiredComponent,
            }],
            since: Some("0.10.0".into()),
            until: None,
            moved_from: None,
            deprecated: None,
        };

        let json = serde_json::to_string_pretty(&item).unwrap();
        println!("{json}");

        // Round-trip test
        let parsed: Item = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.path, item.path);
    }
}
