//! Optional V8 (TurboFan/Maglev) JIT JavaScript engine backend for JSNIR.
//!
//! Designed as a standalone opt-in crate so default JSNIR builds stay ~0.5 MB (QuickJS-ng) while
//! heavy-compute or large dynamic desktop applications can swap in V8 via [`V8Factory`] without
//! changing `Host`, `Tree`, `contract`, or the TSX SDK.
//!
//! Architecture highlights:
//! - **Zero-copy shared memory (`v8::BackingStore`)** for both `jsnir.columns` (`Int32Array`) and
//!   the binary command buffer (`jsnir.cmd` / `jsnir.cmdF32`), compiled inline by TurboFan/Maglev.
//! - **V8 Fast API Calls** (`v8::fast_api::CFunction`) for `jsnir.set(id, value)` and
//!   `jsnir.commit(words)`, allowing TurboFan to invoke Rust directly without JS value boxing.
//! - **Isolate recycling on hot reload** (`JsEngine::reload_in_place`): swaps only the
//!   `v8::Context` inside the existing `v8::OwnedIsolate`, with atomic rollback on load/mount errors.
//! - **Resource safety (`VmLimits`)**: configurable V8 heap ceiling and watchdog-based per-call
//!   execution termination (`IsolateHandle::terminate_execution`).

use jsnir::{
    ScriptBundle,
    engine::{
        CMD_WORDS, COLUMNS, EngineCapabilities, EngineMemory, JsEngine, JsEngineFactory,
        RequestHandler, VmConfig, decode_commands, decode_json_batch, fallback_context_menu_json,
        fallback_drag_json, fallback_scroll_json, project,
    },
    tree::{Op, Replica, Tree},
};
use std::{
    cell::RefCell,
    ffi::c_void,
    rc::Rc,
    sync::{
        Arc, Condvar, Mutex, Once,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

static V8_INIT: Once = Once::new();

fn ensure_v8_initialized() {
    V8_INIT.call_once(|| {
        let platform = v8::new_default_platform(0, false).make_shared();
        v8::V8::initialize_platform(platform);
        v8::V8::initialize();
    });
}

/// Thread-safe factory producing [`V8Engine`] instances for `Host::try_new_with_engine`.
#[derive(Clone, Copy, Debug, Default)]
pub struct V8Factory;

impl JsEngineFactory for V8Factory {
    fn name(&self) -> &'static str {
        "v8"
    }

    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities {
            jit: true,
            bytecode_aot: false,
            in_place_reload: true,
        }
    }

    fn create(
        &self,
        bundle: &ScriptBundle,
        replica: Rc<RefCell<Replica>>,
        config: &VmConfig,
    ) -> Result<Box<dyn JsEngine>, String> {
        Ok(Box::new(V8Engine::try_load(bundle, replica, config)?))
    }
}

/// Per-context host bridge state shared between V8 native callbacks and [`V8Engine`].
struct HostState {
    ops: Vec<Op>,
    replica: Rc<RefCell<Replica>>,
    request_handler: Option<RequestHandler>,
    /// Cleared in `ContextState::drop` so all `BackingStore` refs are released before `Isolate::drop`.
    cmd_store: Option<v8::SharedRef<v8::BackingStore>>,
}

/// Shared `Int32Array` column backing stores visible to JS as `jsnir.columns.*`.
struct V8Columns {
    stores: Vec<v8::SharedRef<v8::BackingStore>>,
    len: usize,
}

impl V8Columns {
    fn install<'s>(
        scope: &mut v8::PinScope<'s, '_>,
        jsnir: v8::Local<'s, v8::Object>,
    ) -> Result<Self, String> {
        let col_obj = v8::Object::new(scope);
        let key = v8::String::new(scope, "columns").ok_or("out of memory")?;
        jsnir.set(scope, key.into(), col_obj.into());
        let mut columns = Self {
            stores: Vec::with_capacity(COLUMNS.len()),
            len: 0,
        };
        columns.rebuild(scope, col_obj, &Tree::default(), 64)?;
        Ok(columns)
    }

    fn rebuild<'s>(
        &mut self,
        scope: &mut v8::PinScope<'s, '_>,
        col_obj: v8::Local<'s, v8::Object>,
        view: &Tree,
        len: usize,
    ) -> Result<(), String> {
        self.stores.clear();
        let byte_len = len * 4;
        for (k, name) in COLUMNS.iter().enumerate() {
            let store = v8::ArrayBuffer::new_backing_store(scope, byte_len).make_shared();
            if let Some(ptr) = store.data() {
                let dst = ptr.as_ptr() as *mut i32;
                for id in 0..len {
                    let row = project(view, id as u16);
                    // SAFETY: `store` holds `len * 4` bytes; `id < len`; JS is not running.
                    unsafe { dst.add(id).write(row[k]) };
                }
            }
            let ab = v8::ArrayBuffer::with_backing_store(scope, &store);
            let typed = v8::Int32Array::new(scope, ab, 0, len)
                .ok_or_else(|| format!("failed to allocate Int32Array for column {name}"))?;
            let key = v8::String::new(scope, name).ok_or("out of memory")?;
            col_obj.set(scope, key.into(), typed.into());
            self.stores.push(store);
        }
        self.len = len;
        Ok(())
    }

    /// Writes updated columns directly into the shared `BackingStore` pointers without entering V8.
    /// Returns `false` if the columns must grow to accommodate `view.nodes.end()`.
    fn sync_in_place(&self, view: &Tree, ids: Option<&[u16]>) -> bool {
        if view.nodes.end() > self.len {
            return false;
        }
        if let Some(ids) = ids {
            for &id in ids {
                let idx = id as usize;
                assert!(idx < self.len, "id outside the column");
                let row = project(view, id);
                for (k, store) in self.stores.iter().enumerate() {
                    if let Some(ptr) = store.data() {
                        // SAFETY: `store` owns `self.len * 4` bytes; JS is not running; `idx < self.len`.
                        unsafe { (ptr.as_ptr() as *mut i32).add(idx).write(row[k]) };
                    }
                }
            }
        } else {
            for idx in 0..self.len {
                let row = project(view, idx as u16);
                for (k, store) in self.stores.iter().enumerate() {
                    if let Some(ptr) = store.data() {
                        // SAFETY: same as above.
                        unsafe { (ptr.as_ptr() as *mut i32).add(idx).write(row[k]) };
                    }
                }
            }
        }
        true
    }
}

/// Watchdog thread enforcing `VmLimits::max_call_duration` via `IsolateHandle::terminate_execution`.
/// Only spawned when `max_call_duration` is `Some(_)`; zero overhead otherwise.
struct WatchState {
    deadline: Option<Instant>,
    shutdown: bool,
}

struct Watchdog {
    duration: Duration,
    shared: Arc<(Mutex<WatchState>, Condvar)>,
    terminated: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Watchdog {
    fn new(handle: v8::IsolateHandle, duration: Duration) -> Self {
        let shared = Arc::new((
            Mutex::new(WatchState {
                deadline: None,
                shutdown: false,
            }),
            Condvar::new(),
        ));
        let terminated = Arc::new(AtomicBool::new(false));
        let shared_bg = shared.clone();
        let terminated_bg = terminated.clone();
        let thread = thread::spawn(move || {
            let (lock, cv) = &*shared_bg;
            let mut st = lock.lock().expect("watchdog lock");
            while !st.shutdown {
                match st.deadline {
                    None => {
                        st = cv.wait(st).expect("watchdog wait");
                    }
                    Some(dl) => {
                        let now = Instant::now();
                        if now >= dl {
                            terminated_bg.store(true, Ordering::Release);
                            handle.terminate_execution();
                            st.deadline = None;
                        } else {
                            let (next, _) = cv.wait_timeout(st, dl - now).expect("watchdog wait");
                            st = next;
                        }
                    }
                }
            }
        });
        Self {
            duration,
            shared,
            terminated,
            thread: Some(thread),
        }
    }

    fn arm(&self) {
        self.terminated.store(false, Ordering::Relaxed);
        let (lock, cv) = &*self.shared;
        let mut st = lock.lock().expect("watchdog lock");
        st.deadline = Some(Instant::now() + self.duration);
        cv.notify_one();
    }

    fn disarm(&self) -> bool {
        let (lock, cv) = &*self.shared;
        let mut st = lock.lock().expect("watchdog lock");
        st.deadline = None;
        cv.notify_one();
        self.terminated.swap(false, Ordering::AcqRel)
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        {
            let (lock, cv) = &*self.shared;
            let mut st = lock.lock().expect("watchdog lock");
            st.deadline = None;
            st.shutdown = true;
            cv.notify_one();
        }
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

/// Active JS context + cached entry-point functions for one loaded bundle inside a `V8Engine`.
struct ContextState {
    context: v8::Global<v8::Context>,
    columns: V8Columns,
    state: Rc<RefCell<HostState>>,
    fn_mount: Option<v8::Global<v8::Function>>,
    fn_event: Option<v8::Global<v8::Function>>,
    fn_drag: Option<v8::Global<v8::Function>>,
    fn_scroll: Option<v8::Global<v8::Function>>,
    fn_context_menu: Option<v8::Global<v8::Function>>,
    fn_placement: Option<v8::Global<v8::Function>>,
    fn_virtual_list: Option<v8::Global<v8::Function>>,
    fn_reload_state: Option<v8::Global<v8::Function>>,
}

impl Drop for ContextState {
    fn drop(&mut self) {
        let mut st = self.state.borrow_mut();
        st.cmd_store = None;
        st.request_handler = None;
        st.ops.clear();
    }
}

/// Embedded V8 JIT engine implementing [`JsEngine`].
/// Created and used exclusively on the app thread.
pub struct V8Engine {
    // Field order is load-bearing: `watchdog` and `active` (all `Global`s + `BackingStore` refs)
    // MUST drop before `isolate`.
    watchdog: Option<Watchdog>,
    active: ContextState,
    isolate: v8::OwnedIsolate,
}

fn format_exception(
    tc: &mut v8::PinnedRef<'_, v8::TryCatch<'_, '_, v8::HandleScope<'_>>>,
    terminated: bool,
) -> String {
    if terminated || tc.has_terminated() {
        return "execution timed out (interrupted by VmLimits)".to_string();
    }
    let Some(exception) = tc.exception() else {
        return "unknown V8 exception".to_string();
    };
    let msg = exception
        .to_string(tc)
        .map(|s| s.to_rust_string_lossy(tc))
        .unwrap_or_else(|| "exception".to_string());
    if let Some(message) = tc.message() {
        let line = message.get_line_number(tc).unwrap_or(0);
        if line > 0 {
            return format!("{msg} (line {line})");
        }
    }
    msg
}

#[inline]
unsafe fn state_from_external<'a>(val: v8::Local<v8::Value>) -> Option<&'a RefCell<HostState>> {
    let ext = v8::Local::<v8::External>::try_from(val).ok()?;
    let ptr = ext.value() as *const RefCell<HostState>;
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { &*ptr })
    }
}

fn throw_type_error(scope: &mut v8::PinScope<'_, '_>, message: &str) {
    if let Some(msg) = v8::String::new(scope, message) {
        let err = v8::Exception::type_error(scope, msg);
        scope.throw_exception(err);
    }
}

// ---------- V8 Native Callbacks + Fast API Callbacks ----------

extern "C" fn fast_set(
    _recv: v8::Local<v8::Object>,
    id: u32,
    value: u32,
    options: &v8::fast_api::FastApiCallbackOptions,
) {
    let Ok(id) = u16::try_from(id) else {
        v8::callback_scope!(unsafe let scope, options);
        throw_type_error(scope, "jsnir.set: id out of u16 range");
        return;
    };
    let Ok(value) = u16::try_from(value) else {
        v8::callback_scope!(unsafe let scope, options);
        throw_type_error(scope, "jsnir.set: value out of u16 range");
        return;
    };
    if let Some(state) = unsafe { state_from_external(options.data) } {
        state.borrow_mut().ops.push(Op::Set { id, value });
    }
}

const FAST_SET: v8::fast_api::CFunction = v8::fast_api::CFunction::new(
    fast_set as _,
    &v8::fast_api::CFunctionInfo::new(
        v8::fast_api::Type::Void.as_info(),
        &[
            v8::fast_api::Type::V8Value.as_info(),
            v8::fast_api::Type::Uint32.as_info(),
            v8::fast_api::Type::Uint32.as_info(),
            v8::fast_api::Type::CallbackOptions.as_info(),
        ],
        v8::fast_api::Int64Representation::Number,
    ),
);

fn slow_set(
    scope: &mut v8::PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let Some(id_num) = args.get(0).number_value(scope) else {
        throw_type_error(scope, "jsnir.set: expected numeric id");
        return;
    };
    let Some(val_num) = args.get(1).number_value(scope) else {
        throw_type_error(scope, "jsnir.set: expected numeric value");
        return;
    };
    if !id_num.is_finite()
        || id_num.fract() != 0.0
        || !(0.0..=65535.0).contains(&id_num)
        || !val_num.is_finite()
        || val_num.fract() != 0.0
        || !(0.0..=65535.0).contains(&val_num)
    {
        throw_type_error(scope, "jsnir.set: operand out of u16 range");
        return;
    }
    if let Some(state) = unsafe { state_from_external(args.data()) } {
        state.borrow_mut().ops.push(Op::Set {
            id: id_num as u16,
            value: val_num as u16,
        });
    }
}

extern "C" fn fast_commit(
    _recv: v8::Local<v8::Object>,
    words: u32,
    options: &v8::fast_api::FastApiCallbackOptions,
) {
    let Some(state) = (unsafe { state_from_external(options.data) }) else {
        return;
    };
    let res = {
        let mut st = state.borrow_mut();
        let HostState { ops, cmd_store, .. } = &mut *st;
        let words = words as usize;
        if words > CMD_WORDS {
            Err(format!(
                "command buffer length {words} exceeds capacity {CMD_WORDS}"
            ))
        } else if let Some(ptr) = cmd_store.as_ref().and_then(|s| s.data()) {
            let slice = unsafe { std::slice::from_raw_parts(ptr.as_ptr() as *const u32, words) };
            decode_commands(slice, &mut [], ops)
        } else {
            Err("command buffer detached".to_string())
        }
    };
    if let Err(err) = res {
        v8::callback_scope!(unsafe let scope, options);
        throw_type_error(scope, &format!("invalid cmd: {err}"));
    }
}

const FAST_COMMIT: v8::fast_api::CFunction = v8::fast_api::CFunction::new(
    fast_commit as _,
    &v8::fast_api::CFunctionInfo::new(
        v8::fast_api::Type::Void.as_info(),
        &[
            v8::fast_api::Type::V8Value.as_info(),
            v8::fast_api::Type::Uint32.as_info(),
            v8::fast_api::Type::CallbackOptions.as_info(),
        ],
        v8::fast_api::Int64Representation::Number,
    ),
);

fn slow_commit(
    scope: &mut v8::PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let Some(words) = args.get(0).uint32_value(scope) else {
        throw_type_error(scope, "jsnir.commit: expected u32 word count");
        return;
    };
    let Some(state) = (unsafe { state_from_external(args.data()) }) else {
        return;
    };
    let res = {
        let mut st = state.borrow_mut();
        let HostState { ops, cmd_store, .. } = &mut *st;
        let words = words as usize;
        if words > CMD_WORDS {
            Err(format!(
                "command buffer length {words} exceeds capacity {CMD_WORDS}"
            ))
        } else if let Some(ptr) = cmd_store.as_ref().and_then(|s| s.data()) {
            let slice = unsafe { std::slice::from_raw_parts(ptr.as_ptr() as *const u32, words) };
            decode_commands(slice, &mut [], ops)
        } else {
            Err("command buffer detached".to_string())
        }
    };
    if let Err(err) = res {
        throw_type_error(scope, &format!("invalid cmd: {err}"));
    }
}

fn cb_commit_strings(
    scope: &mut v8::PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let Some(words) = args.get(0).uint32_value(scope) else {
        throw_type_error(scope, "jsnir.commitStrings: expected u32 word count");
        return;
    };
    let Ok(arr) = v8::Local::<v8::Array>::try_from(args.get(1)) else {
        throw_type_error(scope, "jsnir.commitStrings: expected string array");
        return;
    };
    let len = arr.length();
    let mut strings = Vec::with_capacity(len as usize);
    for idx in 0..len {
        let elem = arr.get_index(scope, idx);
        let text = elem
            .and_then(|v| v.to_string(scope))
            .map(|s| s.to_rust_string_lossy(scope))
            .unwrap_or_default();
        strings.push(text);
    }
    let Some(state) = (unsafe { state_from_external(args.data()) }) else {
        return;
    };
    let res = {
        let mut st = state.borrow_mut();
        let HostState { ops, cmd_store, .. } = &mut *st;
        let words = words as usize;
        if words > CMD_WORDS {
            Err(format!(
                "command buffer length {words} exceeds capacity {CMD_WORDS}"
            ))
        } else if let Some(ptr) = cmd_store.as_ref().and_then(|s| s.data()) {
            let slice = unsafe { std::slice::from_raw_parts(ptr.as_ptr() as *const u32, words) };
            decode_commands(slice, &mut strings, ops)
        } else {
            Err("command buffer detached".to_string())
        }
    };
    if let Err(err) = res {
        throw_type_error(scope, &format!("invalid cmd: {err}"));
    }
}

fn cb_batch(
    scope: &mut v8::PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let Some(json_str) = args.get(0).to_string(scope) else {
        throw_type_error(scope, "jsnir.batch: expected string");
        return;
    };
    let json = json_str.to_rust_string_lossy(scope);
    let Some(state) = (unsafe { state_from_external(args.data()) }) else {
        return;
    };
    if let Err(err) = decode_json_batch(&json, &mut state.borrow_mut().ops) {
        throw_type_error(scope, &err);
    }
}

fn cb_report(
    scope: &mut v8::PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let message = args
        .get(0)
        .to_string(scope)
        .map(|s| s.to_rust_string_lossy(scope))
        .unwrap_or_default();
    eprintln!("app error, the event keeps its changes: {message}");
}

fn cb_set_text(
    scope: &mut v8::PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    _rv: v8::ReturnValue,
) {
    let Some(id) = args.get(0).uint32_value(scope).and_then(|v| u16::try_from(v).ok()) else {
        throw_type_error(scope, "jsnir.setText: id out of u16 range");
        return;
    };
    let text = args
        .get(1)
        .to_string(scope)
        .map(|s| s.to_rust_string_lossy(scope))
        .unwrap_or_default();
    if let Some(state) = unsafe { state_from_external(args.data()) } {
        state.borrow_mut().ops.push(Op::SetText { id, text });
    }
}

fn cb_read_text(
    scope: &mut v8::PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let Some(id) = args.get(0).uint32_value(scope).and_then(|v| u16::try_from(v).ok()) else {
        rv.set_undefined();
        return;
    };
    let Some(state) = (unsafe { state_from_external(args.data()) }) else {
        rv.set_undefined();
        return;
    };
    let text = state
        .borrow()
        .replica
        .borrow()
        .view
        .text(id)
        .map(str::to_owned);
    match text {
        Some(s) => {
            if let Some(v8s) = v8::String::new(scope, &s) {
                rv.set(v8s.into());
            } else {
                rv.set_undefined();
            }
        }
        None => rv.set_undefined(),
    }
}

fn cb_request(
    scope: &mut v8::PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    mut rv: v8::ReturnValue,
) {
    let json = args
        .get(0)
        .to_string(scope)
        .map(|s| s.to_rust_string_lossy(scope))
        .unwrap_or_default();
    let Some(state) = (unsafe { state_from_external(args.data()) }) else {
        throw_type_error(scope, "jsnirRequest: host state unavailable");
        return;
    };
    let handler = state.borrow().request_handler.clone();
    let Some(handler) = handler else {
        throw_type_error(scope, "jsnirRequest: no request handler installed");
        return;
    };
    let result = {
        let mut h = handler.lock().expect("request handler");
        (*h)(json)
    };
    match result {
        Ok(reply) => {
            if let Some(v8s) = v8::String::new(scope, &reply) {
                rv.set(v8s.into());
            }
        }
        Err(err) => {
            if let Some(msg) = v8::String::new(scope, &err) {
                let exc = v8::Exception::error(scope, msg);
                scope.throw_exception(exc);
            }
        }
    }
}

fn bind_fn<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    target: v8::Local<'s, v8::Object>,
    ext: v8::Local<'s, v8::External>,
    name: &str,
    cb: impl v8::MapFnTo<v8::FunctionCallback>,
) -> Result<(), String> {
    let f = v8::Function::builder(cb)
        .data(ext.into())
        .build(scope)
        .ok_or_else(|| format!("failed to build {name}"))?;
    let k = v8::String::new(scope, name).ok_or("oom")?;
    target.set(scope, k.into(), f.into());
    Ok(())
}

fn lookup_fn<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    global: v8::Local<'s, v8::Object>,
    name: &str,
) -> Option<v8::Global<v8::Function>> {
    let key = v8::String::new(scope, name)?;
    let val = global.get(scope, key.into())?;
    let func = v8::Local::<v8::Function>::try_from(val).ok()?;
    Some(v8::Global::new(scope, func))
}

fn build_context(
    isolate: &mut v8::OwnedIsolate,
    watchdog: Option<&Watchdog>,
    bundle: &ScriptBundle,
    replica: Rc<RefCell<Replica>>,
    config: &VmConfig,
) -> Result<ContextState, String> {
    let js = match bundle {
        ScriptBundle::Source(src) => src.as_str(),
        ScriptBundle::Bytecode(_) => {
            return Err(
                "V8 engine requires JavaScript source (received QuickJS JSNRBC01 bytecode)".into(),
            );
        }
    };

    let mut eval_terminated = false;
    let result = (|| -> Result<ContextState, String> {
        v8::scope!(let handle_scope, isolate);
        let context = v8::Context::new(handle_scope, Default::default());
        let scope = &mut v8::ContextScope::new(handle_scope, context);

        // Shared 64 KiB binary command buffer (`jsnir.cmd` + `jsnir.cmdF32`).
        let cmd_store = v8::ArrayBuffer::new_backing_store(scope, CMD_WORDS * 4).make_shared();
        let state = Rc::new(RefCell::new(HostState {
            ops: Vec::new(),
            replica,
            request_handler: config.request_handler.clone(),
            cmd_store: Some(cmd_store.clone()),
        }));
        // Tie `HostState` lifetime to the V8 Context so External pointers stay valid until Context GC.
        context.set_slot(state.clone());
        let ext = v8::External::new(scope, Rc::as_ptr(&state) as *mut c_void);

        let jsnir = v8::Object::new(scope);

        // Fast-API accelerated functions (`jsnir.set`, `jsnir.commit`).
        let set_tmpl = v8::FunctionTemplate::builder(slow_set)
            .data(ext.into())
            .build_fast(scope, &[FAST_SET]);
        let set_fn = set_tmpl
            .get_function(scope)
            .ok_or("failed to build jsnir.set")?;
        let k = v8::String::new(scope, "set").ok_or("oom")?;
        jsnir.set(scope, k.into(), set_fn.into());

        let commit_tmpl = v8::FunctionTemplate::builder(slow_commit)
            .data(ext.into())
            .build_fast(scope, &[FAST_COMMIT]);
        let commit_fn = commit_tmpl
            .get_function(scope)
            .ok_or("failed to build jsnir.commit")?;
        let k = v8::String::new(scope, "commit").ok_or("oom")?;
        jsnir.set(scope, k.into(), commit_fn.into());

        // Standard callbacks (`commitStrings`, `batch`, `report`, `setText`, `readText`).
        bind_fn(scope, jsnir, ext, "commitStrings", cb_commit_strings)?;
        bind_fn(scope, jsnir, ext, "batch", cb_batch)?;
        bind_fn(scope, jsnir, ext, "report", cb_report)?;
        bind_fn(scope, jsnir, ext, "setText", cb_set_text)?;
        bind_fn(scope, jsnir, ext, "readText", cb_read_text)?;

        // Install `jsnir.cmd` (Uint32Array) and `jsnir.cmdF32` (Float32Array) over the SAME ArrayBuffer.
        let cmd_ab = v8::ArrayBuffer::with_backing_store(scope, &cmd_store);
        let cmd_u32 = v8::Uint32Array::new(scope, cmd_ab, 0, CMD_WORDS)
            .ok_or("failed to build jsnir.cmd")?;
        let cmd_f32 = v8::Float32Array::new(scope, cmd_ab, 0, CMD_WORDS)
            .ok_or("failed to build jsnir.cmdF32")?;
        let k = v8::String::new(scope, "cmd").ok_or("oom")?;
        jsnir.set(scope, k.into(), cmd_u32.into());
        let k = v8::String::new(scope, "cmdF32").ok_or("oom")?;
        jsnir.set(scope, k.into(), cmd_f32.into());

        // Install `jsnir.columns.*`.
        let columns = V8Columns::install(scope, jsnir)?;

        // Populate global bindings before evaluating user script.
        let global = context.global(scope);
        let k = v8::String::new(scope, "jsnir").ok_or("oom")?;
        global.set(scope, k.into(), jsnir.into());

        let k = v8::String::new(scope, "__jsnirArgs").ok_or("oom")?;
        let v = v8::String::new(scope, &config.args).ok_or("oom")?;
        global.set(scope, k.into(), v.into());

        if let Some(prev) = config.previous_state.as_deref() {
            let k = v8::String::new(scope, "__jsnirPreviousState").ok_or("oom")?;
            let v = v8::String::new(scope, prev).ok_or("oom")?;
            global.set(scope, k.into(), v.into());
        }
        if config.placement_epoch != 0 {
            let k = v8::String::new(scope, "__jsnirPlacementEpoch").ok_or("oom")?;
            let ep = config.placement_epoch.to_string();
            let v = v8::String::new(scope, &ep).ok_or("oom")?;
            global.set(scope, k.into(), v.into());
        }
        if config.request_handler.is_some() {
            bind_fn(scope, global, ext, "jsnirRequest", cb_request)?;
        }

        // Compile and run the user script inside a TryCatch, timing ONLY user JS evaluation
        // (never V8's internal Context allocation and native bridge wiring).
        {
            if let Some(wd) = watchdog {
                wd.arm();
            }
            let eval_res = {
                v8::tc_scope!(let tc, scope);
                let source = v8::String::new(tc, js).ok_or("script too large for V8 string")?;
                match v8::Script::compile(tc, source, None) {
                    None => Err(format_exception(tc, false)),
                    Some(script) => {
                        if script.run(tc).is_none() {
                            Err(format_exception(tc, false))
                        } else {
                            Ok(())
                        }
                    }
                }
            };
            eval_terminated = watchdog.is_some_and(Watchdog::disarm);
            eval_res?;
        }

        let fn_mount = lookup_fn(scope, global, "jsnirMount");
        let fn_event = lookup_fn(scope, global, "jsnirEvent");
        let fn_drag = lookup_fn(scope, global, "jsnirDrag");
        let fn_scroll = lookup_fn(scope, global, "jsnirScroll");
        let fn_context_menu = lookup_fn(scope, global, "jsnirContextMenu");
        let fn_placement = lookup_fn(scope, global, "jsnirPlacement");
        let fn_virtual_list = lookup_fn(scope, global, "jsnirVirtualList");
        let fn_reload_state = lookup_fn(scope, global, "jsnirReloadState");
        let context_global = v8::Global::new(scope, context);

        Ok(ContextState {
            context: context_global,
            columns,
            state,
            fn_mount,
            fn_event,
            fn_drag,
            fn_scroll,
            fn_context_menu,
            fn_placement,
            fn_virtual_list,
            fn_reload_state,
        })
    })();

    if eval_terminated {
        isolate.cancel_terminate_execution();
        if result.is_ok() {
            return Err("execution timed out (interrupted by VmLimits)".to_string());
        }
    }
    result
}

impl V8Engine {
    pub fn try_load(
        bundle: &ScriptBundle,
        replica: Rc<RefCell<Replica>>,
        config: &VmConfig,
    ) -> Result<Self, String> {
        ensure_v8_initialized();
        let mut params = v8::CreateParams::default();
        if let Some(max_mem) = config.limits.max_memory_bytes {
            // V8 requires enough headroom for young + old generation initialization.
            params = params.heap_limits(0, max_mem.max(4 * 1024 * 1024));
        }
        let mut isolate = v8::Isolate::new(params);
        let watchdog = config
            .limits
            .max_call_duration
            .map(|dur| Watchdog::new(isolate.thread_safe_handle(), dur));
        let active = build_context(&mut isolate, watchdog.as_ref(), bundle, replica, config)?;
        Ok(Self {
            watchdog,
            active,
            isolate,
        })
    }

    fn call_ops<F>(
        isolate: &mut v8::OwnedIsolate,
        watchdog: Option<&Watchdog>,
        ctx_state: &ContextState,
        func: Option<&v8::Global<v8::Function>>,
        name: &str,
        call: F,
    ) -> Result<Vec<Op>, String>
    where
        F: for<'s, 'i> FnOnce(
            &mut v8::PinScope<'s, 'i>,
            v8::Local<'s, v8::Function>,
            v8::Local<'s, v8::Value>,
        ) -> Option<v8::Local<'s, v8::Value>>,
    {
        let Some(func_global) = func else {
            return Err(format!("{name}: function is not defined"));
        };
        ctx_state.state.borrow_mut().ops.clear();
        if let Some(wd) = watchdog {
            wd.arm();
        }
        let call_res = {
            v8::scope!(let handle_scope, isolate);
            let context = v8::Local::new(handle_scope, &ctx_state.context);
            let scope = &mut v8::ContextScope::new(handle_scope, context);
            v8::tc_scope!(let tc, scope);
            let f = v8::Local::new(tc, func_global);
            let recv: v8::Local<v8::Value> = v8::undefined(tc).into();
            if call(tc, f, recv).is_none() {
                Err(format!("{name}: {}", format_exception(tc, false)))
            } else {
                Ok(())
            }
        };
        let terminated = watchdog.is_some_and(Watchdog::disarm);
        if terminated {
            isolate.cancel_terminate_execution();
        }
        let ops = std::mem::take(&mut ctx_state.state.borrow_mut().ops);
        if terminated {
            return Err(format!(
                "{name}: execution timed out (interrupted by VmLimits)"
            ));
        }
        call_res.map(|()| ops)
    }

    fn call_string<F>(
        isolate: &mut v8::OwnedIsolate,
        watchdog: Option<&Watchdog>,
        ctx_state: &ContextState,
        func: Option<&v8::Global<v8::Function>>,
        name: &str,
        call: F,
    ) -> Result<(String, Vec<Op>), String>
    where
        F: for<'s, 'i> FnOnce(
            &mut v8::PinScope<'s, 'i>,
            v8::Local<'s, v8::Function>,
            v8::Local<'s, v8::Value>,
        ) -> Option<v8::Local<'s, v8::Value>>,
    {
        let Some(func_global) = func else {
            return Err(format!("{name}: function is not defined"));
        };
        ctx_state.state.borrow_mut().ops.clear();
        if let Some(wd) = watchdog {
            wd.arm();
        }
        let call_res = {
            v8::scope!(let handle_scope, isolate);
            let context = v8::Local::new(handle_scope, &ctx_state.context);
            let scope = &mut v8::ContextScope::new(handle_scope, context);
            v8::tc_scope!(let tc, scope);
            let f = v8::Local::new(tc, func_global);
            let recv: v8::Local<v8::Value> = v8::undefined(tc).into();
            match call(tc, f, recv) {
                Some(val) => {
                    let s = val
                        .to_string(tc)
                        .map(|s| s.to_rust_string_lossy(tc))
                        .unwrap_or_default();
                    Ok(s)
                }
                None => Err(format!("{name}: {}", format_exception(tc, false))),
            }
        };
        let terminated = watchdog.is_some_and(Watchdog::disarm);
        if terminated {
            isolate.cancel_terminate_execution();
        }
        let ops = std::mem::take(&mut ctx_state.state.borrow_mut().ops);
        if terminated {
            return Err(format!(
                "{name}: execution timed out (interrupted by VmLimits)"
            ));
        }
        call_res.map(|s| (s, ops))
    }
}

impl JsEngine for V8Engine {
    fn name(&self) -> &'static str {
        "v8"
    }

    fn sync(&mut self, view: &Tree, ids: Option<&[u16]>) {
        // Fast path: zero V8 HandleScope entry, direct raw pointer writes into shared BackingStores.
        if self.active.columns.sync_in_place(view, ids) {
            return;
        }
        let len = view.nodes.end().max(self.active.columns.len * 2);
        v8::scope!(let handle_scope, &mut self.isolate);
        let context = v8::Local::new(handle_scope, &self.active.context);
        let scope = &mut v8::ContextScope::new(handle_scope, context);
        let global = context.global(scope);
        let jsnir_key = v8::String::new(scope, "jsnir").expect("oom");
        let jsnir_obj: v8::Local<v8::Object> = global
            .get(scope, jsnir_key.into())
            .and_then(|v| v.try_into().ok())
            .expect("jsnir global");
        let cols_key = v8::String::new(scope, "columns").expect("oom");
        let cols_obj: v8::Local<v8::Object> = jsnir_obj
            .get(scope, cols_key.into())
            .and_then(|v| v.try_into().ok())
            .expect("jsnir.columns");
        self.active
            .columns
            .rebuild(scope, cols_obj, view, len)
            .expect("rebuild V8 columns");
    }

    fn call_mount(&mut self) -> Result<Vec<Op>, String> {
        Self::call_ops(
            &mut self.isolate,
            self.watchdog.as_ref(),
            &self.active,
            self.active.fn_mount.as_ref(),
            "jsnirMount",
            |tc, f, recv| f.call(tc, recv, &[]),
        )
    }

    fn call_event(
        &mut self,
        callback: u16,
        text: &str,
        guard: Option<&str>,
    ) -> Result<Vec<Op>, String> {
        Self::call_ops(
            &mut self.isolate,
            self.watchdog.as_ref(),
            &self.active,
            self.active.fn_event.as_ref(),
            "jsnirEvent",
            |tc, f, recv| {
                let cb = v8::Integer::new_from_unsigned(tc, u32::from(callback)).into();
                let txt = v8::String::new(tc, text)?.into();
                if let Some(g) = guard {
                    let gv = v8::String::new(tc, g)?.into();
                    f.call(tc, recv, &[cb, txt, gv])
                } else {
                    f.call(tc, recv, &[cb, txt])
                }
            },
        )
    }

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
    ) -> Result<Vec<Op>, String> {
        if self.active.fn_drag.is_some() {
            Self::call_ops(
                &mut self.isolate,
                self.watchdog.as_ref(),
                &self.active,
                self.active.fn_drag.as_ref(),
                "jsnirDrag",
                |tc, f, recv| {
                    let a0 = v8::Integer::new_from_unsigned(tc, u32::from(callback)).into();
                    let a1 = v8::Integer::new_from_unsigned(tc, u32::from(phase)).into();
                    let a2 = v8::Number::new(tc, f64::from(x)).into();
                    let a3 = v8::Number::new(tc, f64::from(y)).into();
                    let a4 = v8::Number::new(tc, f64::from(sx)).into();
                    let a5 = v8::Number::new(tc, f64::from(sy)).into();
                    if let Some(g) = guard {
                        let a6 = v8::String::new(tc, g)?.into();
                        f.call(tc, recv, &[a0, a1, a2, a3, a4, a5, a6])
                    } else {
                        f.call(tc, recv, &[a0, a1, a2, a3, a4, a5])
                    }
                },
            )
        } else {
            let json = fallback_drag_json(phase, x, y, sx, sy, dx, dy);
            self.call_event(callback, &json, guard)
        }
    }

    fn call_scroll(
        &mut self,
        callback: u16,
        top: f32,
        height: f32,
        client: f32,
        guard: Option<&str>,
    ) -> Result<Vec<Op>, String> {
        if self.active.fn_scroll.is_some() {
            Self::call_ops(
                &mut self.isolate,
                self.watchdog.as_ref(),
                &self.active,
                self.active.fn_scroll.as_ref(),
                "jsnirScroll",
                |tc, f, recv| {
                    let a0 = v8::Integer::new_from_unsigned(tc, u32::from(callback)).into();
                    let a1 = v8::Number::new(tc, f64::from(top)).into();
                    let a2 = v8::Number::new(tc, f64::from(height)).into();
                    let a3 = v8::Number::new(tc, f64::from(client)).into();
                    if let Some(g) = guard {
                        let a4 = v8::String::new(tc, g)?.into();
                        f.call(tc, recv, &[a0, a1, a2, a3, a4])
                    } else {
                        f.call(tc, recv, &[a0, a1, a2, a3])
                    }
                },
            )
        } else {
            let json = fallback_scroll_json(top, height, client);
            self.call_event(callback, &json, guard)
        }
    }

    fn call_context_menu(
        &mut self,
        callback: u16,
        x: f32,
        y: f32,
        guard: Option<&str>,
    ) -> Result<Vec<Op>, String> {
        if self.active.fn_context_menu.is_some() {
            Self::call_ops(
                &mut self.isolate,
                self.watchdog.as_ref(),
                &self.active,
                self.active.fn_context_menu.as_ref(),
                "jsnirContextMenu",
                |tc, f, recv| {
                    let a0 = v8::Integer::new_from_unsigned(tc, u32::from(callback)).into();
                    let a1 = v8::Number::new(tc, f64::from(x)).into();
                    let a2 = v8::Number::new(tc, f64::from(y)).into();
                    if let Some(g) = guard {
                        let a3 = v8::String::new(tc, g)?.into();
                        f.call(tc, recv, &[a0, a1, a2, a3])
                    } else {
                        f.call(tc, recv, &[a0, a1, a2])
                    }
                },
            )
        } else {
            let json = fallback_context_menu_json(x, y);
            self.call_event(callback, &json, guard)
        }
    }

    fn call_placement(&mut self, text: &str) -> Result<Vec<Op>, String> {
        Self::call_ops(
            &mut self.isolate,
            self.watchdog.as_ref(),
            &self.active,
            self.active.fn_placement.as_ref(),
            "jsnirPlacement",
            |tc, f, recv| {
                let txt = v8::String::new(tc, text)?.into();
                f.call(tc, recv, &[txt])
            },
        )
    }

    fn call_virtual_list(&mut self, delta: &str) -> Result<(String, Vec<Op>), String> {
        Self::call_string(
            &mut self.isolate,
            self.watchdog.as_ref(),
            &self.active,
            self.active.fn_virtual_list.as_ref(),
            "jsnirVirtualList",
            |tc, f, recv| {
                let txt = v8::String::new(tc, delta)?.into();
                f.call(tc, recv, &[txt])
            },
        )
    }

    fn reload_state(&mut self) -> Option<String> {
        Self::call_string(
            &mut self.isolate,
            self.watchdog.as_ref(),
            &self.active,
            self.active.fn_reload_state.as_ref(),
            "jsnirReloadState",
            |tc, f, recv| f.call(tc, recv, &[]),
        )
        .ok()
        .map(|(s, _)| s)
    }

    /// Recycles the existing `v8::OwnedIsolate` by creating only a fresh `v8::Context`.
    /// On load or mount failure, the new context is dropped and `self.active` stays intact.
    fn reload_in_place(
        &mut self,
        bundle: &ScriptBundle,
        replica: Rc<RefCell<Replica>>,
        config: &VmConfig,
    ) -> Option<Result<Vec<Op>, String>> {
        let next = match build_context(
            &mut self.isolate,
            self.watchdog.as_ref(),
            bundle,
            replica,
            config,
        ) {
            Ok(ctx) => ctx,
            Err(err) => return Some(Err(err)),
        };
        match Self::call_ops(
            &mut self.isolate,
            self.watchdog.as_ref(),
            &next,
            next.fn_mount.as_ref(),
            "jsnirMount",
            |tc, f, recv| f.call(tc, recv, &[]),
        ) {
            Ok(ops) => {
                self.active = next;
                Some(Ok(ops))
            }
            Err(err) => Some(Err(err)),
        }
    }

    fn memory_usage(&mut self) -> Option<EngineMemory> {
        Some(self.heap_statistics())
    }
}

impl V8Engine {
    /// Returns live V8 heap statistics (`used_heap_size` and `total_heap_size`).
    pub fn heap_statistics(&mut self) -> EngineMemory {
        let stats = self.isolate.get_heap_statistics();
        EngineMemory {
            used_bytes: stats.used_heap_size(),
            total_bytes: stats.total_heap_size(),
        }
    }
}
