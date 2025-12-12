//! Language-specific parsers.
//!
//! Each submodule implements parsing for a specific language ecosystem,
//! producing the universal `schema::Item` format.

pub mod rust;
pub mod typescript;

// Future:
// pub mod python;
// pub mod go;
