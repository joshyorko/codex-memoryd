//! codex-memoryd — a Codex-native portable memory provider.
//!
//! Layered per SPEC §3.2:
//! - [`protocol`] — wire request/response types + envelope
//! - [`domain`] — durable entities
//! - [`config`] — configuration resolution
//! - [`store`] — SQLite persistence + FTS5/LIKE search
//! - [`policy`] — safety, boundaries, classification
//! - [`ingest`] — local Codex memory import (chunk/classify/dedupe)
//! - [`recall`] — ranking, packing, citations
//! - [`server`] — axum HTTP transport
//! - [`status`] — status assembly
//! - [`export`] — safe record export
//! - [`metrics`] — counters
//! - [`error`] / [`ids`] — error model + identifiers

pub mod activation;
pub mod backup;
pub mod config;
pub mod conformance;
pub mod doctor;
pub mod domain;
pub mod dream;
pub mod error;
pub mod eval;
pub mod export;
pub mod git_import;
pub mod ids;
pub mod ingest;
pub mod mcp;
pub mod metrics;
pub mod perf;
pub mod policy;
pub mod proc_eval;
pub mod protocol;
pub mod recall;
pub mod retrieval_eval;
pub mod server;
pub mod service;
pub mod status;
pub mod store;

pub const PROVIDER_NAME: &str = "codex-memoryd";
pub const PROVIDER_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const API_VERSION: &str = "v1";
