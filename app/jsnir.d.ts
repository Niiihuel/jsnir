// Generated from src/contract.rs and Vm::load. Do not edit by hand: `JSNIR_UPDATE=1 cargo test contract`.
declare var jsnir: {
    set(id: number, value: number): void;
    batch(json: string): void;
    setText(id: number, text: string): void;
    readText(id: number): string | undefined;
    /** Columns shared with Rust, indexed by node id. Read only. Do not keep them across callbacks: they can grow. */
    columns: {
        /** Value of a label. */
        value: Int32Array;
        /** Number of children of a column or list. */
        children: Int32Array;
        /** Start of a field's selection, in characters. */
        selectionStart: Int32Array;
        /** End of a field's selection, in characters. */
        selectionEnd: Int32Array;
        /** Horizontal position in the window, in logical pixels (Taffy layout, rounded). */
        x: Int32Array;
        /** Vertical position in the window, in logical pixels. */
        y: Int32Array;
        /** Width computed by the layout. */
        width: Int32Array;
        /** Height computed by the layout. */
        height: Int32Array;
    };
};
type Axis = "column" | "row";
type Align = "stretch" | "start" | "center" | "end";
type Justify = "start" | "center" | "end" | "between";
type Position = "relative" | "absolute" | "fixed";
type TextAlign = "start" | "center" | "end";
type Side = "top" | "right" | "bottom" | "left";
type AnchorAlign = "start" | "center" | "end";
interface Anchor { node?: number; side?: Side; align?: AnchorAlign; gap?: number; flip?: boolean }
interface Style { width?: number | undefined; height?: number | undefined; grow?: number; shrink?: number | undefined; basis?: number | undefined; padding?: number | [number, number, number, number]; margin?: number | [number, number, number, number]; gap?: number; direction?: Axis; align?: Align | undefined; justify?: Justify; position?: Position; left?: number | undefined; top?: number | undefined; clip?: boolean; scroll?: boolean; group?: boolean; follow?: number | undefined; layer?: number | undefined; anchor?: Anchor | undefined; minWidth?: number | undefined; minHeight?: number | undefined; maxWidth?: number | undefined; maxHeight?: number | undefined }
type Property = "opacity" | "x" | "y" | "scale" | "brightness" | "background" | "edge" | "color" | "draw" | "left" | "width" | "height";
type Easing = "linear" | "ease" | "easeOut" | "easeInOut" | "spring" | "bezier";
interface Transition { property?: Property; duration?: number; delay?: number; easing?: Easing; bezier?: [number, number, number, number] | undefined; hold?: number }
interface Keyframe { offset?: number; value?: number | undefined; color?: number | undefined }
interface Animation { property?: Property; keyframes?: Keyframe[]; duration?: number; delay?: number; easing?: Easing; bezier?: [number, number, number, number] | undefined; iterations?: number | undefined; alternate?: boolean }
interface Shadow { y?: number; blur?: number; spread?: number; color?: number }
interface Variant { background?: number | undefined; gradient?: [number, number] | undefined; brightness?: number | undefined; scale?: number | undefined; opacity?: number | undefined; x?: number | undefined; y?: number | undefined; color?: number | undefined; edge?: number | undefined; height?: number | undefined; width?: number | undefined }
interface Starting { opacity?: number | undefined; x?: number | undefined; y?: number | undefined; scale?: number | undefined; draw?: number | undefined }
interface Scrollbar { width?: number; thumb?: number; track?: number | undefined; radius?: number; minThumb?: number; inset?: number; autoHide?: boolean }
interface Paint { background?: number | undefined; gradient?: [number, number] | undefined; radius?: number | [number, number, number, number]; mask?: string | undefined; edge?: number | undefined; edgeWidth?: number | undefined; edgeSides?: [number, number, number, number] | undefined; highlight?: number | undefined; shadows?: Shadow[]; opacity?: number | undefined; x?: number; y?: number; scale?: number | undefined; brightness?: number | undefined; color?: number | undefined; draw?: number | undefined; transitions?: Transition[]; animations?: Animation[] | undefined; starting?: Starting | undefined; hover?: Variant | undefined; active?: Variant | undefined; focus?: Variant | undefined; scrollbar?: Scrollbar | undefined }
interface Font { size?: number | undefined; weight?: number | undefined; lineHeight?: number | undefined; letterSpacing?: number; family?: string | undefined }
interface InlineImage { source?: string; width?: number; height?: number }
interface Span { text?: string; color?: number | undefined; weight?: number | undefined; size?: number | undefined; family?: string | undefined; underline?: boolean; image?: InlineImage | undefined }
type IconName = "arrowLeft" | "arrowRight" | "tick" | "checkCircle";
type Node =
    | { kind: "column"; style?: Style }
    | { kind: "list"; style?: Style }
    | { kind: "label"; value: number }
    | { kind: "button"; callback: number }
    | { kind: "text"; text: string; style?: Style; font?: Font; paint?: Paint; lines?: number | undefined; align?: TextAlign; spans?: Span[] | undefined }
    | { kind: "input"; text?: string; selection?: [number, number]; submit?: number; change?: number; placeholder?: string; placeholderColor?: number | undefined; multiline?: boolean; submitOnEnter?: boolean; style?: Style; font?: Font; paint?: Paint }
    | { kind: "box"; style?: Style; paint?: Paint; press?: number; disabled?: boolean; hover?: number; contextmenu?: number; dismiss?: number }
    | { kind: "icon"; icon: IconName; size: number; stroke: number; style?: Style; paint?: Paint }
    | { kind: "image"; source: string; style?: Style; paint?: Paint };
type Operation =
    | { op: "create"; id: number; parent?: number | undefined; before?: number | undefined; node: Node }
    | { op: "move"; id: number; before: number | undefined }
    | { op: "set"; id: number; value: number }
    | { op: "settext"; id: number; text: string }
    | { op: "setspans"; id: number; spans: Span[] }
    | { op: "setsource"; id: number; source: string }
    | { op: "setpaint"; id: number; paint: Paint }
    | { op: "setstyle"; id: number; style: Style }
    | { op: "setdisabled"; id: number; disabled: boolean }
    | { op: "remove"; id: number }
    | { op: "after"; ms: number; callback: number };
/** An app batch. Rust fills in the base. */
type Batch = { ops: Operation[] };
