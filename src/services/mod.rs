//! Background services: queue, timers, spreadsheet import, media, capture, web.
//!
//! Services are plain Rust (no GTK) unless documented otherwise, so they can be
//! unit-tested without a display.

pub mod assets;
pub mod capture;
pub mod controller;
pub mod deck;
pub mod media;
pub mod portal;
pub mod queue;
pub mod queue_source;
pub mod remote_bridge;
pub mod schedule;
pub mod soundboard;
pub mod spreadsheet;
pub mod timers;
pub mod x11;
