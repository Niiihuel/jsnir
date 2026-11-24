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
pub use config::{
    AppConfig, AssetsConfig, DevConfig, DevSettings, EngineKind, TypecheckMode, parse_size,
};
pub use session::{DevSession, start_dev};
pub use typecheck::{AsyncTypechecker, TypecheckOutcome, find_tsc, run_typecheck};

/// Instantiates the selected [`jsnir::engine::JsEngineFactory`].
///
/// `EngineKind::V8` requires compiling `jsnir-dev` with `--features engine-v8` so default builds
/// stay lightweight (~6.4 MiB stripped, zero V8 linked).
pub fn create_engine_factory(
    kind: EngineKind,
) -> Result<std::sync::Arc<dyn jsnir::engine::JsEngineFactory>, String> {
    match kind {
        EngineKind::QuickJs => Ok(jsnir::engine::default_factory()),
        EngineKind::V8 => {
            #[cfg(feature = "engine-v8")]
            {
                Ok(std::sync::Arc::new(jsnir_v8::V8Factory))
            }
            #[cfg(not(feature = "engine-v8"))]
            {
                Err(
                    "V8 engine is not compiled into this binary. Rebuild with:\n  cargo build --manifest-path crates/dev/Cargo.toml --features engine-v8"
                        .into(),
                )
            }
        }
    }
}
