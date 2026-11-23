//! Vite-style development tooling for JSNIR.
//!
//! Can be used either through the `jsnir` CLI (`jsnir dev`, `jsnir build`, `jsnir check`, `jsnir init`)
//! or embedded directly into a custom Rust application via [`DevConfig`], [`compile_entry`], and [`start_dev`].
pub mod assets;
pub mod compiler;
pub mod config;
pub mod shortcuts;
pub mod session;
pub mod typecheck;

pub use compiler::{CompiledBundle, JSNIR_DTS, ModuleGraph, SDK_TS, compile_entry, compiled_sdk};
pub use config::{AppConfig, AssetsConfig, DevConfig, DevSettings, TypecheckMode, parse_size};
pub use session::{DevSession, start_dev};
pub use typecheck::{AsyncTypechecker, TypecheckOutcome, find_tsc, run_typecheck};
