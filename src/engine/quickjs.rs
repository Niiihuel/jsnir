//! Default lightweight JavaScript engine backend powered by QuickJS-ng (`rquickjs`).
//!
//! ~0.5 MB stripped binary cost, ~1.4 MB RSS, deterministic reference counting, sub-ms AOT
//! bytecode loading (`JSNRBC01`), and full support for `VmLimits` (memory ceiling + interrupt deadline).

use super::{
    CMD_WORDS, COLUMNS, EngineCapabilities, EngineMemory, JsEngine, JsEngineFactory, VmConfig,
    decode_commands, decode_json_batch, fallback_context_menu_json, fallback_drag_json,
    fallback_scroll_json, project,
};
use crate::{
    BYTECODE_MAGIC, ScriptBundle,
    tree::{Op, Replica, Tree},
};
use rquickjs::{
    ArrayBuffer, ArrayBufferSource, CatchResultExt, Ctx, Exception, Function, Object, Persistent,
    TypedArray,
    function::{IntoArgs, MutFn},
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// Registers a native function on a JS object and records its TypeScript signature from the same declaration.
/// A leading `Ctx` parameter is the VM context and does not appear in the JS signature.
macro_rules! def {
    ($obj:ident, $c:ident, $sigs:ident, $name:literal, || -> $ret:ty $body:block) => {{
        $sigs.push(format!("{}(): {}", $name, <$ret as $crate::contract::Ts>::ts()));
        $obj.set($name, Function::new($c.clone(), move || -> $ret { $body })?)?;
    }};
    ($obj:ident, $c:ident, $sigs:ident, $name:literal, |$ctx:ident: Ctx, $($arg:ident: $ty:ty),*| -> $ret:ty $body:block) => {{
        let args: Vec<String> = vec![$(format!("{}: {}", stringify!($arg), <$ty as $crate::contract::Ts>::ts())),*];
        $sigs.push(format!("{}({}): {}", $name, args.join(", "), <$ret as $crate::contract::Ts>::ts()));
        $obj.set($name, Function::new($c.clone(), move |$ctx: Ctx, $($arg: $ty),*| -> $ret { $body })?)?;
    }};
    ($obj:ident, $c:ident, $sigs:ident, $name:literal, |$($arg:ident: $ty:ty),*| -> $ret:ty $body:block) => {{
        let args: Vec<String> = vec![$(format!("{}: {}", stringify!($arg), <$ty as $crate::contract::Ts>::ts())),*];
        $sigs.push(format!("{}({}): {}", $name, args.join(", "), <$ret as $crate::contract::Ts>::ts()));
        $obj.set($name, Function::new($c.clone(), move |$($arg: $ty),*| -> $ret { $body })?)?;
    }};
}

/// Block of `i32` on the Rust heap that backs an immutable `ArrayBuffer` for QuickJS.
struct Column(*mut [i32]);
impl Column {
    fn new(values: Vec<i32>) -> Self {
        Self(Box::into_raw(values.into_boxed_slice()))
    }
}
// SAFETY: the block neither moves nor is freed until JS releases the buffer and Drop runs.
unsafe impl ArrayBufferSource for Column {
    fn as_ptr(&self) -> *mut u8 {
        self.0 as *mut u8
    }
    fn len(&self) -> usize {
        self.0.len() * 4
    }
}
impl Drop for Column {
    fn drop(&mut self) {
        drop(unsafe { Box::from_raw(self.0) })
    }
}

fn share<'js>(
    c: &Ctx<'js>,
    values: Vec<i32>,
) -> (
    TypedArray<'js, i32>,
    Persistent<ArrayBuffer<'static>>,
    *mut i32,
) {
    let col = Column::new(values);
    let ptr = col.0 as *mut i32;
    let ab = ArrayBuffer::from_source_immutable(c.clone(), col).expect("column out of memory");
    let view = TypedArray::<i32>::from_arraybuffer(ab.clone()).expect("column view");
    // Persistent keeps the GC from freeing the block while Rust writes it.
    (view, Persistent::save(c, ab), ptr)
}

/// A QuickJS VM's shared columns, installed as `jsnir.columns`. They grow when a larger id appears.
struct Columns {
    buffers: Vec<(Persistent<ArrayBuffer<'static>>, *mut i32)>,
    len: usize,
}
impl Columns {
    fn install<'js>(c: &Ctx<'js>, jsnir: &Object<'js>) -> rquickjs::Result<Self> {
        let col = Object::new(c.clone())?;
        jsnir.set("columns", col)?;
        let mut columns = Self {
            buffers: Vec::new(),
            len: 0,
        };
        columns.rebuild(c, &Tree::default(), 64)?;
        Ok(columns)
    }
    fn rebuild(&mut self, c: &Ctx, view: &Tree, len: usize) -> rquickjs::Result<()> {
        let target: Object = c.globals().get::<_, Object>("jsnir")?.get("columns")?;
        let rows: Vec<_> = (0..len).map(|id| project(view, id as u16)).collect();
        self.buffers.clear();
        for (k, name) in COLUMNS.iter().enumerate() {
            let (js, rust, ptr) = share(c, rows.iter().map(|r| r[k]).collect());
            target.set(*name, js)?;
            self.buffers.push((rust, ptr));
        }
        self.len = len;
        Ok(())
    }
    fn sync(&mut self, c: &Ctx, view: &Tree, ids: Option<&[u16]>) {
        let needed = view.nodes.end();
        if needed > self.len {
            let len = needed.max(self.len * 2);
            self.rebuild(c, view, len).expect("columns");
            return;
        }
        if let Some(ids) = ids {
            for &id in ids {
                let idx = id as usize;
                assert!(idx < self.len, "id outside the column");
                let row = project(view, id);
                for (k, &(_, ptr)) in self.buffers.iter().enumerate() {
                    // SAFETY: `Persistent` keeps the `Column` heap slice alive; JS is not running; `idx < self.len`.
                    unsafe { ptr.add(idx).write(row[k]) };
                }
            }
        } else {
            for idx in 0..self.len {
                let row = project(view, idx as u16);
                for (k, &(_, ptr)) in self.buffers.iter().enumerate() {
                    // SAFETY: same as above.
                    unsafe { ptr.add(idx).write(row[k]) };
                }
            }
        }
    }
}

/// Backing storage for the mutable shared JS→Rust binary command buffer in QuickJS.
struct CmdStorage {
    data: *mut [u32],
    alive: Rc<Cell<bool>>,
}
impl CmdStorage {
    fn new(words: usize, alive: Rc<Cell<bool>>) -> Self {
        Self {
            data: Box::into_raw(vec![0u32; words].into_boxed_slice()),
            alive,
        }
    }
}
// SAFETY: `CmdStorage` owns its heap allocation until JS drops the `ArrayBuffer`.
unsafe impl ArrayBufferSource for CmdStorage {
    fn as_ptr(&self) -> *mut u8 {
        self.data as *mut u8
    }
    fn len(&self) -> usize {
        self.data.len() * 4
    }
}
impl Drop for CmdStorage {
    fn drop(&mut self) {
        self.alive.set(false);
        drop(unsafe { Box::from_raw(self.data) });
    }
}

#[derive(Clone)]
struct CommandReader {
    ptr: *const u32,
    cap: usize,
    alive: Rc<Cell<bool>>,
}
impl CommandReader {
    fn decode(&self, words: usize, strings: &mut [String], out: &mut Vec<Op>) -> Result<(), String> {
        if !self.alive.get() {
            return Err("command buffer detached".to_string());
        }
        if words > self.cap {
            return Err(format!(
                "command buffer length {words} exceeds capacity {}",
                self.cap
            ));
        }
        // SAFETY: `alive` is true so `CmdStorage`'s `Box<[u32]>` of `self.cap` words is still allocated,
        // `u32`-aligned, and no JS runs during `commit`.
        let slice = unsafe { std::slice::from_raw_parts(self.ptr, words) };
        decode_commands(slice, strings, out)
    }
}

struct CommandBuffer {
    _buffer: Persistent<ArrayBuffer<'static>>,
}
impl CommandBuffer {
    fn install<'js>(
        c: &Ctx<'js>,
        jsnir: &Object<'js>,
    ) -> rquickjs::Result<(Self, CommandReader)> {
        let alive = Rc::new(Cell::new(true));
        let storage = CmdStorage::new(CMD_WORDS, alive.clone());
        let ptr = storage.data as *const u32;
        let ab = ArrayBuffer::from_source(c.clone(), storage)?;
        let u32_view = TypedArray::<u32>::from_arraybuffer(ab.clone())?;
        let f32_view = TypedArray::<f32>::from_arraybuffer(ab.clone())?;
        jsnir.set("cmd", u32_view)?;
        jsnir.set("cmdF32", f32_view)?;
        Ok((
            Self {
                _buffer: Persistent::save(c, ab),
            },
            CommandReader {
                ptr,
                cap: CMD_WORDS,
                alive,
            },
        ))
    }
}

/// Compiles a JS bundle (SDK + app) ahead of time into QuickJS bytecode (`JSNRBC01` header + raw bytecode).
/// Loading the resulting bytes skips JS parsing completely on startup.
pub fn compile_bytecode(js: &str) -> Result<Vec<u8>, String> {
    let rt = rquickjs::Runtime::new().map_err(|e| format!("VM out of memory: {e}"))?;
    let ctx = rquickjs::Context::full(&rt).map_err(|e| format!("VM out of memory: {e}"))?;
    ctx.with(|c| {
        let res: rquickjs::Result<Vec<u8>> = (|| {
            let filename = std::ffi::CString::new("<jsnir-bundle>").unwrap();
            let flags = (rquickjs::qjs::JS_EVAL_TYPE_GLOBAL
                | rquickjs::qjs::JS_EVAL_FLAG_COMPILE_ONLY) as i32;
            let raw = c.as_raw().as_ptr();
            let mut src = Vec::with_capacity(js.len() + 1);
            src.extend_from_slice(js.as_bytes());
            src.push(0);
            // SAFETY: `src` is null-terminated with length `js.len()` and lives across `JS_Eval`.
            let func = unsafe {
                rquickjs::qjs::JS_Eval(
                    raw,
                    src.as_ptr().cast(),
                    js.len() as _,
                    filename.as_ptr(),
                    flags,
                )
            };
            if unsafe { rquickjs::qjs::JS_IsException(func) } {
                return Err(rquickjs::Error::Exception);
            }
            let mut len = std::mem::MaybeUninit::uninit();
            // SAFETY: `func` is a valid compiled JS bytecode function owned by `raw`.
            let buf = unsafe {
                let b = rquickjs::qjs::JS_WriteObject(
                    raw,
                    len.as_mut_ptr(),
                    func,
                    rquickjs::qjs::JS_WRITE_OBJ_BYTECODE as i32,
                );
                rquickjs::qjs::JS_FreeValue(raw, func);
                b
            };
            if buf.is_null() {
                return Err(rquickjs::Error::Exception);
            }
            let len = unsafe { len.assume_init() };
            let mut out = Vec::with_capacity(BYTECODE_MAGIC.len() + len as usize);
            out.extend_from_slice(BYTECODE_MAGIC);
            // SAFETY: QuickJS returned `len` valid bytes at `buf`.
            out.extend_from_slice(unsafe { std::slice::from_raw_parts(buf, len as usize) });
            unsafe { rquickjs::qjs::js_free(raw, buf.cast()) };
            Ok(out)
        })();
        res.catch(&c).map_err(|e| e.to_string())
    })
}

fn eval_bytecode(c: &Ctx<'_>, bytes: &[u8]) -> rquickjs::Result<()> {
    let payload = bytes
        .strip_prefix(BYTECODE_MAGIC)
        .ok_or_else(|| Exception::throw_type(c, "invalid JSNIR bytecode header"))?;
    let raw = c.as_raw().as_ptr();
    // SAFETY: Without `JS_READ_OBJ_ROM_DATA`, QuickJS copies bytecode into VM-owned memory.
    let obj = unsafe {
        rquickjs::qjs::JS_ReadObject(
            raw,
            payload.as_ptr(),
            payload.len() as _,
            rquickjs::qjs::JS_READ_OBJ_BYTECODE as i32,
        )
    };
    if unsafe { rquickjs::qjs::JS_IsException(obj) } {
        return Err(rquickjs::Error::Exception);
    }
    let val = unsafe { rquickjs::qjs::JS_EvalFunction(raw, obj) };
    if unsafe { rquickjs::qjs::JS_IsException(val) } {
        return Err(rquickjs::Error::Exception);
    }
    unsafe { rquickjs::qjs::JS_FreeValue(raw, val) };
    Ok(())
}

// Lock-free per-call deadline shared with QuickJS's interrupt handler (only armed when `max_call_duration` is set).
struct Deadline {
    epoch: Instant,
    limit_nanos: Arc<AtomicU64>,
    duration: Option<Duration>,
}
struct DeadlineGuard<'a>(&'a AtomicU64);
impl Drop for DeadlineGuard<'_> {
    fn drop(&mut self) {
        self.0.store(0, Ordering::Relaxed);
    }
}
impl Deadline {
    fn arm(&self) -> Option<DeadlineGuard<'_>> {
        let dur = self.duration?;
        let now = self.epoch.elapsed().as_nanos() as u64;
        let deadline = now.saturating_add(dur.as_nanos().min(u64::MAX as u128) as u64).max(1);
        self.limit_nanos.store(deadline, Ordering::Relaxed);
        Some(DeadlineGuard(&self.limit_nanos))
    }
}

/// Factory for the default QuickJS-ng engine.
#[derive(Clone, Copy, Debug, Default)]
pub struct QuickJsFactory;

impl JsEngineFactory for QuickJsFactory {
    fn name(&self) -> &'static str {
        "quickjs-ng"
    }

    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities {
            jit: false,
            bytecode_aot: true,
            in_place_reload: false,
        }
    }

    fn create(
        &self,
        bundle: &ScriptBundle,
        replica: Rc<RefCell<Replica>>,
        config: &VmConfig,
    ) -> Result<Box<dyn JsEngine>, String> {
        Ok(Box::new(QuickJsEngine::try_load_config(
            bundle, replica, config,
        )?))
    }
}

/// A QuickJS VM implementing [`JsEngine`]. Created and used on the app thread only.
pub struct QuickJsEngine {
    // Persistent JS roots (`columns`, `_cmd`) MUST be declared before `ctx` and `_rt` so they drop
    // while `ctx` is still alive and `JS_FreeRuntime` sees an empty GC object list.
    columns: Columns,
    _cmd: CommandBuffer,
    pub(crate) ctx: rquickjs::Context,
    pub(crate) ops: Rc<RefCell<Vec<Op>>>,
    has_jsnir_drag: bool,
    has_jsnir_scroll: bool,
    has_jsnir_context_menu: bool,
    pub(crate) signatures: Vec<String>,
    deadline: Deadline,
    rt: rquickjs::Runtime,
}

impl QuickJsEngine {
    #[cfg(feature = "examples")]
    pub(crate) fn counter(replica: Rc<RefCell<Replica>>) -> Self {
        Self::load(
            include_str!(concat!(env!("OUT_DIR"), "/counter.js")),
            replica,
        )
    }

    pub(crate) fn load(js: &str, replica: Rc<RefCell<Replica>>) -> Self {
        Self::try_load(js, replica, None).unwrap_or_else(|e| panic!("script failed to load: {e}"))
    }

    pub(crate) fn try_load(
        js: &str,
        replica: Rc<RefCell<Replica>>,
        previous: Option<&str>,
    ) -> Result<Self, String> {
        let config = VmConfig {
            previous_state: previous.map(str::to_owned),
            ..Default::default()
        };
        Self::try_load_config(&ScriptBundle::Source(js.to_string()), replica, &config)
    }

    pub fn try_load_config(
        bundle: &ScriptBundle,
        replica: Rc<RefCell<Replica>>,
        config: &VmConfig,
    ) -> Result<Self, String> {
        let rt = rquickjs::Runtime::new().map_err(|e| format!("VM out of memory: {e}"))?;
        if let Some(limit) = config.limits.max_memory_bytes {
            rt.set_memory_limit(limit);
        }
        let deadline = Deadline {
            epoch: Instant::now(),
            limit_nanos: Arc::new(AtomicU64::new(0)),
            duration: config.limits.max_call_duration,
        };
        if config.limits.max_call_duration.is_some() {
            let epoch = deadline.epoch;
            let limit_nanos = deadline.limit_nanos.clone();
            rt.set_interrupt_handler(Some(Box::new(move || {
                let limit = limit_nanos.load(Ordering::Relaxed);
                limit != 0 && (epoch.elapsed().as_nanos() as u64) >= limit
            })));
        }
        let ctx = rquickjs::Context::full(&rt).map_err(|e| format!("VM out of memory: {e}"))?;
        let ops: Rc<RefCell<Vec<Op>>> = Rc::default();
        let mut sigs = Vec::new();
        let _guard = deadline.arm();
        let (columns, cmd, has_jsnir_drag, has_jsnir_scroll, has_jsnir_context_menu) =
            ctx.with(|c| {
                let r = (|| {
                    let jsnir = Object::new(c.clone())?;
                    let sink = ops.clone();
                    def!(jsnir, c, sigs, "set", |id: u16, value: u16| -> () {
                        sink.borrow_mut().push(Op::Set { id, value })
                    });
                    let sink = ops.clone();
                    def!(
                        jsnir,
                        c,
                        sigs,
                        "batch",
                        |ctx: Ctx, json: String| -> rquickjs::Result<()> {
                            decode_json_batch(&json, &mut sink.borrow_mut())
                                .map_err(|e| Exception::throw_type(&ctx, &e))
                        }
                    );
                    let (cmd, cmd_reader) = CommandBuffer::install(&c, &jsnir)?;
                    let cmd_reader_num = cmd_reader.clone();
                    let sink = ops.clone();
                    def!(
                        jsnir,
                        c,
                        sigs,
                        "commit",
                        |ctx: Ctx, words: u32| -> rquickjs::Result<()> {
                            let mut ops = sink.borrow_mut();
                            cmd_reader_num
                                .decode(words as usize, &mut [], &mut ops)
                                .map_err(|e| {
                                    Exception::throw_type(&ctx, &format!("invalid cmd: {e}"))
                                })
                        }
                    );
                    let sink = ops.clone();
                    def!(
                        jsnir,
                        c,
                        sigs,
                        "commitStrings",
                        |ctx: Ctx, words: u32, strings: Vec<String>| -> rquickjs::Result<()> {
                            let mut strings = strings;
                            let mut ops = sink.borrow_mut();
                            cmd_reader
                                .decode(words as usize, &mut strings, &mut ops)
                                .map_err(|e| {
                                    Exception::throw_type(&ctx, &format!("invalid cmd: {e}"))
                                })
                        }
                    );
                    def!(jsnir, c, sigs, "report", |message: String| -> () {
                        eprintln!("app error, the event keeps its changes: {message}")
                    });
                    let sink = ops.clone();
                    def!(jsnir, c, sigs, "setText", |id: u16, text: String| -> () {
                        sink.borrow_mut().push(Op::SetText { id, text })
                    });
                    let read = replica.clone();
                    def!(jsnir, c, sigs, "readText", |id: u16| -> Option<String> {
                        read.borrow().view.text(id).map(str::to_owned)
                    });
                    c.globals().set("jsnir", jsnir.clone())?;
                    if let Some(state) = config.previous_state.as_deref() {
                        c.globals().set("__jsnirPreviousState", state)?;
                    }
                    c.globals().set("__jsnirArgs", config.args.as_str())?;
                    if config.placement_epoch != 0 {
                        c.globals()
                            .set("__jsnirPlacementEpoch", config.placement_epoch.to_string())?;
                    }
                    if let Some(handler) = config.request_handler.clone() {
                        let request = Function::new(
                            c.clone(),
                            MutFn::new(move |ctx: Ctx<'_>, json: String| {
                                let mut handler = handler.lock().expect("request handler");
                                (*handler)(json)
                                    .map_err(|error| Exception::throw_message(&ctx, &error))
                            }),
                        )?;
                        c.globals().set("jsnirRequest", request)?;
                    }
                    let columns = Columns::install(&c, &jsnir)?;
                    match bundle {
                        ScriptBundle::Source(js) => c.eval::<(), _>(js.as_str())?,
                        ScriptBundle::Bytecode(bytes) => eval_bytecode(&c, bytes)?,
                    }
                    let globals = c.globals();
                    let has_drag = globals.get::<_, Function>("jsnirDrag").is_ok();
                    let has_scroll = globals.get::<_, Function>("jsnirScroll").is_ok();
                    let has_context_menu = globals.get::<_, Function>("jsnirContextMenu").is_ok();
                    Ok((columns, cmd, has_drag, has_scroll, has_context_menu))
                })();
                r.catch(&c).map_err(|e| e.to_string())
            })?;
        drop(_guard);
        Ok(Self {
            ctx,
            ops,
            columns,
            _cmd: cmd,
            has_jsnir_drag,
            has_jsnir_scroll,
            has_jsnir_context_menu,
            signatures: sigs,
            deadline,
            rt,
        })
    }

    pub(crate) fn sync(&mut self, view: &Tree, ids: Option<&[u16]>) {
        let Self { ctx, columns, .. } = self;
        ctx.with(|c| columns.sync(&c, view, ids));
    }

    pub(crate) fn try_invoke<R: for<'js> rquickjs::FromJs<'js>>(
        &self,
        name: &str,
        args: impl for<'js> IntoArgs<'js>,
    ) -> Result<R, String> {
        let _guard = self.deadline.arm();
        self.ctx.with(|c| {
            let r: rquickjs::Result<R> = c
                .globals()
                .get::<_, Function>(name)
                .and_then(|f| f.call(args));
            r.catch(&c).map_err(|e| format!("{name}: {e}"))
        })
    }

    pub(crate) fn try_call(
        &self,
        name: &str,
        args: impl for<'js> IntoArgs<'js>,
    ) -> Result<Vec<Op>, String> {
        self.try_call_with::<()>(name, args).map(|((), ops)| ops)
    }

    fn try_call_with<R: for<'js> rquickjs::FromJs<'js>>(
        &self,
        name: &str,
        args: impl for<'js> IntoArgs<'js>,
    ) -> Result<(R, Vec<Op>), String> {
        self.ops.borrow_mut().clear();
        let result = self.try_invoke::<R>(name, args);
        let ops = std::mem::take(&mut *self.ops.borrow_mut());
        result.map(|value| (value, ops))
    }

    #[cfg(all(test, feature = "examples"))]
    pub(crate) fn call(&self, name: &str, args: impl for<'js> IntoArgs<'js>) -> Vec<Op> {
        self.try_call(name, args).unwrap_or_else(|e| panic!("{e}"))
    }
}

impl JsEngine for QuickJsEngine {
    fn name(&self) -> &'static str {
        "quickjs-ng"
    }

    fn sync(&mut self, view: &Tree, ids: Option<&[u16]>) {
        QuickJsEngine::sync(self, view, ids)
    }

    fn call_mount(&mut self) -> Result<Vec<Op>, String> {
        self.try_call("jsnirMount", ())
    }

    fn call_event(
        &mut self,
        callback: u16,
        text: &str,
        guard: Option<&str>,
    ) -> Result<Vec<Op>, String> {
        match guard {
            Some(guard) => self.try_call("jsnirEvent", (callback, text, guard)),
            None => self.try_call("jsnirEvent", (callback, text)),
        }
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
        if self.has_jsnir_drag {
            match guard {
                Some(guard) => self.try_call("jsnirDrag", (callback, phase, x, y, sx, sy, guard)),
                None => self.try_call("jsnirDrag", (callback, phase, x, y, sx, sy)),
            }
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
        if self.has_jsnir_scroll {
            match guard {
                Some(guard) => self.try_call("jsnirScroll", (callback, top, height, client, guard)),
                None => self.try_call("jsnirScroll", (callback, top, height, client)),
            }
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
        if self.has_jsnir_context_menu {
            match guard {
                Some(guard) => self.try_call("jsnirContextMenu", (callback, x, y, guard)),
                None => self.try_call("jsnirContextMenu", (callback, x, y)),
            }
        } else {
            let json = fallback_context_menu_json(x, y);
            self.call_event(callback, &json, guard)
        }
    }

    fn call_placement(&mut self, text: &str) -> Result<Vec<Op>, String> {
        self.try_call("jsnirPlacement", (text,))
    }

    fn call_virtual_list(&mut self, delta: &str) -> Result<(String, Vec<Op>), String> {
        self.try_call_with::<String>("jsnirVirtualList", (delta,))
    }

    fn reload_state(&mut self) -> Option<String> {
        self.try_invoke::<String>("jsnirReloadState", ()).ok()
    }

    fn memory_usage(&mut self) -> Option<EngineMemory> {
        let u = self.rt.memory_usage();
        Some(EngineMemory {
            used_bytes: u.malloc_size.max(0) as usize,
            total_bytes: u.malloc_limit.max(0) as usize,
        })
    }
}
