//! Language-specific parsers.
//!
//! Each submodule implements parsing for a specific language ecosystem,
//! producing the universal `schema::Item` format.

pub mod rust;

// Future:
// pub mod typescript;
// pub mod python;
// pub mod go;
