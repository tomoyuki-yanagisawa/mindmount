//! Operations shared by the CLI, MCP, and web adapters.
//!
//! These functions orchestrate config, storage, indexing, and the database.
//! They return data and errors but never print.

pub mod index;
pub mod mount;
