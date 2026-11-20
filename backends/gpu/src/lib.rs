//! Rendering and native surfaces for the JSNIR backends and examples.
mod app;
pub mod input;
pub mod renderer;
pub mod surface;
mod view;

pub use app::{Pusher, capture_at, open, open_with};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
