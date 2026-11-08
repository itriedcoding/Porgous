//! Porgous core: game server management, authentication, containers and updates.

pub mod agent;
pub mod api;
pub mod app;
pub mod audit;
pub mod auth;
pub mod backup;
pub mod config;
pub mod console;
pub mod crypto;
pub mod db;
pub mod eggs;
pub mod error;
pub mod files;
pub mod files_service;
pub mod models;
pub mod nodes;
pub mod ratelimit;
pub mod runtime;
pub mod scheduler;
pub mod schema;
pub mod server_ops;
pub mod state;
pub mod updater;
pub mod util;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
