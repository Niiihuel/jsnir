//! Engine-agnostic boundary between the JSNIR host protocol and pluggable JavaScript VMs.
//!
//! Default backend is QuickJS-ng (`engine-quickjs`, ~0.5 MB stripped, ~1.4 MB RSS, deterministic
//! reference counting, sub-millisecond AOT bytecode startup). Applications with heavy JS compute
//! or massive dynamic subtrees can plug in an external JIT engine (such as `jsnir-v8`) via
//! [`JsEngine`] and [`JsEngineFactory`] without changing `Host`, `Tree`, `contract`, or the TSX SDK.

#[cfg(feature = "engine-quickjs")]
pub mod quickjs;

pub use crate::contract::{CMD_WORDS, COLUMNS, decode_commands, project};
use crate::{
    ScriptBundle,
    tree::{Batch, Op, Replica, Tree},
};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, Mutex},
    time::Duration,
};

/// Host callback invoked synchronously by `jsnirRequest(json)` on the app thread.
pub type RequestHandler = Arc<Mutex<dyn FnMut(String) -> Result<String, String> + Send>>;

/// Resource safety limits enforced by a [`JsEngine`] on each VM instance / callback invocation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VmLimits {
    /// Maximum JS heap size in bytes (`None` = engine default).
    pub max_memory_bytes: Option<usize>,
    /// Maximum wall-clock duration allowed for a single JS mount or event callback (`None` = unlimited).
    pub max_call_duration: Option<Duration>,
}

/// Engine-agnostic configuration passed when loading or reloading a JS bundle.
#[derive(Clone, Default)]
pub struct VmConfig {
    /// JSON string exposed as `__jsnirArgs`.
    pub args: String,
    /// Monotonic epoch exposed as `__jsnirPlacementEpoch` (guards stale popover placements across reloads).
    pub placement_epoch: u64,
    /// Serialized signal state from the previous VM (`__jsnirPreviousState`) on live reload.
    pub previous_state: Option<String>,
    /// Optional synchronous `jsnirRequest(json)` host handler.
    pub request_handler: Option<RequestHandler>,
    /// Memory and per-call execution limits.
    pub limits: VmLimits,
}

/// Snapshot of VM heap memory reported by a [`JsEngine`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EngineMemory {
    pub used_bytes: usize,
    pub total_bytes: usize,
}

/// Capability flags advertised by a [`JsEngineFactory`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EngineCapabilities {
    /// Engine compiles hot JS functions to native machine code at runtime.
    pub jit: bool,
    /// Engine supports precompiled AOT bytecode (`JSNRBC01` or engine-specific code cache).
    pub bytecode_aot: bool,
    /// Engine can recycle its underlying isolate/runtime across [`JsEngine::reload_in_place`].
    pub in_place_reload: bool,
}

/// Engine-agnostic boundary between the JSNIR host protocol and a JavaScript VM.
/// Lives exclusively on the dedicated app thread; the UI thread never touches it.
pub trait JsEngine {
    /// Human-readable backend identifier (e.g. `"quickjs-ng"`, `"v8"`).
    fn name(&self) -> &'static str;

    /// Copies updated node columns from `view` into the shared `Int32Array` buffers visible as `jsnir.columns`.
    /// Called strictly while no JS is executing.
    fn sync(&mut self, view: &Tree, ids: Option<&[u16]>);

    /// Invokes `jsnirMount()` and returns the emitted operations.
    /// If JS throws, any partial operations emitted before the throw must be discarded.
    fn call_mount(&mut self) -> Result<Vec<Op>, String>;

    /// Invokes `jsnirEvent(callback, text[, guard])`.
    fn call_event(
        &mut self,
        callback: u16,
        text: &str,
        guard: Option<&str>,
    ) -> Result<Vec<Op>, String>;

    /// Invokes `jsnirDrag(callback, phase, x, y, sx, sy[, guard])` (or falls back to JSON `jsnirEvent`
    /// if the loaded bundle did not define `jsnirDrag`).
    #[allow(clippy::too_many_arguments)]
    fn call_drag(
        &mut self,
        callback: u16,
        phase: u8,
        x: f32,
        y: f32,
        sx: f32,
        sy: f32,
        dx: f32,
        dy: f32,
        guard: Option<&str>,
    ) -> Result<Vec<Op>, String>;

    /// Invokes `jsnirScroll(callback, top, height, client[, guard])` (or falls back to JSON `jsnirEvent`).
    fn call_scroll(
        &mut self,
        callback: u16,
        top: f32,
        height: f32,
        client: f32,
        guard: Option<&str>,
    ) -> Result<Vec<Op>, String>;

    /// Invokes `jsnirContextMenu(callback, x, y[, guard])` (or falls back to JSON `jsnirEvent`).
    fn call_context_menu(
        &mut self,
        callback: u16,
        x: f32,
        y: f32,
        guard: Option<&str>,
    ) -> Result<Vec<Op>, String>;

    /// Invokes `jsnirPlacement(text)`.
    fn call_placement(&mut self, text: &str) -> Result<Vec<Op>, String>;

    /// Invokes `jsnirVirtualList(delta)` and returns `(json_bindings_reply, emitted_ops)`.
    fn call_virtual_list(&mut self, delta: &str) -> Result<(String, Vec<Op>), String>;

    /// Invokes `jsnirReloadState()` to snapshot live JS signals before a hot reload.
    fn reload_state(&mut self) -> Option<String>;

    /// Optional live-reload fast path that recycles the underlying runtime/isolate in place
    /// (e.g., swapping only the `v8::Context` on an existing `v8::Isolate`).
    /// Returning `None` asks `Host` to fall back to [`JsEngineFactory::create`].
    /// If reloading fails (`Some(Err(..))`), the previous app state MUST remain intact and runnable.
    fn reload_in_place(
        &mut self,
        _bundle: &ScriptBundle,
        _replica: Rc<RefCell<Replica>>,
        _config: &VmConfig,
    ) -> Option<Result<Vec<Op>, String>> {
        None
    }

    /// Reports current VM heap usage when supported by the backend.
    fn memory_usage(&self) -> Option<EngineMemory> {
        None
    }
}

/// Thread-safe factory stored by `Host` to spawn and reload [`JsEngine`] instances on the app thread.
pub trait JsEngineFactory: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> EngineCapabilities;
    fn create(
        &self,
        bundle: &ScriptBundle,
        replica: Rc<RefCell<Replica>>,
        config: &VmConfig,
    ) -> Result<Box<dyn JsEngine>, String>;
}

/// Returns the default lightweight JS engine factory (`QuickJsFactory`).
#[cfg(feature = "engine-quickjs")]
pub fn default_factory() -> Arc<dyn JsEngineFactory> {
    Arc::new(quickjs::QuickJsFactory)
}

/// Parses a legacy JSON `jsnir.batch(json)` payload into `out`.
pub fn decode_json_batch(json: &str, out: &mut Vec<Op>) -> Result<(), String> {
    let batch: Batch = serde_json::from_str(json).map_err(|e| format!("invalid batch: {e}"))?;
    out.extend(batch.ops);
    Ok(())
}

/// JSON fallback payload for legacy bundles that do not export `jsnirDrag`.
#[allow(clippy::too_many_arguments)]
pub fn fallback_drag_json(
    phase: u8,
    x: f32,
    y: f32,
    sx: f32,
    sy: f32,
    dx: f32,
    dy: f32,
) -> String {
    let phase_str = match phase {
        0 => "start",
        1 => "move",
        _ => "end",
    };
    serde_json::json!({
        "phase": phase_str,
        "x": x,
        "y": y,
        "startX": sx,
        "startY": sy,
        "dx": dx,
        "dy": dy,
    })
    .to_string()
}

/// JSON fallback payload for legacy bundles that do not export `jsnirScroll`.
pub fn fallback_scroll_json(top: f32, height: f32, client: f32) -> String {
    format!(r#"{{"top":{top},"height":{height},"client":{client}}}"#)
}

/// JSON fallback payload for legacy bundles that do not export `jsnirContextMenu`.
pub fn fallback_context_menu_json(x: f32, y: f32) -> String {
    serde_json::json!({ "x": x, "y": y }).to_string()
}
