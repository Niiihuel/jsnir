// JSNIR SDK for writing interfaces in TSX.
//
// Components run once and signals are fine-grained: each change emits exactly its own operation
// (`jsnir.set`, `jsnir.setText`, `setpaint`...), and new structure crosses in a single batch per subtree
// (`jsnir.batch`). There is no virtual tree to diff: in QuickJS every bytecode instruction costs, and
// diffing trees in the interpreter would cost more than the bridge. What the UI writes (the draft, the
// selection) is read from the Rust side, its only owner. Transitions, hover and press states run in Rust
// without calling JS. The wire types live in jsnir.d.ts, generated from Rust; this file is checked against them.

// ---------- Live reload ----------
// Before swapping the VM, Rust asks for `jsnirReloadState()`; the new VM receives that text in
// `__jsnirPreviousState` before the app runs. Each signal and list gets its value back when its position in
// creation order and its initial value match: same order and same initializer, same signal.
// What the UI writes (draft, selection, scroll) does not go through here: it lives in Rust and is kept.
declare var __jsnirPreviousState: string | undefined;
interface SavedValue {
    initial: string | undefined;
    value: unknown;
}
var previousState: SavedValue[] = typeof __jsnirPreviousState === "string" ? JSON.parse(__jsnirPreviousState) : [];
var restorables: Array<{ initial: string | undefined; read: () => unknown }> = [];
function restore<T>(initial: T, read: () => unknown): T {
    var key = JSON.stringify(initial);
    var previous = previousState[restorables.length];
    restorables.push({ initial: key, read: read });
    return previous !== undefined && key !== undefined && previous.initial === key ? (previous.value as T) : initial;
}
function jsnirReloadState(): string {
    return JSON.stringify(
        restorables.map(function (r) {
            return { initial: r.initial, value: r.read() };
        }),
    );
}

// ---------- Startup arguments ----------
declare var __jsnirArgs: string | undefined;
/** Arguments the host passed at startup (a JSON object), or an empty object. */
function args<T extends object>(): Partial<T> {
    return typeof __jsnirArgs === "string" && __jsnirArgs !== "" ? JSON.parse(__jsnirArgs) : {};
}

// ---------- Scopes ----------
// Every effect and callback belongs to the scope that was mounting when it was created. Removing a
// subtree disposes its scope, so its effects stop writing to nodes that no longer exist.

interface Scope {
    effects: Effect[];
    callbacks: number[];
    children: Scope[];
}
var currentScope: Scope = { effects: [], callbacks: [], children: [] };
function inScope<T>(scope: Scope, body: () => T): T {
    var previous = currentScope;
    currentScope = scope;
    try {
        return body();
    } finally {
        currentScope = previous;
    }
}
function dispose(scope: Scope): void {
    for (var i = 0; i < scope.effects.length; i++) scope.effects[i].disposed = true;
    for (var j = 0; j < scope.callbacks.length; j++) unregister(scope.callbacks[j]);
    for (var k = 0; k < scope.children.length; k++) dispose(scope.children[k]);
    scope.effects = [];
    scope.callbacks = [];
    scope.children = [];
}

// ---------- Signals ----------

interface Effect {
    (): void;
    disposed?: boolean;
}
var observer: Effect | null = null;
var queued: Effect[] = [];
var batching = false;
var signals = new WeakSet<object>();

/** Reactive value. Reading `value` inside an effect subscribes to it; assigning it updates its dependents. */
interface Signal<T> {
    value: T;
    update(change: (current: T) => T): void;
}
/** A fixed value, a signal, or a function that reads signals. */
type Source<T> = T | Signal<T> | (() => T);

function signal<T>(initial: T): Signal<T> {
    var current: T = restore(initial, function () {
        return current;
    });
    var subscribers: Effect[] = [];
    var s: Signal<T> = {
        get value() {
            if (observer !== null && subscribers.indexOf(observer) < 0) subscribers.push(observer);
            return current;
        },
        set value(next: T) {
            if (next === current) return;
            current = next;
            subscribers = subscribers.filter(function (e) {
                return !e.disposed;
            });
            for (var i = 0; i < subscribers.length; i++) schedule(subscribers[i]);
        },
        update(change) {
            s.value = change(current);
        },
    };
    signals.add(s);
    return s;
}
function schedule(e: Effect): void {
    if (queued.indexOf(e) < 0) queued.push(e);
    if (!batching) flush();
}
// Runs each queued effect once. Inside an event it waits until the callback ends.
function flush(): void {
    var previous = batching;
    batching = true;
    try {
        while (queued.length > 0) {
            var next = queued.shift() as Effect;
            if (!next.disposed) next();
        }
    } finally {
        batching = previous;
    }
}
function effect(body: () => void): void {
    var e: Effect = function () {
        var previous = observer;
        observer = e;
        try {
            body();
        } finally {
            observer = previous;
        }
    };
    currentScope.effects.push(e);
    e();
}
function isSignal(source: unknown): boolean {
    return typeof source === "object" && source !== null && signals.has(source);
}
function read<T>(source: Source<T>): T {
    if (typeof source === "function") return (source as () => T)();
    if (isSignal(source)) return (source as Signal<T>).value;
    return source as T;
}
/** Initial value of a source. If it is reactive, `apply` receives every later change. */
function bind<T>(source: Source<T>, apply: (value: T) => void): T {
    if (typeof source !== "function" && !isSignal(source)) return source as T;
    var first = true;
    var initial: T | undefined;
    effect(function () {
        var v = read(source);
        if (first) {
            first = false;
            initial = v;
        } else apply(v);
    });
    return initial as T;
}

// ---------- Lists ----------

/** List with explicit mutations: each `push` creates a single subtree, without diffing the whole list. */
interface ListSignal<T> {
    readonly items: readonly T[];
    push(item: T): void;
}
var listeners = new WeakMap<object, Array<(item: never) => void>>();
function list<T>(initial: T[] = []): ListSignal<T> {
    var items: T[] = restore(initial, function () {
        return items;
    }).slice();
    var onPush: Array<(item: T) => void> = [];
    var l: ListSignal<T> = {
        items: items,
        push(item) {
            items.push(item);
            for (var i = 0; i < onPush.length; i++) onPush[i](item);
        },
    };
    listeners.set(l, onPush);
    return l;
}

// ---------- Elements and operations ----------

/** What a component returns: it knows how to create its node and children inside a batch. */
interface Element {
    mount(parent: number | undefined, ops: Operation[]): void;
}
declare namespace JSX {
    type Element = globalThis.Element;
    interface ElementChildrenAttribute {
        children: {};
    }
}
type Child = Element | null | undefined | false;
type Children = Child | Children[];

var nextId = 1;
var freeIds: number[] = [];
function newId(): number {
    if (freeIds.length > 0) return freeIds.pop() as number;
    if (nextId > 65535) throw new RangeError("JSNIR supports up to 65,535 live nodes");
    return nextId++;
}
var handlers: Array<((text: string) => void) | undefined> = [];
var freeCallbacks: number[] = [];
function register(handler: (text: string) => void): number {
    var id = freeCallbacks.length > 0 ? (freeCallbacks.pop() as number) : handlers.length + 1;
    handlers[id - 1] = handler;
    currentScope.callbacks.push(id);
    return id;
}
function unregister(id: number): void {
    if (handlers[id - 1] === undefined) return;
    handlers[id - 1] = undefined;
    freeCallbacks.push(id);
}
function send(ops: Operation[]): void {
    var batch: Batch = { ops: ops };
    jsnir.batch(JSON.stringify(batch));
}
function mountChildren(children: Children | undefined, parent: number, ops: Operation[]): void {
    if (Array.isArray(children)) for (var i = 0; i < children.length; i++) mountChildren(children[i], parent, ops);
    else if (children) children.mount(parent, ops);
}
// JSX factory (`jsxFactory`). The component runs here, exactly once.
function h(component: (props: any) => Element, props: object | null, ...children: unknown[]): Element {
    var nested = children.length === 0 ? undefined : children.length === 1 ? children[0] : children;
    return component(nested === undefined ? { ...props } : { ...props, children: nested });
}

/** A mounted subtree: its root id, and `remove` to take it out with everything it bound. */
interface Mounted {
    id: number;
    remove(): void;
}
/** Mounts `element` under `parent` in its own scope and sends it as one batch, before the sibling `before` if given. */
function mountInto(parent: number, element: Element, before?: number): Mounted {
    var scope: Scope = { effects: [], callbacks: [], children: [] };
    currentScope.children.push(scope);
    var ops: Operation[] = [];
    inScope(scope, function () {
        element.mount(parent, ops);
    });
    var first = ops[0];
    if (first === undefined || first.op !== "create") throw new Error("mountInto: the element created no node");
    if (before !== undefined) first.before = before;
    var id = first.id;
    var created = ops.filter((op) => op.op === "create").map((op) => (op as { id: number }).id);
    send(ops);
    return {
        id: id,
        remove() {
            dispose(scope);
            send([{ op: "remove", id: id }]);
            for (var i = 0; i < created.length; i++) freeIds.push(created[i]);
        },
    };
}
/** Replaces a node's paint; properties with a transition animate from what is on screen. */
function setPaint(id: number, paint: Paint): void {
    send([{ op: "setpaint", id: id, paint: paint }]);
}
function setSpans(id: number, spans: Span[]): void {
    send([{ op: "setspans", id: id, spans: spans }]);
}
/** Calls `run` once after `ms` milliseconds. Rust keeps the timer; JS does not wait. */
function after(ms: number, run: () => void): void {
    var callback = register(function () {
        unregister(callback);
        run();
    });
    send([{ op: "after", ms: ms, callback: callback }]);
}

// ---------- Components ----------

/** A container: flexbox layout (`style`), looks (`paint`) and an optional press callback. */
function Box(props: {
    style?: Source<Style>;
    paint?: Source<Paint>;
    onPress?: () => void;
    /** The pointer enters (true) or leaves (false) the box, unless a higher layer covers it. */
    onHover?: (inside: boolean) => void;
    /** A right-button press over the box, at window coordinates. */
    onContextMenu?: (p: { x: number; y: number }) => void;
    /** Escape, or a press outside the box and outside any higher layer: light dismiss of menus and popovers. */
    onDismiss?: () => void;
    disabled?: Source<boolean>;
    ref?: (id: number) => void;
    children?: Children;
}): Element {
    return {
        mount(parent, ops) {
            var id = newId();
            if (props.ref) props.ref(id);
            var style = bind(props.style ?? {}, function (s) {
                send([{ op: "setstyle", id: id, style: s }]);
            });
            var paint = bind(props.paint ?? {}, function (p) {
                setPaint(id, p);
            });
            var disabled = bind(props.disabled ?? false, function (d) {
                send([{ op: "setdisabled", id: id, disabled: d }]);
            });
            var press = props.onPress ? register(props.onPress) : 0;
            var onHover = props.onHover;
            var hover = onHover === undefined ? 0 : register(function (text) {
                (onHover as (inside: boolean) => void)(text === "1");
            });
            var onContextMenu = props.onContextMenu;
            var contextmenu = onContextMenu === undefined ? 0 : register(function (text) {
                (onContextMenu as (p: { x: number; y: number }) => void)(JSON.parse(text));
            });
            var onDismiss = props.onDismiss;
            var dismiss = onDismiss === undefined ? 0 : register(function () {
                (onDismiss as () => void)();
            });
            ops.push({
                op: "create",
                id: id,
                parent: parent,
                node: {
                    kind: "box",
                    style: style,
                    paint: paint,
                    press: press,
                    disabled: disabled,
                    hover: hover,
                    contextmenu: contextmenu,
                    dismiss: dismiss,
                },
            });
            mountChildren(props.children, id, ops);
        },
    };
}
/** A `Box` that lays out its children in a row. */
function Row(props: Parameters<typeof Box>[0]): Element {
    var style = props.style;
    return Box({
        ...props,
        style: function () {
            return { ...read(style ?? {}), direction: "row" as Axis };
        },
    });
}
function Column(props: { style?: Style; children?: Children }): Element {
    return {
        mount(parent, ops) {
            var id = newId();
            ops.push({ op: "create", id: id, parent: parent, node: { kind: "column", style: props.style } });
            mountChildren(props.children, id, ops);
        },
    };
}
/** Scrolling list. `children` builds each item; new items arrive with `items.push`. */
function List<T>(props: { items: ListSignal<T>; style?: Style; children: (item: T) => Element }): Element {
    return {
        mount(parent, ops) {
            var id = newId();
            ops.push({ op: "create", id: id, parent: parent, node: { kind: "list", style: props.style } });
            var items = props.items.items;
            for (var i = 0; i < items.length; i++) props.children(items[i]).mount(id, ops);
            (listeners.get(props.items) as Array<(item: T) => void>).push(function (item) {
                var created: Operation[] = [];
                props.children(item).mount(id, created);
                send(created);
            });
        },
    };
}
function Text(props: {
    style?: Style;
    font?: Font;
    paint?: Source<Paint>;
    /** At most this many lines; the last one shown ends in "…" when the text does not fit. */
    lines?: number;
    /** Alignment of each line inside the node's width. */
    align?: TextAlign;
    /** Rich text: these runs are drawn instead of `children`, which must be their plain text. */
    spans?: Source<Span[]>;
    children: Source<string>;
}): Element {
    return {
        mount(parent, ops) {
            var id = newId();
            var text = bind(props.children, function (v) {
                jsnir.setText(id, v);
            });
            var spans = props.spans === undefined ? undefined : bind(props.spans, function (v) {
                setSpans(id, v);
            });
            var paint = bind(props.paint ?? {}, function (p) {
                setPaint(id, p);
            });
            ops.push({
                op: "create",
                id: id,
                parent: parent,
                node: { kind: "text", text: text, style: props.style, font: props.font, paint: paint, lines: props.lines, align: props.align, spans: spans },
            });
        },
    };
}
/** A built-in stroke icon, `size` logical pixels square. */
function Icon(props: { name: IconName; size: number; stroke: number; style?: Style; paint?: Source<Paint> }): Element {
    return {
        mount(parent, ops) {
            var id = newId();
            var paint = bind(props.paint ?? {}, function (p) {
                setPaint(id, p);
            });
            ops.push({
                op: "create",
                id: id,
                parent: parent,
                node: { kind: "icon", icon: props.name, size: props.size, stroke: props.stroke, style: props.style, paint: paint },
            });
        },
    };
}
/** An image the embedding program supplied by `source` (`Host::set_image`), stretched to its box. */
function Image(props: { source: Source<string>; style?: Style; paint?: Source<Paint> }): Element {
    return {
        mount(parent, ops) {
            var id = newId();
            var source = bind(props.source, function (s) {
                send([{ op: "setsource", id: id, source: s }]);
            });
            var paint = bind(props.paint ?? {}, function (p) {
                setPaint(id, p);
            });
            ops.push({ op: "create", id: id, parent: parent, node: { kind: "image", source: source, style: props.style, paint: paint } });
        },
    };
}
/** Three-digit numeric label (0 to 999). */
function Label(props: { value: Source<number> }): Element {
    return {
        mount(parent, ops) {
            var id = newId();
            var value = bind(props.value, function (v) {
                jsnir.set(id, v);
            });
            ops.push({ op: "create", id: id, parent: parent, node: { kind: "label", value: value } });
        },
    };
}
function Button(props: { onClick: () => void }): Element {
    return {
        mount(parent, ops) {
            var callback = register(function () {
                props.onClick();
            });
            ops.push({ op: "create", id: newId(), parent: parent, node: { kind: "button", callback: callback } });
        },
    };
}
/** Live state of a field. The UI writes it; JS reads it from the replica without waiting. */
interface Field {
    text(): string;
    selection(): [number, number];
    /** Layout rectangle (x, y, width, height) in logical pixels, read from the columns without crossing. */
    rect(): [number, number, number, number];
}
/** A field submit. Rust already captured `text` and cleared the field in the same operation. */
interface TextSubmit {
    text: string;
    field: Field;
}
function TextInput(props: {
    style?: Style;
    font?: Font;
    paint?: Source<Paint>;
    /** Initial text. Afterwards the UI owns it; read it with `onChange`. */
    value?: string;
    placeholder?: string;
    placeholderColor?: number;
    onSubmit?: (submit: TextSubmit) => void;
    /** Receives the text after each edit, as a DOM `input` event. */
    onChange?: (text: string) => void;
}): Element {
    return {
        mount(parent, ops) {
            var id = newId();
            var field: Field = {
                text: function () {
                    return jsnir.readText(id) ?? "";
                },
                selection: function () {
                    var c = jsnir.columns;
                    return [c.selectionStart[id], c.selectionEnd[id]];
                },
                rect: function () {
                    var c = jsnir.columns;
                    return [c.x[id], c.y[id], c.width[id], c.height[id]];
                },
            };
            var onSubmit = props.onSubmit;
            var submit = onSubmit === undefined ? 0 : register(function (text) {
                (onSubmit as (submit: TextSubmit) => void)({ text: text, field: field });
            });
            var change = props.onChange ? register(props.onChange) : 0;
            var paint = bind(props.paint ?? {}, function (p) {
                setPaint(id, p);
            });
            ops.push({
                op: "create",
                id: id,
                parent: parent,
                node: {
                    kind: "input",
                    text: props.value,
                    selection: props.value === undefined ? undefined : [props.value.length, props.value.length],
                    submit: submit,
                    change: change,
                    placeholder: props.placeholder,
                    placeholderColor: props.placeholderColor,
                    style: props.style,
                    font: props.font,
                    paint: paint,
                },
            });
        },
    };
}

/** Mounts `children(value)` and mounts it again, in place of the old one, whenever `value` changes. */
function Dynamic<T>(props: { value: Source<T>; style?: Style; children: (value: T) => Element }): Element {
    return Box({
        style: props.style,
        ref: function (id) {
            var current: Mounted | null = null;
            var first = true;
            // Children belong to the scope that mounted this element, so removing it removes them too.
            var scope = currentScope;
            var swap = function (value: T) {
                inScope(scope, function () {
                    if (current !== null) current.remove();
                    current = mountInto(id, props.children(value));
                });
            };
            effect(function () {
                var value = read(props.value);
                // The first child waits until the batch that creates the container has been sent.
                if (first) {
                    first = false;
                    mountLater.push(function () {
                        swap(value);
                    });
                } else swap(value);
            });
        },
    });
}
// Subtrees to mount once the batch that creates their parent has been sent.
var mountLater: Array<() => void> = [];
function mountPending(): void {
    while (mountLater.length > 0) (mountLater.shift() as () => void)();
}

/** One child of a `For`: its item signal, its mounted subtree and the scope that holds its effects. */
interface KeyedRow<T> {
    signal: Signal<T>;
    mounted: Mounted;
    scope: Scope;
}
// Index in `order` (node ids in tree order) where a child goes: before `before`, or last.
function placeAt(order: number[], before: number | undefined): number {
    return before === undefined ? order.length : order.indexOf(before);
}
/** Mounts one child per item, keyed by `key`. Adding, removing or reordering items creates, removes or moves only those children. A child reads its item through `item()`, which changes when an item with the same key is replaced. */
function For<T>(props: {
    each: Source<T[]>;
    key: (item: T) => string | number;
    style?: Style;
    children: (item: () => T) => Element;
}): Element {
    return Box({
        style: props.style,
        ref: function (id) {
            // Children belong to the scope that mounted this list, so removing it removes them too.
            var scope = currentScope;
            var rows = new Map<string | number, KeyedRow<T>>();
            // Node ids of the children, in the order the tree has them.
            var order: number[] = [];
            var mountRow = function (item: T, before: number | undefined): KeyedRow<T> {
                var s = signal(item);
                var rowScope: Scope = { effects: [], callbacks: [], children: [] };
                currentScope.children.push(rowScope);
                var mounted = inScope(rowScope, function () {
                    return mountInto(id, props.children(function () {
                        return s.value;
                    }), before);
                });
                order.splice(placeAt(order, before), 0, mounted.id);
                return { signal: s, mounted: mounted, scope: rowScope };
            };
            var sync = function (items: T[]) {
                inScope(scope, function () {
                    var keys = items.map(props.key);
                    var fresh = new Set(keys);
                    if (fresh.size !== keys.length) throw new Error("For: duplicate key");
                    rows.forEach(function (row, key) {
                        if (fresh.has(key)) return;
                        dispose(row.scope);
                        scope.children.splice(scope.children.indexOf(row.scope), 1);
                        row.mounted.remove();
                        order.splice(order.indexOf(row.mounted.id), 1);
                        rows.delete(key);
                    });
                    // From the last item back, each child goes before the one placed after it: a kept child
                    // moves only when it is not already there.
                    var before: number | undefined = undefined;
                    for (var i = items.length - 1; i >= 0; i--) {
                        var row = rows.get(keys[i]);
                        if (row === undefined) {
                            row = mountRow(items[i], before);
                            rows.set(keys[i], row);
                        } else {
                            row.signal.value = items[i];
                            if (order[order.indexOf(row.mounted.id) + 1] !== before) {
                                send([{ op: "move", id: row.mounted.id, before: before }]);
                                order.splice(order.indexOf(row.mounted.id), 1);
                                order.splice(placeAt(order, before), 0, row.mounted.id);
                            }
                        }
                        before = row.mounted.id;
                    }
                });
            };
            var first = true;
            effect(function () {
                var items = read(props.each);
                // The first children wait until the batch that creates the list has been sent.
                if (first) {
                    first = false;
                    mountLater.push(function () {
                        sync(items);
                    });
                } else sync(items);
            });
        },
    });
}

// ---------- Overlays ----------

/** A panel on top of the page, placed beside the node `anchor()` returns while `open` is true, 8 px from it. Escape or
 *  a press outside closes it (sets `open` to false). It is mounted in the element it is written in, so an anchor the
 *  panel names must exist when it opens. */
function Popover(props: {
    open: Signal<boolean>;
    anchor: () => number | undefined;
    side?: Side;
    align?: AnchorAlign;
    layer?: number;
    style?: Style;
    paint?: Source<Paint>;
    children: Element;
}): Element {
    return {
        mount(parent) {
            var scope = currentScope;
            var current: Mounted | null = null;
            var show = function () {
                var node = props.anchor();
                if (node === undefined || current !== null) return;
                var anchor: Anchor = { node: node, side: props.side ?? "bottom", align: props.align ?? "start", gap: 8, flip: true };
                current = mountInto(
                    parent as number,
                    Box({
                        style: { ...props.style, position: "fixed", layer: props.layer ?? 1, anchor: anchor },
                        paint: props.paint,
                        onDismiss: function () {
                            props.open.value = false;
                        },
                        children: props.children,
                    }),
                );
            };
            var first = true;
            effect(function () {
                var on = props.open.value;
                // The first state waits until the batch that creates the parent has been sent.
                if (first) {
                    first = false;
                    if (on) mountLater.push(function () { inScope(scope, show); });
                } else if (on) inScope(scope, show);
                else if (current !== null) {
                    current.remove();
                    current = null;
                }
            });
        },
    };
}
/** Shows `text` beside its child while the pointer is over the child, after `delay` ms: a `Popover` on layer 1000. */
function Tooltip(props: {
    text: Source<string>;
    side?: Side;
    delay?: number;
    font?: Font;
    style?: Style;
    paint?: Source<Paint>;
    children: Element;
}): Element {
    var open = signal(false);
    var wrapper = 0;
    var hovering = false;
    var token = 0;
    return Box({
        ref: function (id) {
            wrapper = id;
        },
        onHover: function (inside) {
            hovering = inside;
            var mine = ++token;
            if (!inside) {
                open.value = false;
                return;
            }
            after(props.delay ?? 0, function () {
                if (hovering && mine === token) open.value = true;
            });
        },
        children: [
            props.children,
            Popover({
                open: open,
                anchor: function () {
                    return wrapper;
                },
                side: props.side ?? "top",
                align: "center",
                layer: 1000,
                style: props.style,
                paint: props.paint,
                children: Text({ font: props.font, children: props.text }),
            }),
        ],
    });
}

// ---------- Protocol with Rust ----------

var root: (() => Element) | null = null;
/** Registers the app's root component. Rust mounts it at startup. */
function mount(app: () => Element): void {
    root = app;
}
var hostHandler: ((message: any) => void) | undefined;
/** Receives the messages the embedding Rust program sends with `Host::push`, parsed from JSON. The window itself sends `{"jsnirScale": n}` when it opens and whenever its scale factor changes (device pixels per logical pixel). */
function onHost(handler: (message: any) => void): void {
    hostHandler = handler;
}
// Callback 0 is reserved for messages from Rust (`HOST_CALLBACK`); SDK callbacks start at 1.
function hostEvent(text: string): void {
    if (hostHandler !== undefined) hostHandler(JSON.parse(text));
}
// Rust calls `jsnirMount` once and `jsnirEvent` per event, on the VM thread. The operations they emit
// form that call's batch.
function jsnirMount(): void {
    if (root === null) throw new Error("mount(App) was never called");
    var ops: Operation[] = [];
    root().mount(undefined, ops);
    send(ops);
    mountPending();
}
function jsnirEvent(callback: number, text: string): void {
    var handler = callback === 0 ? hostEvent : handlers[callback - 1];
    // A press can arrive after its node was removed: there is nothing left to do.
    if (handler === undefined) return;
    var previous = batching;
    batching = true;
    try {
        handler(text);
    } finally {
        batching = previous;
    }
    flush();
    mountPending();
}
