//! File format adapters. Loaders and savers translate between external files
//! and the SQLite-backed dataset; they contain no terminal interaction.

pub(crate) mod csv_format;
pub mod loaders;
pub mod savers;
