//! The single contract between Rust and JS. Each column and each native function is declared once,
//! and both lenses come from that declaration: the Rust code and the TypeScript type (`app/jsnir.d.ts`).
//!
//! Columns: numeric data on the Rust heap, one value per node id. JS reads them as `Int32Array`
//! with a single interpreter instruction, without calling Rust or converting values. Rust is the only
//! writer and writes only between callbacks, on the VM thread: reads and writes never overlap.
use crate::tree::{
    Align, Axis, Cursor, Edges, Justify, Node, Op, Paint, Position, Radius, ScrollAnchor,
    ScrollEdge, Style, Tree,
};
use rquickjs::{ArrayBuffer, ArrayBufferSource, Ctx, Object, Persistent, TypedArray};

/// TypeScript type of a value that crosses the boundary.
pub trait Ts {
    fn ts() -> String;
}
macro_rules! ts {
    ($($t:ty => $s:literal),* $(,)?) => { $(impl Ts for $t { fn ts() -> String { $s.into() } })* };
}
ts! { bool => "boolean", u16 => "number", u32 => "number", i32 => "number", f32 => "number", f64 => "number", String => "string", () => "void" }
impl<T: Ts> Ts for Option<T> {
    fn ts() -> String {
        format!("{} | undefined", T::ts())
    }
}
impl<T: Ts> Ts for rquickjs::Result<T> {
    fn ts() -> String {
        T::ts()
    }
}
impl<T: Ts> Ts for Box<T> {
    fn ts() -> String {
        T::ts()
    }
}
impl<T: Ts> Ts for Vec<T> {
    fn ts() -> String {
        format!("{}[]", T::ts())
    }
}
impl<T: Ts, const N: usize> Ts for [T; N] {
    fn ts() -> String {
        format!("[{}]", vec![T::ts(); N].join(", "))
    }
}
impl Ts for TypedArray<'_, i32> {
    fn ts() -> String {
        "Int32Array".into()
    }
}

/// `snake_case` Rust name → `camelCase` JS name, the convention on each side of the boundary.
pub(crate) fn camel(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper = false;
    for c in name.chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// Declares a contract enum of plain strings: the Rust enum with serde and a TypeScript union of literals.
macro_rules! contract_strings {
    (
        $(#[doc = $doc:literal])*
        $vis:vis enum $name:ident as $ts:literal { $first:ident $(, $variant:ident)* $(,)? }
    ) => {
        $(#[doc = $doc])*
        #[derive(Debug, serde::Deserialize, Clone, Copy, Default, PartialEq, Eq, Hash)]
        #[serde(rename_all = "camelCase")]
        $vis enum $name {
            #[default]
            $first,
            $($variant),*
        }
        impl $crate::contract::Ts for $name {
            fn ts() -> String {
                $ts.into()
            }
        }
        impl $name {
            /// TypeScript declaration: the union of the accepted strings.
            pub(crate) fn ts_decl() -> String {
                let names = [stringify!($first) $(, stringify!($variant))*];
                let literals: Vec<String> = names
                    .iter()
                    .map(|n| {
                        let mut c = n.chars();
                        let first = c.next().unwrap().to_lowercase();
                        format!("\"{}{}\"", first, c.as_str())
                    })
                    .collect();
                format!("type {} = {};\n", $ts, literals.join(" | "))
            }
        }
    };
}

/// Declares a contract enum and generates both lenses: the Rust enum with serde and its TypeScript union.
/// Each variant's JSON name comes from `rename_all = "lowercase"` on both sides.
/// Markers: `(skip)` on a field means Rust derives it and JS does not send it; `(default)` makes it optional
/// in JS; `(ui)` on a variant makes it UI only and keeps it out of the type JS sees.
macro_rules! contract_enum {
    (
        $(#[doc = $doc:literal])*
        $vis:vis enum $name:ident as $ts:literal, tag $tag:literal {
            $(
                $(#[doc = $vdoc:literal])*
                $(($vmark:ident))? $variant:ident {
                    $($(#[doc = $fdoc:literal])* $(($fmark:ident))? $field:ident: $fty:ty),* $(,)?
                }
            ),* $(,)?
        }
    ) => {
        $(#[doc = $doc])*
        #[derive(Debug, serde::Deserialize, Clone, PartialEq)]
        #[serde(tag = $tag, rename_all = "lowercase", rename_all_fields = "camelCase")]
        $vis enum $name {
            $(
                $(#[doc = $vdoc])*
                $variant {
                    $($(#[doc = $fdoc])* $(#[serde($fmark)])? $field: $fty),*
                },
            )*
        }
        impl $crate::contract::Ts for $name {
            fn ts() -> String {
                $ts.into()
            }
        }
        impl $name {
            /// TypeScript declaration: the union of the variants the app may send.
            pub(crate) fn ts_decl() -> String {
                let mut variants: Vec<String> = Vec::new();
                $(
                    if stringify!($($vmark)?) != "ui" {
                        let mut fields = vec![format!("{}: \"{}\"", $tag, stringify!($variant).to_lowercase())];
                        $(
                            let ts = <$fty as $crate::contract::Ts>::ts();
                            match stringify!($($fmark)?) {
                                "skip" => {}
                                "default" => fields.push(format!("{}?: {}", $crate::contract::camel(stringify!($field)), ts)),
                                _ => fields.push(format!("{}: {}", $crate::contract::camel(stringify!($field)), ts)),
                            }
                        )*
                        variants.push(format!("{{ {} }}", fields.join("; ")));
                    }
                )*
                format!("type {} =\n    | {};\n", $ts, variants.join("\n    | "))
            }
        }
    };
}

/// Declares a contract struct: the Rust struct with serde (every field optional, unknown fields
/// rejected) and its TypeScript interface, from the same declaration.
macro_rules! contract_struct {
    (
        $(#[doc = $doc:literal])*
        $vis:vis struct $name:ident as $ts:literal {
            $($(#[doc = $fdoc:literal])* $field:ident: $fty:ty),* $(,)?
        }
    ) => {
        $(#[doc = $doc])*
        #[derive(Debug, serde::Deserialize, Clone, Default, PartialEq)]
        #[serde(default, deny_unknown_fields, rename_all = "camelCase")]
        $vis struct $name {
            $($(#[doc = $fdoc])* pub $field: $fty),*
        }
        impl $crate::contract::Ts for $name {
            fn ts() -> String {
                $ts.into()
            }
        }
        impl $name {
            /// TypeScript declaration: every field is optional, as in serde.
            pub(crate) fn ts_decl() -> String {
                let fields: Vec<String> = vec![$(format!("{}?: {}", $crate::contract::camel(stringify!($field)), <$fty as $crate::contract::Ts>::ts())),*];
                format!("interface {} {{ {} }}\n", $ts, fields.join("; "))
            }
        }
    };
}

/// Declares the columns: the name JS sees and how to get its value from the view for an id.
macro_rules! contract_columns {
    ($($(#[doc = $doc:literal])+ $name:ident: |$t:ident, $id:ident| $value:expr,)*) => {
        pub const COLUMNS: &[&str] = &[$(stringify!($name)),*];
        /// Value of each column for an id, in `COLUMNS` order. Zero if the node has no such data.
        pub fn project(tree: &Tree, id: u16) -> [i32; COLUMNS.len()] {
            [$({
                let ($t, $id) = (tree, id);
                $value
            }),*]
        }
        fn columns_dts() -> String {
            let mut out = String::new();
            $(
                out += "        /**";
                $(out += $doc;)+
                out += " */\n";
                out += concat!("        ", stringify!($name), ": Int32Array;\n");
            )*
            out
        }
    };
}
contract_columns! {
    /// Value of a label.
    value: |t, id| match t.nodes.get(&id) { Some(Node::Label { value }) => *value as i32, _ => 0 },
    /// Number of children of a column or list.
    children: |t, id| t.children(id).len() as i32,
    /// Start of a field's selection, in characters.
    selectionStart: |t, id| match t.nodes.get(&id) { Some(Node::Input { selection, .. }) => selection[0] as i32, _ => 0 },
    /// End of a field's selection, in characters.
    selectionEnd: |t, id| match t.nodes.get(&id) { Some(Node::Input { selection, .. }) => selection[1] as i32, _ => 0 },
    /// Horizontal position in the window, in logical pixels (Taffy layout, rounded).
    x: |t, id| t.rect(id)[0] as i32,
    /// Vertical position in the window, in logical pixels.
    y: |t, id| t.rect(id)[1] as i32,
    /// Width computed by the layout.
    width: |t, id| t.rect(id)[2] as i32,
    /// Height computed by the layout.
    height: |t, id| t.rect(id)[3] as i32,
    /// Offset of a scroll node, in logical pixels (0 for other nodes).
    scrollTop: |t, id| t.scroll(id)[0] as i32,
    /// Content height of a scroll node, in logical pixels.
    scrollHeight: |t, id| t.scroll(id)[1] as i32,
    /// Box height of a scroll node, in logical pixels.
    clientHeight: |t, id| t.scroll(id)[2] as i32,
}

/// Block of i32 on the Rust heap that backs an immutable `ArrayBuffer` for JS.
pub(crate) struct Column(*mut [i32]);
impl Column {
    pub(crate) fn new(values: Vec<i32>) -> Self {
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
/// Shares values with JS: returns the view JS sees, the `Persistent` root keeping the buffer alive,
/// and the stable raw pointer on the Rust heap for zero-overhead writes between callbacks.
/// QuickJS-ng 0.16.2 silently ignores JS writes to an immutable buffer.
pub(crate) fn share<'js>(
    c: &Ctx<'js>,
    values: Vec<i32>,
) -> (TypedArray<'js, i32>, Persistent<ArrayBuffer<'static>>, *mut i32) {
    let col = Column::new(values);
    let ptr = col.0 as *mut i32;
    let ab = ArrayBuffer::from_source_immutable(c.clone(), col).expect("column out of memory");
    let view = TypedArray::<i32>::from_arraybuffer(ab.clone()).expect("column view");
    // Persistent keeps the GC from freeing the block while Rust writes it.
    (view, Persistent::save(c, ab), ptr)
}

/// A VM's columns, installed as `jsnir.columns`. They grow when a larger id appears.
pub(crate) struct Columns {
    buffers: Vec<(Persistent<ArrayBuffer<'static>>, *mut i32)>,
    len: usize,
}
impl Columns {
    pub(crate) fn install<'js>(c: &Ctx<'js>, jsnir: &Object<'js>) -> rquickjs::Result<Self> {
        let col = Object::new(c.clone())?;
        jsnir.set("columns", col)?;
        let mut columns = Self {
            buffers: Vec::new(),
            len: 0,
        };
        columns.rebuild(c, &Tree::default(), 64)?;
        Ok(columns)
    }
    // Creates new columns with the full state of the view and hangs them from `jsnir.columns`.
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
    /// Copies the given nodes of the view to the columns, or all of them with `None`. Only while no JS runs.
    pub(crate) fn sync(&mut self, c: &Ctx, view: &Tree, ids: Option<&[u16]>) {
        let needed = view.nodes.end();
        if needed > self.len {
            // shortcut: growing copies everything; trees of hundreds of thousands of nodes would want growth by blocks.
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
            // In-place full refresh without reallocating 11 ArrayBuffers or touching JS GC.
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

use std::{cell::Cell, rc::Rc};

/// Number of `u32` words (64 KiB) in the shared JS→Rust binary command buffer (`jsnir.cmd` / `jsnir.cmdF32`).
pub(crate) const CMD_WORDS: usize = 16_384;

/// Backing storage for the mutable shared JS→Rust binary command buffer.
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

/// Read handle passed to the `jsnir.commit` closure without retaining any QuickJS GC root.
#[derive(Clone)]
pub(crate) struct CommandReader {
    ptr: *const u32,
    cap: usize,
    alive: Rc<Cell<bool>>,
}
impl CommandReader {
    pub(crate) fn decode(&self, words: usize, out: &mut Vec<Op>) -> Result<(), String> {
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
        decode_commands(slice, out)
    }
}

/// Shared mutable binary command buffer exposed as `jsnir.cmd` (`Uint32Array`) and `jsnir.cmdF32` (`Float32Array`).
/// JS encodes frequent mutations as packed 32-bit words without `JSON.stringify`, then calls `jsnir.commit(words)`.
pub(crate) struct CommandBuffer {
    _buffer: Persistent<ArrayBuffer<'static>>,
}
impl CommandBuffer {
    pub(crate) fn install<'js>(
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

pub(crate) const OP_SET: u32 = 1;
pub(crate) const OP_SET_DISABLED: u32 = 2;
pub(crate) const OP_REMOVE: u32 = 3;
pub(crate) const OP_MOVE: u32 = 4;
pub(crate) const OP_FOCUS: u32 = 5;
pub(crate) const OP_AFTER: u32 = 6;
pub(crate) const OP_SCROLL_TO: u32 = 7;
pub(crate) const OP_SET_STYLE: u32 = 8;
pub(crate) const OP_SET_PAINT: u32 = 9;

pub(crate) fn decode_commands(words: &[u32], out: &mut Vec<Op>) -> Result<(), String> {
    let mut i = 0usize;
    let next = |i: &mut usize| -> Result<u32, String> {
        let v = *words
            .get(*i)
            .ok_or_else(|| "truncated binary command".to_string())?;
        *i += 1;
        Ok(v)
    };
    let next_u16 = |i: &mut usize| -> Result<u16, String> {
        let v = next(i)?;
        u16::try_from(v).map_err(|_| format!("invalid u16 operand {v}"))
    };
    let next_f32 = |i: &mut usize| -> Result<f32, String> { Ok(f32::from_bits(next(i)?)) };
    let next_f32x4 = |i: &mut usize| -> Result<[f32; 4], String> {
        Ok([next_f32(i)?, next_f32(i)?, next_f32(i)?, next_f32(i)?])
    };

    while i < words.len() {
        let op = next(&mut i)?;
        match op {
            OP_SET => {
                let id = next_u16(&mut i)?;
                let value = next_u16(&mut i)?;
                out.push(Op::Set { id, value });
            }
            OP_SET_DISABLED => {
                let id = next_u16(&mut i)?;
                let disabled = next(&mut i)? != 0;
                out.push(Op::SetDisabled { id, disabled });
            }
            OP_REMOVE => {
                let id = next_u16(&mut i)?;
                out.push(Op::Remove { id });
            }
            OP_MOVE => {
                let id = next_u16(&mut i)?;
                let raw = next(&mut i)?;
                let before = if raw == u32::MAX {
                    None
                } else {
                    Some(u16::try_from(raw).map_err(|_| format!("invalid before id {raw}"))?)
                };
                out.push(Op::Move { id, before });
            }
            OP_FOCUS => {
                let id = next_u16(&mut i)?;
                out.push(Op::Focus { id });
            }
            OP_AFTER => {
                let callback = next_u16(&mut i)?;
                let ms = next_f32(&mut i)?;
                out.push(Op::After { ms, callback });
            }
            OP_SCROLL_TO => {
                let id = next_u16(&mut i)?;
                let flags = next(&mut i)?;
                let raw_offset = next_f32(&mut i)?;
                let offset = (flags & 1 != 0).then_some(raw_offset);
                let to = (flags & 2 != 0).then_some(if flags & 4 != 0 {
                    ScrollEdge::End
                } else {
                    ScrollEdge::Start
                });
                let smooth = flags & 8 != 0;
                out.push(Op::ScrollTo {
                    id,
                    offset,
                    to,
                    smooth,
                });
            }
            OP_SET_STYLE => {
                let id = next_u16(&mut i)?;
                let mask = next(&mut i)?;
                let flags = next(&mut i)?;
                let mut style = Style {
                    direction: if flags & 1 != 0 {
                        Axis::Row
                    } else {
                        Axis::Column
                    },
                    align: match (flags >> 1) & 0x7 {
                        1 => Some(Align::Stretch),
                        2 => Some(Align::Start),
                        3 => Some(Align::Center),
                        4 => Some(Align::End),
                        _ => None,
                    },
                    justify: match (flags >> 4) & 0x3 {
                        1 => Justify::Center,
                        2 => Justify::End,
                        3 => Justify::Between,
                        _ => Justify::Start,
                    },
                    position: match (flags >> 6) & 0x3 {
                        1 => Position::Absolute,
                        2 => Position::Fixed,
                        _ => Position::Relative,
                    },
                    clip: flags & (1 << 8) != 0,
                    scroll: flags & (1 << 9) != 0,
                    scroll_anchor: if flags & (1 << 10) != 0 {
                        ScrollAnchor::End
                    } else {
                        ScrollAnchor::Start
                    },
                    group: flags & (1 << 11) != 0,
                    ..Style::default()
                };
                if mask & (1 << 0) != 0 {
                    style.width = Some(next_f32(&mut i)?);
                }
                if mask & (1 << 1) != 0 {
                    style.height = Some(next_f32(&mut i)?);
                }
                if mask & (1 << 2) != 0 {
                    style.grow = next_f32(&mut i)?;
                }
                if mask & (1 << 3) != 0 {
                    style.shrink = Some(next_f32(&mut i)?);
                }
                if mask & (1 << 4) != 0 {
                    style.basis = Some(next_f32(&mut i)?);
                }
                if mask & (1 << 5) != 0 {
                    style.padding = Edges::Sides(next_f32x4(&mut i)?);
                }
                if mask & (1 << 6) != 0 {
                    style.margin = Edges::Sides(next_f32x4(&mut i)?);
                }
                if mask & (1 << 7) != 0 {
                    style.gap = next_f32(&mut i)?;
                }
                if mask & (1 << 8) != 0 {
                    style.left = Some(next_f32(&mut i)?);
                }
                if mask & (1 << 9) != 0 {
                    style.top = Some(next_f32(&mut i)?);
                }
                if mask & (1 << 10) != 0 {
                    style.follow = Some(next_u16(&mut i)?);
                }
                if mask & (1 << 11) != 0 {
                    style.layer = Some(next(&mut i)? as i32);
                }
                if mask & (1 << 12) != 0 {
                    style.min_width = Some(next_f32(&mut i)?);
                }
                if mask & (1 << 13) != 0 {
                    style.min_height = Some(next_f32(&mut i)?);
                }
                if mask & (1 << 14) != 0 {
                    style.max_width = Some(next_f32(&mut i)?);
                }
                if mask & (1 << 15) != 0 {
                    style.max_height = Some(next_f32(&mut i)?);
                }
                out.push(Op::SetStyle { id, style });
            }
            OP_SET_PAINT => {
                let id = next_u16(&mut i)?;
                let mask = next(&mut i)?;
                let mut paint = Paint::default();
                if mask & (1 << 0) != 0 {
                    paint.background = Some(next(&mut i)?);
                }
                if mask & (1 << 1) != 0 {
                    paint.gradient = Some([next(&mut i)?, next(&mut i)?]);
                }
                if mask & (1 << 2) != 0 {
                    paint.radius = Radius::Corners(next_f32x4(&mut i)?);
                }
                if mask & (1 << 3) != 0 {
                    paint.edge = Some(next(&mut i)?);
                }
                if mask & (1 << 4) != 0 {
                    paint.edge_width = Some(next_f32(&mut i)?);
                }
                if mask & (1 << 5) != 0 {
                    paint.edge_sides = Some(next_f32x4(&mut i)?);
                }
                if mask & (1 << 6) != 0 {
                    paint.highlight = Some(next(&mut i)?);
                }
                if mask & (1 << 7) != 0 {
                    paint.opacity = Some(next_f32(&mut i)?);
                }
                if mask & (1 << 8) != 0 {
                    paint.x = next_f32(&mut i)?;
                }
                if mask & (1 << 9) != 0 {
                    paint.y = next_f32(&mut i)?;
                }
                if mask & (1 << 10) != 0 {
                    paint.scale = Some(next_f32(&mut i)?);
                }
                if mask & (1 << 11) != 0 {
                    paint.brightness = Some(next_f32(&mut i)?);
                }
                if mask & (1 << 12) != 0 {
                    paint.color = Some(next(&mut i)?);
                }
                if mask & (1 << 13) != 0 {
                    paint.draw = Some(next_f32(&mut i)?);
                }
                if mask & (1 << 14) != 0 {
                    let c = next(&mut i)?;
                    paint.cursor = Some(match c {
                        0 => Cursor::Default,
                        1 => Cursor::Pointer,
                        2 => Cursor::Text,
                        3 => Cursor::NotAllowed,
                        4 => Cursor::Grab,
                        5 => Cursor::Grabbing,
                        6 => Cursor::Move,
                        7 => Cursor::ColResize,
                        8 => Cursor::RowResize,
                        9 => Cursor::EwResize,
                        10 => Cursor::NsResize,
                        11 => Cursor::NeswResize,
                        12 => Cursor::NwseResize,
                        _ => return Err(format!("invalid cursor code {c}")),
                    });
                }
                out.push(Op::SetPaint {
                    id,
                    paint: Box::new(paint),
                });
            }
            _ => return Err(format!("unknown binary opcode {op}")),
        }
    }
    Ok(())
}

pub(crate) fn dts(signatures: &[String]) -> String {
    let mut out = String::from(
        "// Generated from src/contract.rs and Vm::load. Do not edit by hand: `JSNIR_UPDATE=1 cargo test contract`.\n\
         declare var jsnir: {\n",
    );
    for s in signatures {
        out += &format!("    {s};\n");
    }
    out += "    /** Shared binary command buffer (Uint32 view) flushed with `jsnir.commit(words)`. */\n";
    out += "    cmd: Uint32Array;\n";
    out += "    /** Shared binary command buffer (Float32 view over the same memory as `cmd`). */\n";
    out += "    cmdF32: Float32Array;\n";
    out += "    /** Columns shared with Rust, indexed by node id. Read only. Do not keep them across callbacks: they can grow. */\n";
    out += "    columns: {\n";
    out += &columns_dts();
    out += "    };\n};\n";
    out += &crate::tree::ts_decls();
    out += "/** An app batch. Rust fills in the base. */\ntype Batch = { ops: Operation[] };\n";
    out
}
