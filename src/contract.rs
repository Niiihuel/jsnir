//! The single contract between Rust and JS. Each column and each native function is declared once,
//! and both lenses come from that declaration: the Rust code and the TypeScript type (`app/jsnir.d.ts`).
//!
//! Columns: numeric data on the Rust heap, one value per node id. JS reads them as `Int32Array`
//! with a single interpreter instruction, without calling Rust or converting values. Rust is the only
//! writer and writes only between callbacks, on the VM thread: reads and writes never overlap.
use crate::tree::{Node, Tree};
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
/// Shares values with JS: returns the view JS sees and the buffer Rust keeps for writing.
/// QuickJS-ng 0.16.2 silently ignores JS writes to an immutable buffer.
pub(crate) fn share<'js>(
    c: &Ctx<'js>,
    values: Vec<i32>,
) -> (TypedArray<'js, i32>, Persistent<ArrayBuffer<'static>>) {
    let ab = ArrayBuffer::from_source_immutable(c.clone(), Column::new(values))
        .expect("column out of memory");
    let view = TypedArray::<i32>::from_arraybuffer(ab.clone()).expect("column view");
    // Persistent keeps the GC from freeing the block while Rust writes it.
    (view, Persistent::save(c, ab))
}
/// Writes to a shared column. Call it only while no JS runs, on the VM thread.
pub(crate) fn write_column(ab: &ArrayBuffer, writes: impl Iterator<Item = (u16, i32)>) {
    // If JS detached the buffer, `as_raw` returns None and nothing is written to freed memory.
    let raw = ab.as_raw().expect("column detached");
    let (ptr, len) = (raw.as_ptr() as *mut i32, raw.len() / 4);
    for (id, value) in writes {
        assert!((id as usize) < len, "id outside the column");
        // SAFETY: the pointer is valid because JS is not running; the index is inside the block.
        unsafe { ptr.add(id as usize).write(value) }
    }
}

/// A VM's columns, installed as `jsnir.columns`. They grow when a larger id appears.
pub(crate) struct Columns {
    buffers: Vec<Persistent<ArrayBuffer<'static>>>,
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
            let (js, rust) = share(c, rows.iter().map(|r| r[k]).collect());
            target.set(*name, js)?;
            self.buffers.push(rust);
        }
        self.len = len;
        Ok(())
    }
    /// Copies the given nodes of the view to the columns, or all of them with `None`. Only while no JS runs.
    pub(crate) fn sync(&mut self, c: &Ctx, view: &Tree, ids: Option<&[u16]>) {
        let needed = view.nodes.end();
        if ids.is_none() || needed > self.len {
            // shortcut: growing copies everything; trees of hundreds of thousands of nodes would want growth by blocks.
            let len = if needed > self.len {
                needed.max(self.len * 2)
            } else {
                self.len
            };
            self.rebuild(c, view, len).expect("columns");
            return;
        }
        // Each node is projected once for all columns.
        let rows: Vec<_> = ids
            .unwrap()
            .iter()
            .map(|&id| (id, project(view, id)))
            .collect();
        for (k, persistent) in self.buffers.iter().enumerate() {
            let ab = persistent.clone().restore(c).expect("column");
            write_column(&ab, rows.iter().map(|(id, row)| (*id, row[k])));
        }
    }
}
pub(crate) fn dts(signatures: &[String]) -> String {
    let mut out = String::from(
        "// Generated from src/contract.rs and Vm::load. Do not edit by hand: `JSNIR_UPDATE=1 cargo test contract`.\n\
         declare var jsnir: {\n",
    );
    for s in signatures {
        out += &format!("    {s};\n");
    }
    out += "    /** Columns shared with Rust, indexed by node id. Read only. Do not keep them across callbacks: they can grow. */\n";
    out += "    columns: {\n";
    out += &columns_dts();
    out += "    };\n};\n";
    out += &crate::tree::ts_decls();
    out += "/** An app batch. Rust fills in the base. */\ntype Batch = { ops: Operation[] };\n";
    out
}
