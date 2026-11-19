//! Rendering and native surfaces for the JSNIR backends and examples.
pub mod input;
pub mod renderer;
pub mod surface;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
