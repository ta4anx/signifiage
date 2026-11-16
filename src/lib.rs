//! Signifiage library: model, services, UI and web remote.
//!
//! The binary is a thin dispatcher; every behaviour lives here so tests and
//! examples can drive it without a `main`.

rust_i18n::i18n!("locales", fallback = "en");

pub mod cli;
pub mod config;
pub mod i18n;
pub mod logging;
pub mod model;
pub mod player;
pub mod server;
pub mod services;
pub mod ui;

/// Crate version, shown in About and `--version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// GTK application id, also used as the desktop id.
pub const APP_ID: &str = "io.github.ta4anx.Signifiage";

/// GTK application id for standalone player windows (non-unique).
pub const PLAYER_APP_ID: &str = "io.github.ta4anx.Signifiage.Player";
