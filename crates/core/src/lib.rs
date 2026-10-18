//! Search, download and export for hitomi.la
//!
//! Shared by the desktop app and the web version. Nothing in here depends on Tauri, the
//! app that uses the core creates a [`Context`] and calls the functions in [`api`].
pub mod api;
pub mod config;
pub mod context;
pub mod delete;
pub mod download_manager;
pub mod errors;
pub mod events;
pub mod export;
pub mod extensions;
pub mod hitomi;
pub mod hitomi_client;
pub mod logger;
pub mod pages;
pub mod queue;
pub mod subscriptions;
pub mod types;
pub mod utils;

pub use context::Context;
