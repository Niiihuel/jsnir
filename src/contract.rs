//! The single contract between Rust and JS. Each column and each native function is declared once,
//! and both lenses come from that declaration: the Rust code and the TypeScript type (`app/jsnir.d.ts`).
//!
//! Columns: numeric data on the Rust heap, one value per node id. JS reads them as `Int32Array`
//! with a single interpreter instruction, without calling Rust or converting values. Rust is the only
//! writer and writes only between callbacks, on the VM thread: reads and writes never overlap.
use crate::tree::{
    Align, Anchor, AnchorAlign, Animation, Axis, Cursor, Easing, Edges, Exiting, Font, Icon,
    InlineImage, InputPurpose, Justify, Keyframe, Node, Op, Paint, PlacementSubscription, Position,
    Property, Radius, ScrollAnchor, ScrollEdge, Scrollbar, Shadow, Side, Span, Starting, Style,
    TextAlign, Transition, Tree, Variant,
};

/// TypeScript type of a value that crosses the boundary.
#[cfg_attr(not(feature = "engine-quickjs"), allow(dead_code))]
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
impl<T: Ts, E> Ts for std::result::Result<T, E> {
    fn ts() -> String {
        T::ts()
    }
}
impl<T: Ts> Ts for Box<T> {
    fn ts() -> String {
        T::ts()
    }
}
impl<T: Ts> Ts for std::sync::Arc<T> {
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

/// `snake_case` Rust name → `camelCase` JS name, the convention on each side of the boundary.
#[cfg_attr(not(feature = "engine-quickjs"), allow(dead_code))]
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
            #[cfg_attr(not(feature = "engine-quickjs"), allow(dead_code))]
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
            #[cfg_attr(not(feature = "engine-quickjs"), allow(dead_code))]
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
            #[cfg_attr(not(feature = "engine-quickjs"), allow(dead_code))]
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
        #[cfg_attr(not(feature = "engine-quickjs"), allow(dead_code))]
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

/// Number of `u32` words (64 KiB) in the shared JS→Rust binary command buffer (`jsnir.cmd` / `jsnir.cmdF32`).
pub const CMD_WORDS: usize = 16_384;

pub(crate) const OP_SET: u32 = 1;
pub(crate) const OP_SET_DISABLED: u32 = 2;
pub(crate) const OP_REMOVE: u32 = 3;
pub(crate) const OP_MOVE: u32 = 4;
pub(crate) const OP_FOCUS: u32 = 5;
pub(crate) const OP_AFTER: u32 = 6;
pub(crate) const OP_SCROLL_TO: u32 = 7;
pub(crate) const OP_SET_STYLE: u32 = 8;
pub(crate) const OP_SET_PAINT: u32 = 9;
pub(crate) const OP_SET_TEXT: u32 = 10;
pub(crate) const OP_CREATE_BOX: u32 = 11;
pub(crate) const OP_CREATE_TEXT: u32 = 12;
pub(crate) const OP_CREATE_COLUMN: u32 = 13;
pub(crate) const OP_CREATE_LABEL: u32 = 14;
pub(crate) const OP_CREATE_BUTTON: u32 = 15;
pub(crate) const OP_CREATE_ICON: u32 = 16;
pub(crate) const OP_CREATE_IMAGE: u32 = 17;
pub(crate) const OP_SET_SOURCE: u32 = 18;
pub(crate) const OP_CALLBACK_GUARD: u32 = 19;
pub(crate) const OP_CREATE_INPUT: u32 = 20;
pub(crate) const OP_CREATE_LIST: u32 = 21;
pub(crate) const OP_SET_FIELD: u32 = 22;
pub(crate) const OP_SET_SPANS: u32 = 23;
pub(crate) const OP_VIRTUAL_LIST: u32 = 24;

#[inline]
fn next_word(words: &[u32], i: &mut usize) -> Result<u32, String> {
    let v = *words
        .get(*i)
        .ok_or_else(|| "truncated binary command".to_string())?;
    *i += 1;
    Ok(v)
}
#[inline]
fn next_u16(words: &[u32], i: &mut usize) -> Result<u16, String> {
    let v = next_word(words, i)?;
    u16::try_from(v).map_err(|_| format!("invalid u16 operand {v}"))
}
#[inline]
fn next_opt_u16(words: &[u32], i: &mut usize) -> Result<Option<u16>, String> {
    let raw = next_word(words, i)?;
    if raw == u32::MAX {
        Ok(None)
    } else {
        Ok(Some(
            u16::try_from(raw).map_err(|_| format!("invalid optional u16 operand {raw}"))?,
        ))
    }
}
#[inline]
fn next_f32(words: &[u32], i: &mut usize) -> Result<f32, String> {
    Ok(f32::from_bits(next_word(words, i)?))
}
#[inline]
fn next_f32x4(words: &[u32], i: &mut usize) -> Result<[f32; 4], String> {
    Ok([
        next_f32(words, i)?,
        next_f32(words, i)?,
        next_f32(words, i)?,
        next_f32(words, i)?,
    ])
}
#[inline]
fn take_str(words: &[u32], i: &mut usize, strings: &mut [String]) -> Result<String, String> {
    let idx = next_word(words, i)? as usize;
    let len = strings.len();
    let slot = strings
        .get_mut(idx)
        .ok_or_else(|| format!("string index {idx} out of bounds ({len})"))?;
    Ok(std::mem::take(slot))
}

fn decode_cursor(code: u32) -> Result<Cursor, String> {
    match code {
        0 => Ok(Cursor::Default),
        1 => Ok(Cursor::Pointer),
        2 => Ok(Cursor::Text),
        3 => Ok(Cursor::NotAllowed),
        4 => Ok(Cursor::Grab),
        5 => Ok(Cursor::Grabbing),
        6 => Ok(Cursor::Move),
        7 => Ok(Cursor::ColResize),
        8 => Ok(Cursor::RowResize),
        9 => Ok(Cursor::EwResize),
        10 => Ok(Cursor::NsResize),
        11 => Ok(Cursor::NeswResize),
        12 => Ok(Cursor::NwseResize),
        _ => Err(format!("invalid cursor code {code}")),
    }
}

fn decode_variant(words: &[u32], i: &mut usize) -> Result<Variant, String> {
    let mask = next_word(words, i)?;
    let mut v = Variant::default();
    if mask & (1 << 0) != 0 {
        v.background = Some(next_word(words, i)?);
    }
    if mask & (1 << 1) != 0 {
        v.gradient = Some([next_word(words, i)?, next_word(words, i)?]);
    }
    if mask & (1 << 2) != 0 {
        v.brightness = Some(next_f32(words, i)?);
    }
    if mask & (1 << 3) != 0 {
        v.scale = Some(next_f32(words, i)?);
    }
    if mask & (1 << 4) != 0 {
        v.opacity = Some(next_f32(words, i)?);
    }
    if mask & (1 << 5) != 0 {
        v.x = Some(next_f32(words, i)?);
    }
    if mask & (1 << 6) != 0 {
        v.y = Some(next_f32(words, i)?);
    }
    if mask & (1 << 7) != 0 {
        v.color = Some(next_word(words, i)?);
    }
    if mask & (1 << 8) != 0 {
        v.edge = Some(next_word(words, i)?);
    }
    if mask & (1 << 9) != 0 {
        v.height = Some(next_f32(words, i)?);
    }
    if mask & (1 << 10) != 0 {
        v.width = Some(next_f32(words, i)?);
    }
    if mask & (1 << 11) != 0 {
        v.underline = Some(next_word(words, i)? != 0);
    }
    if mask & (1 << 12) != 0 {
        v.cursor = Some(decode_cursor(next_word(words, i)?)?);
    }
    Ok(v)
}

fn decode_anchor(words: &[u32], i: &mut usize, strings: &mut [String]) -> Result<Anchor, String> {
    let node = next_u16(words, i)?;
    let aflags = next_word(words, i)?;
    let side = match aflags & 0x3 {
        0 => Side::Top,
        1 => Side::Right,
        2 => Side::Bottom,
        _ => Side::Left,
    };
    let align = match (aflags >> 2) & 0x3 {
        1 => AnchorAlign::Center,
        2 => AnchorAlign::End,
        _ => AnchorAlign::Start,
    };
    let flip = aflags & (1 << 4) != 0;
    let gap = next_f32(words, i)?;
    let placement = if aflags & (1 << 5) != 0 {
        let callback = next_u16(words, i)?;
        let token = take_str(words, i, strings)?;
        Some(PlacementSubscription { callback, token })
    } else {
        None
    };
    Ok(Anchor {
        node,
        side,
        align,
        gap,
        flip,
        placement,
    })
}

fn decode_shadows(words: &[u32], i: &mut usize) -> Result<Vec<Shadow>, String> {
    let len = next_word(words, i)? as usize;
    let mut shadows = Vec::with_capacity(len);
    for _ in 0..len {
        shadows.push(Shadow {
            y: next_f32(words, i)?,
            blur: next_f32(words, i)?,
            spread: next_f32(words, i)?,
            color: next_word(words, i)?,
        });
    }
    Ok(shadows)
}

fn decode_transitions(words: &[u32], i: &mut usize) -> Result<Vec<Transition>, String> {
    let len = next_word(words, i)? as usize;
    let mut transitions = Vec::with_capacity(len);
    for _ in 0..len {
        let hdr = next_word(words, i)?;
        let property = decode_property(hdr & 0xf)?;
        let easing = decode_easing((hdr >> 4) & 0x7)?;
        let has_bezier = hdr & (1 << 7) != 0;
        let duration = next_f32(words, i)?;
        let delay = next_f32(words, i)?;
        let hold = next_f32(words, i)?;
        let bezier = if has_bezier {
            Some(next_f32x4(words, i)?)
        } else {
            None
        };
        transitions.push(Transition {
            property,
            duration,
            delay,
            easing,
            bezier,
            hold,
        });
    }
    Ok(transitions)
}

fn decode_animations(words: &[u32], i: &mut usize) -> Result<Vec<Animation>, String> {
    let len = next_word(words, i)? as usize;
    let mut animations = Vec::with_capacity(len);
    for _ in 0..len {
        let hdr = next_word(words, i)?;
        let property = decode_property(hdr & 0xf)?;
        let easing = decode_easing((hdr >> 4) & 0x7)?;
        let has_bezier = hdr & (1 << 7) != 0;
        let has_iterations = hdr & (1 << 8) != 0;
        let alternate = hdr & (1 << 9) != 0;
        let duration = next_f32(words, i)?;
        let delay = next_f32(words, i)?;
        let bezier = if has_bezier {
            Some(next_f32x4(words, i)?)
        } else {
            None
        };
        let iterations = if has_iterations {
            Some(next_f32(words, i)?)
        } else {
            None
        };
        let kf_len = next_word(words, i)? as usize;
        let mut keyframes = Vec::with_capacity(kf_len);
        for _ in 0..kf_len {
            let kf_flags = next_word(words, i)?;
            let offset = next_f32(words, i)?;
            let value = if kf_flags & 1 != 0 {
                Some(next_f32(words, i)?)
            } else {
                None
            };
            let color = if kf_flags & 2 != 0 {
                Some(next_word(words, i)?)
            } else {
                None
            };
            keyframes.push(Keyframe {
                offset,
                value,
                color,
            });
        }
        animations.push(Animation {
            property,
            keyframes,
            duration,
            delay,
            easing,
            bezier,
            iterations,
            alternate,
        });
    }
    Ok(animations)
}

fn decode_starting(words: &[u32], i: &mut usize) -> Result<Starting, String> {
    let smask = next_word(words, i)?;
    Ok(Starting {
        opacity: (smask & (1 << 0) != 0)
            .then(|| next_f32(words, i))
            .transpose()?,
        x: (smask & (1 << 1) != 0)
            .then(|| next_f32(words, i))
            .transpose()?,
        y: (smask & (1 << 2) != 0)
            .then(|| next_f32(words, i))
            .transpose()?,
        scale: (smask & (1 << 3) != 0)
            .then(|| next_f32(words, i))
            .transpose()?,
        draw: (smask & (1 << 4) != 0)
            .then(|| next_f32(words, i))
            .transpose()?,
    })
}

fn decode_exiting(words: &[u32], i: &mut usize) -> Result<Exiting, String> {
    let emask = next_word(words, i)?;
    Ok(Exiting {
        opacity: (emask & (1 << 0) != 0)
            .then(|| next_f32(words, i))
            .transpose()?,
        x: (emask & (1 << 1) != 0)
            .then(|| next_f32(words, i))
            .transpose()?,
        y: (emask & (1 << 2) != 0)
            .then(|| next_f32(words, i))
            .transpose()?,
        scale: (emask & (1 << 3) != 0)
            .then(|| next_f32(words, i))
            .transpose()?,
    })
}

fn decode_scrollbar(words: &[u32], i: &mut usize) -> Result<Scrollbar, String> {
    let sb_flags = next_word(words, i)?;
    let width = next_f32(words, i)?;
    let thumb = next_word(words, i)?;
    let radius = next_f32(words, i)?;
    let min_thumb = next_f32(words, i)?;
    let inset = next_f32(words, i)?;
    let track = if sb_flags & 1 != 0 {
        Some(next_word(words, i)?)
    } else {
        None
    };
    let auto_hide = sb_flags & 2 != 0;
    Ok(Scrollbar {
        width,
        thumb,
        track,
        radius,
        min_thumb,
        inset,
        auto_hide,
    })
}

fn decode_property(code: u32) -> Result<Property, String> {
    match code {
        0 => Ok(Property::Opacity),
        1 => Ok(Property::X),
        2 => Ok(Property::Y),
        3 => Ok(Property::Scale),
        4 => Ok(Property::Brightness),
        5 => Ok(Property::Background),
        6 => Ok(Property::Edge),
        7 => Ok(Property::Color),
        8 => Ok(Property::Draw),
        9 => Ok(Property::Left),
        10 => Ok(Property::Width),
        11 => Ok(Property::Height),
        12 => Ok(Property::Radius),
        _ => Err(format!("invalid property code {code}")),
    }
}

fn decode_easing(code: u32) -> Result<Easing, String> {
    match code {
        0 => Ok(Easing::Linear),
        1 => Ok(Easing::Ease),
        2 => Ok(Easing::EaseOut),
        3 => Ok(Easing::EaseInOut),
        4 => Ok(Easing::Spring),
        5 => Ok(Easing::Bezier),
        _ => Err(format!("invalid easing code {code}")),
    }
}

fn decode_input_purpose(code: u32) -> Result<InputPurpose, String> {
    match code {
        0 => Ok(InputPurpose::Normal),
        1 => Ok(InputPurpose::Password),
        2 => Ok(InputPurpose::Search),
        3 => Ok(InputPurpose::Email),
        4 => Ok(InputPurpose::Number),
        _ => Err(format!("invalid input purpose code {code}")),
    }
}

fn decode_style(words: &[u32], i: &mut usize, strings: &mut [String]) -> Result<Style, String> {
    let mask = next_word(words, i)?;
    let flags = next_word(words, i)?;
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
        style.width = Some(next_f32(words, i)?);
    }
    if mask & (1 << 1) != 0 {
        style.height = Some(next_f32(words, i)?);
    }
    if mask & (1 << 2) != 0 {
        style.grow = next_f32(words, i)?;
    }
    if mask & (1 << 3) != 0 {
        style.shrink = Some(next_f32(words, i)?);
    }
    if mask & (1 << 4) != 0 {
        style.basis = Some(next_f32(words, i)?);
    }
    if mask & (1 << 5) != 0 {
        style.padding = Edges::Sides(next_f32x4(words, i)?);
    }
    if mask & (1 << 6) != 0 {
        style.margin = Edges::Sides(next_f32x4(words, i)?);
    }
    if mask & (1 << 7) != 0 {
        style.gap = next_f32(words, i)?;
    }
    if mask & (1 << 8) != 0 {
        style.left = Some(next_f32(words, i)?);
    }
    if mask & (1 << 9) != 0 {
        style.top = Some(next_f32(words, i)?);
    }
    if mask & (1 << 10) != 0 {
        style.follow = Some(next_u16(words, i)?);
    }
    if mask & (1 << 11) != 0 {
        style.layer = Some(next_word(words, i)? as i32);
    }
    if mask & (1 << 12) != 0 {
        style.min_width = Some(next_f32(words, i)?);
    }
    if mask & (1 << 13) != 0 {
        style.min_height = Some(next_f32(words, i)?);
    }
    if mask & (1 << 14) != 0 {
        style.max_width = Some(next_f32(words, i)?);
    }
    if mask & (1 << 15) != 0 {
        style.max_height = Some(next_f32(words, i)?);
    }
    if mask & (1 << 16) != 0 {
        style.anchor = Some(decode_anchor(words, i, strings)?);
    }
    Ok(style)
}

fn decode_paint(words: &[u32], i: &mut usize, strings: &mut [String]) -> Result<Paint, String> {
    let mask = next_word(words, i)?;
    let mut paint = Paint::default();
    if mask & (1 << 0) != 0 {
        paint.background = Some(next_word(words, i)?);
    }
    if mask & (1 << 1) != 0 {
        paint.gradient = Some([next_word(words, i)?, next_word(words, i)?]);
    }
    if mask & (1 << 2) != 0 {
        paint.radius = Radius::Corners(next_f32x4(words, i)?);
    }
    if mask & (1 << 3) != 0 {
        paint.edge = Some(next_word(words, i)?);
    }
    if mask & (1 << 4) != 0 {
        paint.edge_width = Some(next_f32(words, i)?);
    }
    if mask & (1 << 5) != 0 {
        paint.edge_sides = Some(next_f32x4(words, i)?);
    }
    if mask & (1 << 6) != 0 {
        paint.highlight = Some(next_word(words, i)?);
    }
    if mask & (1 << 7) != 0 {
        paint.opacity = Some(next_f32(words, i)?);
    }
    if mask & (1 << 8) != 0 {
        paint.x = next_f32(words, i)?;
    }
    if mask & (1 << 9) != 0 {
        paint.y = next_f32(words, i)?;
    }
    if mask & (1 << 10) != 0 {
        paint.scale = Some(next_f32(words, i)?);
    }
    if mask & (1 << 11) != 0 {
        paint.brightness = Some(next_f32(words, i)?);
    }
    if mask & (1 << 12) != 0 {
        paint.color = Some(next_word(words, i)?);
    }
    if mask & (1 << 13) != 0 {
        paint.draw = Some(next_f32(words, i)?);
    }
    if mask & (1 << 14) != 0 {
        paint.cursor = Some(decode_cursor(next_word(words, i)?)?);
    }
    if mask & (1 << 15) != 0 {
        paint.hover = Some(decode_variant(words, i)?);
    }
    if mask & (1 << 16) != 0 {
        paint.active = Some(decode_variant(words, i)?);
    }
    if mask & (1 << 17) != 0 {
        paint.focus = Some(decode_variant(words, i)?);
    }
    if mask & (1 << 18) != 0 {
        paint.mask = Some(take_str(words, i, strings)?);
    }
    if mask & (1 << 19) != 0 {
        paint.shadows = decode_shadows(words, i)?;
    }
    if mask & (1 << 20) != 0 {
        paint.transitions = decode_transitions(words, i)?;
    }
    if mask & (1 << 21) != 0 {
        paint.animations = Some(decode_animations(words, i)?);
    }
    if mask & (1 << 22) != 0 {
        paint.starting = Some(decode_starting(words, i)?);
    }
    if mask & (1 << 23) != 0 {
        paint.scrollbar = Some(decode_scrollbar(words, i)?);
    }
    if mask & (1 << 24) != 0 {
        paint.exiting = Some(decode_exiting(words, i)?);
    }
    Ok(paint)
}

fn decode_spans(words: &[u32], i: &mut usize, strings: &mut [String]) -> Result<Vec<Span>, String> {
    let len = next_word(words, i)? as usize;
    let mut spans = Vec::with_capacity(len);
    for _ in 0..len {
        let mask = next_word(words, i)?;
        let mut span = Span {
            underline: mask & (1 << 5) != 0,
            italic: mask & (1 << 6) != 0,
            strike: mask & (1 << 7) != 0,
            ..Span::default()
        };
        if mask & (1 << 0) != 0 {
            span.text = take_str(words, i, strings)?;
        }
        if mask & (1 << 1) != 0 {
            span.color = Some(next_word(words, i)?);
        }
        if mask & (1 << 2) != 0 {
            span.weight = Some(next_u16(words, i)?);
        }
        if mask & (1 << 3) != 0 {
            span.size = Some(next_f32(words, i)?);
        }
        if mask & (1 << 4) != 0 {
            span.family = Some(take_str(words, i, strings)?);
        }
        if mask & (1 << 8) != 0 {
            span.background = Some(next_word(words, i)?);
        }
        if mask & (1 << 9) != 0 {
            span.background_radius = next_f32(words, i)?;
        }
        if mask & (1 << 10) != 0 {
            let source = take_str(words, i, strings)?;
            let width = next_f32(words, i)?;
            let height = next_f32(words, i)?;
            span.image = Some(InlineImage {
                source,
                width,
                height,
            });
        }
        spans.push(span);
    }
    Ok(spans)
}

fn decode_font(words: &[u32], i: &mut usize, strings: &mut [String]) -> Result<Font, String> {
    let mask = next_word(words, i)?;
    let mut font = Font::default();
    if mask & (1 << 0) != 0 {
        font.size = Some(next_f32(words, i)?);
    }
    if mask & (1 << 1) != 0 {
        font.weight = Some(next_u16(words, i)?);
    }
    if mask & (1 << 2) != 0 {
        font.line_height = Some(next_f32(words, i)?);
    }
    if mask & (1 << 3) != 0 {
        font.letter_spacing = next_f32(words, i)?;
    }
    if mask & (1 << 4) != 0 {
        font.family = Some(take_str(words, i, strings)?);
    }
    Ok(font)
}

pub fn decode_commands(
    words: &[u32],
    strings: &mut [String],
    out: &mut Vec<Op>,
) -> Result<(), String> {
    let mut i = 0usize;
    while i < words.len() {
        let op = next_word(words, &mut i)?;
        match op {
            OP_SET => {
                let id = next_u16(words, &mut i)?;
                let value = next_u16(words, &mut i)?;
                out.push(Op::Set { id, value });
            }
            OP_SET_DISABLED => {
                let id = next_u16(words, &mut i)?;
                let disabled = next_word(words, &mut i)? != 0;
                out.push(Op::SetDisabled { id, disabled });
            }
            OP_REMOVE => {
                let id = next_u16(words, &mut i)?;
                out.push(Op::Remove { id });
            }
            OP_MOVE => {
                let id = next_u16(words, &mut i)?;
                let before = next_opt_u16(words, &mut i)?;
                out.push(Op::Move { id, before });
            }
            OP_FOCUS => {
                let id = next_u16(words, &mut i)?;
                out.push(Op::Focus { id });
            }
            OP_AFTER => {
                let callback = next_u16(words, &mut i)?;
                let ms = next_f32(words, &mut i)?;
                out.push(Op::After { ms, callback });
            }
            OP_SCROLL_TO => {
                let id = next_u16(words, &mut i)?;
                let flags = next_word(words, &mut i)?;
                let raw_offset = next_f32(words, &mut i)?;
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
                let id = next_u16(words, &mut i)?;
                let style = decode_style(words, &mut i, strings)?;
                out.push(Op::SetStyle { id, style });
            }
            OP_SET_PAINT => {
                let id = next_u16(words, &mut i)?;
                let paint = decode_paint(words, &mut i, strings)?;
                out.push(Op::SetPaint {
                    id,
                    paint: std::sync::Arc::new(paint),
                });
            }
            OP_SET_TEXT => {
                let id = next_u16(words, &mut i)?;
                let text = take_str(words, &mut i, strings)?;
                out.push(Op::SetText { id, text });
            }
            OP_CREATE_BOX => {
                let id = next_u16(words, &mut i)?;
                let parent = next_opt_u16(words, &mut i)?;
                let before = next_opt_u16(words, &mut i)?;
                let flags = next_word(words, &mut i)?;
                let disabled = flags & (1 << 0) != 0;
                let press = if flags & (1 << 1) != 0 { next_u16(words, &mut i)? } else { 0 };
                let hover = if flags & (1 << 2) != 0 { next_u16(words, &mut i)? } else { 0 };
                let contextmenu = if flags & (1 << 3) != 0 { next_u16(words, &mut i)? } else { 0 };
                let dismiss = if flags & (1 << 4) != 0 { next_u16(words, &mut i)? } else { 0 };
                let scroll = if flags & (1 << 5) != 0 { next_u16(words, &mut i)? } else { 0 };
                let drag = if flags & (1 << 6) != 0 { next_u16(words, &mut i)? } else { 0 };
                let style = if flags & (1 << 7) != 0 {
                    decode_style(words, &mut i, strings)?
                } else {
                    Style::default()
                };
                let paint = if flags & (1 << 8) != 0 {
                    std::sync::Arc::new(decode_paint(words, &mut i, strings)?)
                } else {
                    std::sync::Arc::default()
                };
                out.push(Op::Create {
                    id,
                    parent,
                    before,
                    node: Node::Box {
                        children: Vec::new(),
                        style,
                        paint,
                        press,
                        disabled,
                        hover,
                        contextmenu,
                        dismiss,
                        scroll,
                        drag,
                    },
                });
            }
            OP_CREATE_TEXT => {
                let id = next_u16(words, &mut i)?;
                let parent = next_opt_u16(words, &mut i)?;
                let before = next_opt_u16(words, &mut i)?;
                let text = take_str(words, &mut i, strings)?;
                let flags = next_word(words, &mut i)?;
                let align = match flags & 0x3 {
                    1 => TextAlign::Center,
                    2 => TextAlign::End,
                    _ => TextAlign::Start,
                };
                let lines = if flags & (1 << 2) != 0 {
                    Some(next_u16(words, &mut i)?)
                } else {
                    None
                };
                let font = if flags & (1 << 3) != 0 {
                    decode_font(words, &mut i, strings)?
                } else {
                    Font::default()
                };
                let style = if flags & (1 << 4) != 0 {
                    decode_style(words, &mut i, strings)?
                } else {
                    Style::default()
                };
                let paint = if flags & (1 << 5) != 0 {
                    std::sync::Arc::new(decode_paint(words, &mut i, strings)?)
                } else {
                    std::sync::Arc::default()
                };
                let spans = if flags & (1 << 6) != 0 {
                    Some(decode_spans(words, &mut i, strings)?)
                } else {
                    None
                };
                out.push(Op::Create {
                    id,
                    parent,
                    before,
                    node: Node::Text {
                        text,
                        style,
                        font,
                        paint,
                        lines,
                        align,
                        spans,
                    },
                });
            }
            OP_CREATE_COLUMN => {
                let id = next_u16(words, &mut i)?;
                let parent = next_opt_u16(words, &mut i)?;
                let before = next_opt_u16(words, &mut i)?;
                let flags = next_word(words, &mut i)?;
                let style = if flags & 1 != 0 {
                    decode_style(words, &mut i, strings)?
                } else {
                    Style::default()
                };
                out.push(Op::Create {
                    id,
                    parent,
                    before,
                    node: Node::Column {
                        children: Vec::new(),
                        style,
                    },
                });
            }
            OP_CREATE_LABEL => {
                let id = next_u16(words, &mut i)?;
                let parent = next_opt_u16(words, &mut i)?;
                let before = next_opt_u16(words, &mut i)?;
                let value = next_u16(words, &mut i)?;
                out.push(Op::Create {
                    id,
                    parent,
                    before,
                    node: Node::Label { value },
                });
            }
            OP_CREATE_BUTTON => {
                let id = next_u16(words, &mut i)?;
                let parent = next_opt_u16(words, &mut i)?;
                let before = next_opt_u16(words, &mut i)?;
                let callback = next_u16(words, &mut i)?;
                out.push(Op::Create {
                    id,
                    parent,
                    before,
                    node: Node::Button { callback },
                });
            }
            OP_CREATE_ICON => {
                let id = next_u16(words, &mut i)?;
                let parent = next_opt_u16(words, &mut i)?;
                let before = next_opt_u16(words, &mut i)?;
                let icon = match next_word(words, &mut i)? {
                    0 => Icon::ArrowLeft,
                    1 => Icon::ArrowRight,
                    2 => Icon::Tick,
                    3 => Icon::CheckCircle,
                    code => return Err(format!("invalid icon code {code}")),
                };
                let size = next_f32(words, &mut i)?;
                let stroke = next_f32(words, &mut i)?;
                let flags = next_word(words, &mut i)?;
                let style = if flags & 1 != 0 {
                    decode_style(words, &mut i, strings)?
                } else {
                    Style::default()
                };
                let paint = if flags & 2 != 0 {
                    std::sync::Arc::new(decode_paint(words, &mut i, strings)?)
                } else {
                    std::sync::Arc::default()
                };
                out.push(Op::Create {
                    id,
                    parent,
                    before,
                    node: Node::Icon {
                        icon,
                        size,
                        stroke,
                        style,
                        paint,
                    },
                });
            }
            OP_CREATE_IMAGE => {
                let id = next_u16(words, &mut i)?;
                let parent = next_opt_u16(words, &mut i)?;
                let before = next_opt_u16(words, &mut i)?;
                let source = take_str(words, &mut i, strings)?;
                let flags = next_word(words, &mut i)?;
                let style = if flags & 1 != 0 {
                    decode_style(words, &mut i, strings)?
                } else {
                    Style::default()
                };
                let paint = if flags & 2 != 0 {
                    std::sync::Arc::new(decode_paint(words, &mut i, strings)?)
                } else {
                    std::sync::Arc::default()
                };
                out.push(Op::Create {
                    id,
                    parent,
                    before,
                    node: Node::Image {
                        source,
                        style,
                        paint,
                    },
                });
            }
            OP_SET_SOURCE => {
                let id = next_u16(words, &mut i)?;
                let source = take_str(words, &mut i, strings)?;
                out.push(Op::SetSource { id, source });
            }
            OP_CALLBACK_GUARD => {
                let callback = next_u16(words, &mut i)?;
                let raw = next_word(words, &mut i)?;
                let token = if raw == u32::MAX {
                    None
                } else {
                    let idx = raw as usize;
                    let len = strings.len();
                    let slot = strings
                        .get_mut(idx)
                        .ok_or_else(|| format!("string index {idx} out of bounds ({len})"))?;
                    Some(std::mem::take(slot))
                };
                out.push(Op::CallbackGuard { callback, token });
            }
            OP_CREATE_INPUT => {
                let id = next_u16(words, &mut i)?;
                let parent = next_opt_u16(words, &mut i)?;
                let before = next_opt_u16(words, &mut i)?;
                let flags = next_word(words, &mut i)?;
                let multiline = flags & (1 << 0) != 0;
                let submit_on_enter = flags & (1 << 1) != 0;
                let retain_on_submit = flags & (1 << 2) != 0;
                let purpose = decode_input_purpose((flags >> 3) & 0x7)?;
                let text = if flags & (1 << 6) != 0 {
                    take_str(words, &mut i, strings)?
                } else {
                    String::new()
                };
                let selection = if flags & (1 << 7) != 0 {
                    [next_word(words, &mut i)?, next_word(words, &mut i)?]
                } else {
                    [0, 0]
                };
                let submit = if flags & (1 << 8) != 0 {
                    next_u16(words, &mut i)?
                } else {
                    0
                };
                let change = if flags & (1 << 9) != 0 {
                    next_u16(words, &mut i)?
                } else {
                    0
                };
                let placeholder = if flags & (1 << 10) != 0 {
                    take_str(words, &mut i, strings)?
                } else {
                    String::new()
                };
                let placeholder_color = if flags & (1 << 11) != 0 {
                    Some(next_word(words, &mut i)?)
                } else {
                    None
                };
                let font = if flags & (1 << 12) != 0 {
                    decode_font(words, &mut i, strings)?
                } else {
                    Font::default()
                };
                let style = if flags & (1 << 13) != 0 {
                    decode_style(words, &mut i, strings)?
                } else {
                    Style::default()
                };
                let paint = if flags & (1 << 14) != 0 {
                    std::sync::Arc::new(decode_paint(words, &mut i, strings)?)
                } else {
                    std::sync::Arc::default()
                };
                out.push(Op::Create {
                    id,
                    parent,
                    before,
                    node: Node::Input {
                        text,
                        selection,
                        submit,
                        change,
                        placeholder,
                        placeholder_color,
                        multiline,
                        submit_on_enter,
                        retain_on_submit,
                        purpose,
                        style,
                        font,
                        paint,
                    },
                });
            }
            OP_CREATE_LIST => {
                let id = next_u16(words, &mut i)?;
                let parent = next_opt_u16(words, &mut i)?;
                let before = next_opt_u16(words, &mut i)?;
                let flags = next_word(words, &mut i)?;
                let scroll = if flags & 1 != 0 {
                    next_u16(words, &mut i)?
                } else {
                    0
                };
                let style = if flags & 2 != 0 {
                    decode_style(words, &mut i, strings)?
                } else {
                    Style::default()
                };
                out.push(Op::Create {
                    id,
                    parent,
                    before,
                    node: Node::List {
                        children: Vec::new(),
                        style,
                        scroll,
                    },
                });
            }
            OP_SET_FIELD => {
                let id = next_u16(words, &mut i)?;
                let text = take_str(words, &mut i, strings)?;
                let expected = take_str(words, &mut i, strings)?;
                out.push(Op::SetField { id, expected, text });
            }
            OP_SET_SPANS => {
                let id = next_u16(words, &mut i)?;
                let spans = decode_spans(words, &mut i, strings)?;
                out.push(Op::SetSpans { id, spans });
            }
            OP_VIRTUAL_LIST => {
                let id = next_u16(words, &mut i)?;
                let name = take_str(words, &mut i, strings)?;
                out.push(Op::VirtualList { id, name });
            }
            _ => return Err(format!("unknown binary opcode {op}")),
        }
    }
    Ok(())
}

#[cfg_attr(not(feature = "engine-quickjs"), allow(dead_code))]
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
