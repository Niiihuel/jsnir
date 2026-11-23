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
    key?: string;
}
var previousState: SavedValue[] = typeof __jsnirPreviousState === "string" ? JSON.parse(__jsnirPreviousState) : [];
var previousByKey = new Map<string, SavedValue>();
for (var prevIdx = 0; prevIdx < previousState.length; prevIdx++) {
    var entry = previousState[prevIdx];
    if (entry.key !== undefined) previousByKey.set(entry.key, entry);
}
var keyCounts: { [key: string]: number } = {};
var restorables: Array<{ initial: string | undefined; read: () => unknown; key?: string }> = [];
function restore<T>(initial: T, read: () => unknown, key?: string): T {
    var initJson = JSON.stringify(initial);
    var slotKey: string | undefined = undefined;
    var previous: SavedValue | undefined;
    if (key !== undefined) {
        var count = keyCounts[key] ?? 0;
        keyCounts[key] = count + 1;
        slotKey = count === 0 ? key : key + "#" + count;
        previous = previousByKey.get(slotKey);
    } else {
        previous = previousState[restorables.length];
    }
    restorables.push({ initial: initJson, read: read, key: slotKey });
    return previous !== undefined && initJson !== undefined && previous.initial === initJson ? (previous.value as T) : initial;
}
function jsnirReloadState(): string {
    return JSON.stringify(
        restorables.map(function (r) {
            return r.key === undefined ? { initial: r.initial, value: r.read() } : { initial: r.initial, value: r.read(), key: r.key };
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
    // Virtual rows are transient: neither payloads nor local signals belong in reload state.
    ids?: number[];
    cleanup?: Array<() => void>;
    // Set by dispose: the scope's nodes left the screen with an ancestor's, or are about to with its own remove.
    disposed?: boolean;
}
function childScope(transient = currentScope.ids !== undefined): Scope {
    // Born under a disposed scope (a deferred mount that runs after its owner left), it is disposed too: nothing it
    // would mount has a parent on screen any more.
    return { effects: [], callbacks: [], children: [], ids: transient ? [] : undefined, disposed: currentScope.disposed };
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
// Reports an error the app survives. Nothing here may throw: formatting the value or handing it to Rust
// (a lone surrogate is not valid UTF-8) falls back to a fixed message.
function report(error: unknown): void {
    try {
        jsnir.report(error instanceof Error ? `${error.message}\n${error.stack ?? ""}` : String(error));
    } catch (_) {
        try { jsnir.report("a cleanup threw a value that cannot be printed"); } catch (_) { }
    }
}
function dispose(scope: Scope): void {
    scope.disposed = true;
    for (var i = 0; i < scope.effects.length; i++) scope.effects[i].disposed = true;
    for (var j = 0; j < scope.callbacks.length; j++) {
        var id = scope.callbacks[j];
        var guarded = rowCallbacks.get(id);
        if (guarded === undefined || guarded.scope === scope) unregister(id);
    }
    for (var k = 0; k < scope.children.length; k++) dispose(scope.children[k]);
    scope.cleanup?.forEach(run => {
        // A throw here would drop the whole event's batch, removes included: report it and go on.
        try { run(); } catch (error) { report(error); }
    });
    scope.ids?.forEach(id => retiring.push(id));
    scope.ids = undefined;
    scope.cleanup = undefined;
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

function signal<T>(initial: T, key?: string): Signal<T> {
    var current: T = currentScope.ids !== undefined ? initial : restore(initial, function () {
        return current;
    }, key);
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
    // A rerun can come from any flush, even one inside another subtree's mount: it runs in the scope that made it, so
    // what it creates belongs there and not to whatever scope happened to be current.
    var owner = currentScope;
    var e: Effect = function () {
        var previous = observer;
        observer = e;
        try {
            inScope(owner, body);
        } finally {
            observer = previous;
        }
    };
    currentScope.effects.push(e);
    if (owner.disposed) {
        e.disposed = true;
        return;
    }
    e();
}

/** Releases a resource when the component's mounted scope is removed. */
function onCleanup(release: () => void): void {
    (currentScope.cleanup ??= []).push(release);
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
function list<T>(initial: T[] = [], key?: string): ListSignal<T> {
    var items: T[] = restore(initial, function () {
        return items;
    }, key).slice();
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
// Ids of disposed nodes, reusable once the remove that takes them off the host has been sent.
var retiring: number[] = [];
var removing = 0;
function newId(): number {
    if (freeIds.length === 0 && nextId > 65535) throw new RangeError("JSNIR supports up to 65,535 live nodes");
    var id = freeIds.length > 0 ? freeIds.pop() as number : nextId++;
    currentScope.ids?.push(id);
    return id;
}
var handlers: Array<((text: string) => void) | undefined> = [];
var dragHandlers: Array<((e: DragEvent) => void) | undefined> = [];
var scrollHandlers: Array<((s: { top: number; height: number; client: number }) => void) | undefined> = [];
var contextMenuHandlers: Array<((p: { x: number; y: number }) => void) | undefined> = [];
var freeCallbacks: number[] = [];
var placementTokens: { [callback: number]: string | undefined } = {};
declare var __jsnirPlacementEpoch: string | undefined;
var nextPlacementToken = 1;
var nextCallbackToken = 1;
var rowCallbacks = new Map<number, { token: string; scope: Scope }>();
function register(handler: (text: string) => void): number {
    var id = freeCallbacks.length > 0 ? (freeCallbacks.pop() as number) : handlers.length + 1;
    handlers[id - 1] = handler;
    currentScope.callbacks.push(id);
    if (currentScope.ids !== undefined) {
        var token = (typeof __jsnirPlacementEpoch === "string" ? __jsnirPlacementEpoch : "0")
            + ":" + nextCallbackToken++;
        rowCallbacks.set(id, { token, scope: currentScope });
        emitCallbackGuard(id, token);
    }
    return id;
}
function registerDrag(handler: (e: DragEvent) => void): number {
    var id = register(function (text) {
        handler(JSON.parse(text));
    });
    dragHandlers[id - 1] = handler;
    return id;
}
function registerScroll(handler: (s: { top: number; height: number; client: number }) => void): number {
    var id = register(function (text) {
        handler(JSON.parse(text));
    });
    scrollHandlers[id - 1] = handler;
    return id;
}
function registerContextMenu(handler: (p: { x: number; y: number }) => void): number {
    var id = register(function (text) {
        handler(JSON.parse(text));
    });
    contextMenuHandlers[id - 1] = handler;
    return id;
}
function unregister(id: number): void {
    if (handlers[id - 1] === undefined) return;
    handlers[id - 1] = undefined;
    dragHandlers[id - 1] = undefined;
    scrollHandlers[id - 1] = undefined;
    contextMenuHandlers[id - 1] = undefined;
    var guarded = rowCallbacks.get(id);
    if (guarded !== undefined) {
        rowCallbacks.delete(id);
        var callbacks = guarded.scope.callbacks;
        callbacks[callbacks.indexOf(id)] = 0; // do not unregister a future owner of this id
        emitCallbackGuard(id, undefined);
    }
    if (placementTokens[id] !== undefined) delete placementTokens[id];
    freeCallbacks.push(id);
}

// ---------- Binary Command Buffer ----------
// Frequent ops and node creations are packed directly into the shared `jsnir.cmd` / `jsnir.cmdF32`
// ArrayBuffer (with strings in `cmdStrs`) and flushed in one `jsnir.commit` / `jsnir.commitStrings`
// call without `JSON.stringify` or `serde_json`.
var OP_SET = 1;
var OP_SET_DISABLED = 2;
var OP_REMOVE = 3;
var OP_MOVE = 4;
var OP_FOCUS = 5;
var OP_AFTER = 6;
var OP_SCROLL_TO = 7;
var OP_SET_STYLE = 8;
var OP_SET_PAINT = 9;
var OP_SET_TEXT = 10;
var OP_CREATE_BOX = 11;
var OP_CREATE_TEXT = 12;
var OP_CREATE_COLUMN = 13;
var OP_CREATE_LABEL = 14;
var OP_CREATE_BUTTON = 15;
var OP_CREATE_ICON = 16;
var OP_CREATE_IMAGE = 17;
var OP_SET_SOURCE = 18;
var OP_CALLBACK_GUARD = 19;
var CURSOR_CODES: { [k: string]: number | undefined } = {
    default: 0,
    pointer: 1,
    text: 2,
    notAllowed: 3,
    grab: 4,
    grabbing: 5,
    move: 6,
    colResize: 7,
    rowResize: 8,
    ewResize: 9,
    nsResize: 10,
    neswResize: 11,
    nwseResize: 12,
};
var ICON_CODES: { [k: string]: number | undefined } = {
    arrowLeft: 0,
    arrowRight: 1,
    tick: 2,
    checkCircle: 3,
};
var cmdPos = 0;
var cmdStrs: string[] = [];
var inMount = false;
function resetCmd(): void {
    cmdPos = 0;
    cmdStrs.length = 0;
}
function flushCmd(): void {
    if (cmdPos === 0) return;
    var words = cmdPos;
    cmdPos = 0;
    if (cmdStrs.length === 0) {
        jsnir.commit(words);
    } else {
        var strs = cmdStrs;
        cmdStrs = [];
        jsnir.commitStrings(words, strs);
    }
}
function reserveCmd(words: number): void {
    if (cmdPos + words > jsnir.cmd.length) flushCmd();
}
function maybeFlushCmd(): void {
    if (!batching && !inMount) flushCmd();
}
function pushStr(s: string): number {
    var idx = cmdStrs.length;
    cmdStrs.push(s);
    return idx;
}
function isU16(v: number): boolean {
    return Number.isInteger(v) && v >= 0 && v <= 65535;
}
function canEncodeStyle(s: Style | undefined): boolean {
    if (s === undefined) return true;
    if (
        s.anchor !== undefined
        || (Array.isArray(s.padding) && s.padding.length !== 4)
        || (Array.isArray(s.margin) && s.margin.length !== 4)
        || (s.follow !== undefined && !isU16(s.follow))
    ) return false;
    if (s.direction !== undefined && s.direction !== "column" && s.direction !== "row") return false;
    if (s.align !== undefined && s.align !== "stretch" && s.align !== "start" && s.align !== "center" && s.align !== "end") return false;
    if (s.justify !== undefined && s.justify !== "start" && s.justify !== "center" && s.justify !== "end" && s.justify !== "between") return false;
    if (s.position !== undefined && s.position !== "relative" && s.position !== "absolute" && s.position !== "fixed") return false;
    if (s.scrollAnchor !== undefined && s.scrollAnchor !== "start" && s.scrollAnchor !== "end") return false;
    return true;
}
function writeStyle(u: Uint32Array, f: Float32Array, start: number, s: Style): number {
    var flags = 0;
    if (s.direction === "row") flags |= 1;
    if (s.align === "stretch") flags |= 1 << 1;
    else if (s.align === "start") flags |= 2 << 1;
    else if (s.align === "center") flags |= 3 << 1;
    else if (s.align === "end") flags |= 4 << 1;
    if (s.justify === "center") flags |= 1 << 4;
    else if (s.justify === "end") flags |= 2 << 4;
    else if (s.justify === "between") flags |= 3 << 4;
    if (s.position === "absolute") flags |= 1 << 6;
    else if (s.position === "fixed") flags |= 2 << 6;
    if (s.clip) flags |= 1 << 8;
    if (s.scroll) flags |= 1 << 9;
    if (s.scrollAnchor === "end") flags |= 1 << 10;
    if (s.group) flags |= 1 << 11;

    var p = start + 2;
    var mask = 0;
    if (s.width !== undefined) { mask |= 1 << 0; f[p++] = s.width; }
    if (s.height !== undefined) { mask |= 1 << 1; f[p++] = s.height; }
    if (s.grow !== undefined) { mask |= 1 << 2; f[p++] = s.grow; }
    if (s.shrink !== undefined) { mask |= 1 << 3; f[p++] = s.shrink; }
    if (s.basis !== undefined) { mask |= 1 << 4; f[p++] = s.basis; }
    if (s.padding !== undefined) {
        mask |= 1 << 5;
        if (typeof s.padding === "number") {
            f[p] = s.padding; f[p + 1] = s.padding; f[p + 2] = s.padding; f[p + 3] = s.padding;
        } else {
            f[p] = s.padding[0]; f[p + 1] = s.padding[1]; f[p + 2] = s.padding[2]; f[p + 3] = s.padding[3];
        }
        p += 4;
    }
    if (s.margin !== undefined) {
        mask |= 1 << 6;
        if (typeof s.margin === "number") {
            f[p] = s.margin; f[p + 1] = s.margin; f[p + 2] = s.margin; f[p + 3] = s.margin;
        } else {
            f[p] = s.margin[0]; f[p + 1] = s.margin[1]; f[p + 2] = s.margin[2]; f[p + 3] = s.margin[3];
        }
        p += 4;
    }
    if (s.gap !== undefined) { mask |= 1 << 7; f[p++] = s.gap; }
    if (s.left !== undefined) { mask |= 1 << 8; f[p++] = s.left; }
    if (s.top !== undefined) { mask |= 1 << 9; f[p++] = s.top; }
    if (s.follow !== undefined) { mask |= 1 << 10; u[p++] = s.follow; }
    if (s.layer !== undefined) { mask |= 1 << 11; u[p++] = (s.layer | 0) >>> 0; }
    if (s.minWidth !== undefined) { mask |= 1 << 12; f[p++] = s.minWidth; }
    if (s.minHeight !== undefined) { mask |= 1 << 13; f[p++] = s.minHeight; }
    if (s.maxWidth !== undefined) { mask |= 1 << 14; f[p++] = s.maxWidth; }
    if (s.maxHeight !== undefined) { mask |= 1 << 15; f[p++] = s.maxHeight; }
    u[start] = mask;
    u[start + 1] = flags;
    return p;
}
function canEncodeVariant(v: Variant): boolean {
    if (v.gradient !== undefined && (!Array.isArray(v.gradient) || v.gradient.length !== 2)) return false;
    if (v.cursor !== undefined && CURSOR_CODES[v.cursor] === undefined) return false;
    return true;
}
function writeVariant(u: Uint32Array, f: Float32Array, start: number, v: Variant): number {
    var p = start + 1;
    var mask = 0;
    if (v.background !== undefined) { mask |= 1 << 0; u[p++] = v.background >>> 0; }
    if (v.gradient !== undefined) {
        mask |= 1 << 1;
        u[p++] = v.gradient[0] >>> 0;
        u[p++] = v.gradient[1] >>> 0;
    }
    if (v.brightness !== undefined) { mask |= 1 << 2; f[p++] = v.brightness; }
    if (v.scale !== undefined) { mask |= 1 << 3; f[p++] = v.scale; }
    if (v.opacity !== undefined) { mask |= 1 << 4; f[p++] = v.opacity; }
    if (v.x !== undefined) { mask |= 1 << 5; f[p++] = v.x; }
    if (v.y !== undefined) { mask |= 1 << 6; f[p++] = v.y; }
    if (v.color !== undefined) { mask |= 1 << 7; u[p++] = v.color >>> 0; }
    if (v.edge !== undefined) { mask |= 1 << 8; u[p++] = v.edge >>> 0; }
    if (v.height !== undefined) { mask |= 1 << 9; f[p++] = v.height; }
    if (v.width !== undefined) { mask |= 1 << 10; f[p++] = v.width; }
    if (v.underline !== undefined) { mask |= 1 << 11; u[p++] = v.underline ? 1 : 0; }
    if (v.cursor !== undefined) { mask |= 1 << 12; u[p++] = CURSOR_CODES[v.cursor] as number; }
    u[start] = mask;
    return p;
}
function canEncodePaint(paint: Paint | undefined): boolean {
    if (paint === undefined) return true;
    if (
        paint.mask !== undefined
        || (paint.shadows !== undefined && paint.shadows.length > 0)
        || (paint.transitions !== undefined && paint.transitions.length > 0)
        || paint.animations !== undefined
        || paint.starting !== undefined
        || paint.scrollbar !== undefined
        || (paint.gradient !== undefined && (!Array.isArray(paint.gradient) || paint.gradient.length !== 2))
        || (Array.isArray(paint.radius) && paint.radius.length !== 4)
        || (paint.edgeSides !== undefined && (!Array.isArray(paint.edgeSides) || paint.edgeSides.length !== 4))
        || (paint.cursor !== undefined && CURSOR_CODES[paint.cursor] === undefined)
        || (paint.hover !== undefined && !canEncodeVariant(paint.hover))
        || (paint.active !== undefined && !canEncodeVariant(paint.active))
        || (paint.focus !== undefined && !canEncodeVariant(paint.focus))
    ) return false;
    return true;
}
function writePaint(u: Uint32Array, f: Float32Array, start: number, paint: Paint): number {
    var p = start + 1;
    var mask = 0;
    if (paint.background !== undefined) { mask |= 1 << 0; u[p++] = paint.background >>> 0; }
    if (paint.gradient !== undefined) {
        mask |= 1 << 1;
        u[p++] = paint.gradient[0] >>> 0;
        u[p++] = paint.gradient[1] >>> 0;
    }
    if (paint.radius !== undefined) {
        mask |= 1 << 2;
        if (typeof paint.radius === "number") {
            f[p] = paint.radius; f[p + 1] = paint.radius; f[p + 2] = paint.radius; f[p + 3] = paint.radius;
        } else {
            f[p] = paint.radius[0]; f[p + 1] = paint.radius[1]; f[p + 2] = paint.radius[2]; f[p + 3] = paint.radius[3];
        }
        p += 4;
    }
    if (paint.edge !== undefined) { mask |= 1 << 3; u[p++] = paint.edge >>> 0; }
    if (paint.edgeWidth !== undefined) { mask |= 1 << 4; f[p++] = paint.edgeWidth; }
    if (paint.edgeSides !== undefined) {
        mask |= 1 << 5;
        f[p] = paint.edgeSides[0]; f[p + 1] = paint.edgeSides[1]; f[p + 2] = paint.edgeSides[2]; f[p + 3] = paint.edgeSides[3];
        p += 4;
    }
    if (paint.highlight !== undefined) { mask |= 1 << 6; u[p++] = paint.highlight >>> 0; }
    if (paint.opacity !== undefined) { mask |= 1 << 7; f[p++] = paint.opacity; }
    if (paint.x !== undefined) { mask |= 1 << 8; f[p++] = paint.x; }
    if (paint.y !== undefined) { mask |= 1 << 9; f[p++] = paint.y; }
    if (paint.scale !== undefined) { mask |= 1 << 10; f[p++] = paint.scale; }
    if (paint.brightness !== undefined) { mask |= 1 << 11; f[p++] = paint.brightness; }
    if (paint.color !== undefined) { mask |= 1 << 12; u[p++] = paint.color >>> 0; }
    if (paint.draw !== undefined) { mask |= 1 << 13; f[p++] = paint.draw; }
    if (paint.cursor !== undefined) { mask |= 1 << 14; u[p++] = CURSOR_CODES[paint.cursor] as number; }
    if (paint.hover !== undefined) { mask |= 1 << 15; p = writeVariant(u, f, p, paint.hover); }
    if (paint.active !== undefined) { mask |= 1 << 16; p = writeVariant(u, f, p, paint.active); }
    if (paint.focus !== undefined) { mask |= 1 << 17; p = writeVariant(u, f, p, paint.focus); }
    u[start] = mask;
    return p;
}
function canEncodeFont(font: Font | undefined): boolean {
    if (font === undefined) return true;
    return font.weight === undefined || isU16(font.weight);
}
function writeFont(u: Uint32Array, f: Float32Array, start: number, font: Font): number {
    var p = start + 1;
    var mask = 0;
    if (font.size !== undefined) { mask |= 1 << 0; f[p++] = font.size; }
    if (font.weight !== undefined) { mask |= 1 << 1; u[p++] = font.weight; }
    if (font.lineHeight !== undefined) { mask |= 1 << 2; f[p++] = font.lineHeight; }
    if (font.letterSpacing !== undefined) { mask |= 1 << 3; f[p++] = font.letterSpacing; }
    if (font.family !== undefined) { mask |= 1 << 4; u[p++] = pushStr(font.family); }
    u[start] = mask;
    return p;
}
function emitSet(id: number, value: number): void {
    if (!isU16(value)) {
        flushCmd();
        jsnir.set(id, value);
        return;
    }
    reserveCmd(3);
    var u = jsnir.cmd;
    var p = cmdPos;
    u[p] = OP_SET;
    u[p + 1] = id;
    u[p + 2] = value;
    cmdPos = p + 3;
    maybeFlushCmd();
}
function emitSetDisabled(id: number, disabled: boolean): void {
    reserveCmd(3);
    var u = jsnir.cmd;
    var p = cmdPos;
    u[p] = OP_SET_DISABLED;
    u[p + 1] = id;
    u[p + 2] = disabled ? 1 : 0;
    cmdPos = p + 3;
    maybeFlushCmd();
}
function emitRemove(id: number): void {
    reserveCmd(2);
    var u = jsnir.cmd;
    var p = cmdPos;
    u[p] = OP_REMOVE;
    u[p + 1] = id;
    cmdPos = p + 2;
    maybeFlushCmd();
}
function emitMove(id: number, before: number | undefined): void {
    reserveCmd(3);
    var u = jsnir.cmd;
    var p = cmdPos;
    u[p] = OP_MOVE;
    u[p + 1] = id;
    u[p + 2] = before === undefined ? 0xffffffff : before;
    cmdPos = p + 3;
    maybeFlushCmd();
}
function emitFocus(id: number): void {
    reserveCmd(2);
    var u = jsnir.cmd;
    var p = cmdPos;
    u[p] = OP_FOCUS;
    u[p + 1] = id;
    cmdPos = p + 2;
    maybeFlushCmd();
}
function emitAfter(callback: number, ms: number): void {
    reserveCmd(3);
    var u = jsnir.cmd;
    var p = cmdPos;
    u[p] = OP_AFTER;
    u[p + 1] = callback;
    jsnir.cmdF32[p + 2] = ms;
    cmdPos = p + 3;
    maybeFlushCmd();
}
function emitScrollTo(id: number, to: { offset?: number; to?: ScrollEdge; smooth?: boolean }): void {
    reserveCmd(4);
    var u = jsnir.cmd;
    var p = cmdPos;
    var flags = (to.offset !== undefined ? 1 : 0)
        | (to.to !== undefined ? 2 : 0)
        | (to.to === "end" ? 4 : 0)
        | (to.smooth ? 8 : 0);
    u[p] = OP_SCROLL_TO;
    u[p + 1] = id;
    u[p + 2] = flags;
    jsnir.cmdF32[p + 3] = to.offset ?? 0;
    cmdPos = p + 4;
    maybeFlushCmd();
}
function emitSetText(id: number, text: string): void {
    reserveCmd(3);
    var u = jsnir.cmd;
    var p = cmdPos;
    u[p] = OP_SET_TEXT;
    u[p + 1] = id;
    u[p + 2] = pushStr(text);
    cmdPos = p + 3;
    maybeFlushCmd();
}
function emitSetSource(id: number, source: string): void {
    reserveCmd(3);
    var u = jsnir.cmd;
    var p = cmdPos;
    u[p] = OP_SET_SOURCE;
    u[p + 1] = id;
    u[p + 2] = pushStr(source);
    cmdPos = p + 3;
    maybeFlushCmd();
}
function emitCallbackGuard(callback: number, token: string | undefined): void {
    reserveCmd(3);
    var u = jsnir.cmd;
    var p = cmdPos;
    u[p] = OP_CALLBACK_GUARD;
    u[p + 1] = callback;
    u[p + 2] = token === undefined ? 0xffffffff : pushStr(token);
    cmdPos = p + 3;
    maybeFlushCmd();
}
function emitSetStyle(id: number, s: Style): void {
    if (!canEncodeStyle(s)) {
        send([{ op: "setstyle", id: id, style: s }]);
        return;
    }
    reserveCmd(26);
    var u = jsnir.cmd;
    var p = cmdPos;
    u[p] = OP_SET_STYLE;
    u[p + 1] = id;
    cmdPos = writeStyle(u, jsnir.cmdF32, p + 2, s);
    maybeFlushCmd();
}
function emitSetPaint(id: number, paint: Paint): void {
    if (!canEncodePaint(paint)) {
        send([{ op: "setpaint", id: id, paint: paint }]);
        return;
    }
    reserveCmd(70);
    var u = jsnir.cmd;
    var p = cmdPos;
    u[p] = OP_SET_PAINT;
    u[p + 1] = id;
    cmdPos = writePaint(u, jsnir.cmdF32, p + 2, paint);
    maybeFlushCmd();
}
function canEncodeOp(op: Operation): boolean {
    switch (op.op) {
        case "create": {
            var n = op.node;
            switch (n.kind) {
                case "box":
                    return canEncodeStyle(n.style) && canEncodePaint(n.paint);
                case "text":
                    return n.spans === undefined
                        && (n.lines === undefined || isU16(n.lines))
                        && (n.align === undefined || n.align === "start" || n.align === "center" || n.align === "end")
                        && canEncodeFont(n.font)
                        && canEncodeStyle(n.style)
                        && canEncodePaint(n.paint);
                case "column":
                    return canEncodeStyle(n.style);
                case "label":
                    return isU16(n.value);
                case "button":
                    return isU16(n.callback);
                case "icon":
                    return ICON_CODES[n.icon] !== undefined
                        && canEncodeStyle(n.style)
                        && canEncodePaint(n.paint);
                case "image":
                    return canEncodeStyle(n.style) && canEncodePaint(n.paint);
                default:
                    return false;
            }
        }
        case "setstyle":
            return canEncodeStyle(op.style);
        case "setpaint":
            return canEncodePaint(op.paint);
        case "set":
            return isU16(op.value);
        case "setdisabled":
        case "remove":
        case "move":
        case "focus":
        case "after":
        case "scrollto":
        case "settext":
        case "setsource":
        case "callbackguard":
            return true;
        default:
            return false;
    }
}
function encodeOp(op: Operation): void {
    switch (op.op) {
        case "create": {
            var n = op.node;
            var parent = op.parent === undefined ? 0xffffffff : op.parent;
            var before = op.before === undefined ? 0xffffffff : op.before;
            switch (n.kind) {
                case "box": {
                    reserveCmd(103);
                    var u = jsnir.cmd;
                    var f = jsnir.cmdF32;
                    var start = cmdPos;
                    var p = start + 5;
                    var flags = n.disabled ? 1 : 0;
                    if (n.press) { flags |= 1 << 1; u[p++] = n.press; }
                    if (n.hover) { flags |= 1 << 2; u[p++] = n.hover; }
                    if (n.contextmenu) { flags |= 1 << 3; u[p++] = n.contextmenu; }
                    if (n.dismiss) { flags |= 1 << 4; u[p++] = n.dismiss; }
                    if (n.scroll) { flags |= 1 << 5; u[p++] = n.scroll; }
                    if (n.drag) { flags |= 1 << 6; u[p++] = n.drag; }
                    if (n.style !== undefined) { flags |= 1 << 7; p = writeStyle(u, f, p, n.style); }
                    if (n.paint !== undefined) { flags |= 1 << 8; p = writePaint(u, f, p, n.paint); }
                    u[start] = OP_CREATE_BOX;
                    u[start + 1] = op.id;
                    u[start + 2] = parent;
                    u[start + 3] = before;
                    u[start + 4] = flags;
                    cmdPos = p;
                    return;
                }
                case "text": {
                    reserveCmd(105);
                    var u = jsnir.cmd;
                    var f = jsnir.cmdF32;
                    var start = cmdPos;
                    var textIdx = pushStr(n.text);
                    var p = start + 6;
                    var flags = n.align === "center" ? 1 : n.align === "end" ? 2 : 0;
                    if (n.lines !== undefined) { flags |= 1 << 2; u[p++] = n.lines; }
                    if (n.font !== undefined) { flags |= 1 << 3; p = writeFont(u, f, p, n.font); }
                    if (n.style !== undefined) { flags |= 1 << 4; p = writeStyle(u, f, p, n.style); }
                    if (n.paint !== undefined) { flags |= 1 << 5; p = writePaint(u, f, p, n.paint); }
                    u[start] = OP_CREATE_TEXT;
                    u[start + 1] = op.id;
                    u[start + 2] = parent;
                    u[start + 3] = before;
                    u[start + 4] = textIdx;
                    u[start + 5] = flags;
                    cmdPos = p;
                    return;
                }
                case "column": {
                    reserveCmd(29);
                    var u = jsnir.cmd;
                    var start = cmdPos;
                    var p = start + 5;
                    var flags = 0;
                    if (n.style !== undefined) { flags = 1; p = writeStyle(u, jsnir.cmdF32, p, n.style); }
                    u[start] = OP_CREATE_COLUMN;
                    u[start + 1] = op.id;
                    u[start + 2] = parent;
                    u[start + 3] = before;
                    u[start + 4] = flags;
                    cmdPos = p;
                    return;
                }
                case "label": {
                    reserveCmd(5);
                    var u = jsnir.cmd;
                    var p = cmdPos;
                    u[p] = OP_CREATE_LABEL;
                    u[p + 1] = op.id;
                    u[p + 2] = parent;
                    u[p + 3] = before;
                    u[p + 4] = n.value;
                    cmdPos = p + 5;
                    return;
                }
                case "button": {
                    reserveCmd(5);
                    var u = jsnir.cmd;
                    var p = cmdPos;
                    u[p] = OP_CREATE_BUTTON;
                    u[p + 1] = op.id;
                    u[p + 2] = parent;
                    u[p + 3] = before;
                    u[p + 4] = n.callback;
                    cmdPos = p + 5;
                    return;
                }
                case "icon": {
                    reserveCmd(100);
                    var u = jsnir.cmd;
                    var f = jsnir.cmdF32;
                    var start = cmdPos;
                    var p = start + 8;
                    var flags = 0;
                    if (n.style !== undefined) { flags |= 1; p = writeStyle(u, f, p, n.style); }
                    if (n.paint !== undefined) { flags |= 2; p = writePaint(u, f, p, n.paint); }
                    u[start] = OP_CREATE_ICON;
                    u[start + 1] = op.id;
                    u[start + 2] = parent;
                    u[start + 3] = before;
                    u[start + 4] = ICON_CODES[n.icon] as number;
                    f[start + 5] = n.size;
                    f[start + 6] = n.stroke;
                    u[start + 7] = flags;
                    cmdPos = p;
                    return;
                }
                case "image": {
                    reserveCmd(98);
                    var u = jsnir.cmd;
                    var f = jsnir.cmdF32;
                    var start = cmdPos;
                    var srcIdx = pushStr(n.source);
                    var p = start + 6;
                    var flags = 0;
                    if (n.style !== undefined) { flags |= 1; p = writeStyle(u, f, p, n.style); }
                    if (n.paint !== undefined) { flags |= 2; p = writePaint(u, f, p, n.paint); }
                    u[start] = OP_CREATE_IMAGE;
                    u[start + 1] = op.id;
                    u[start + 2] = parent;
                    u[start + 3] = before;
                    u[start + 4] = srcIdx;
                    u[start + 5] = flags;
                    cmdPos = p;
                    return;
                }
            }
            return;
        }
        case "setstyle": {
            reserveCmd(26);
            var u = jsnir.cmd;
            var p = cmdPos;
            u[p] = OP_SET_STYLE;
            u[p + 1] = op.id;
            cmdPos = writeStyle(u, jsnir.cmdF32, p + 2, op.style);
            return;
        }
        case "setpaint": {
            reserveCmd(70);
            var u = jsnir.cmd;
            var p = cmdPos;
            u[p] = OP_SET_PAINT;
            u[p + 1] = op.id;
            cmdPos = writePaint(u, jsnir.cmdF32, p + 2, op.paint);
            return;
        }
        case "setdisabled": {
            reserveCmd(3);
            var u = jsnir.cmd;
            var p = cmdPos;
            u[p] = OP_SET_DISABLED;
            u[p + 1] = op.id;
            u[p + 2] = op.disabled ? 1 : 0;
            cmdPos = p + 3;
            return;
        }
        case "remove": {
            reserveCmd(2);
            var u = jsnir.cmd;
            var p = cmdPos;
            u[p] = OP_REMOVE;
            u[p + 1] = op.id;
            cmdPos = p + 2;
            return;
        }
        case "move": {
            reserveCmd(3);
            var u = jsnir.cmd;
            var p = cmdPos;
            u[p] = OP_MOVE;
            u[p + 1] = op.id;
            u[p + 2] = op.before === undefined ? 0xffffffff : op.before;
            cmdPos = p + 3;
            return;
        }
        case "focus": {
            emitFocus(op.id);
            return;
        }
        case "after": {
            reserveCmd(3);
            var u = jsnir.cmd;
            var p = cmdPos;
            u[p] = OP_AFTER;
            u[p + 1] = op.callback;
            jsnir.cmdF32[p + 2] = op.ms;
            cmdPos = p + 3;
            return;
        }
        case "scrollto": {
            reserveCmd(4);
            var u = jsnir.cmd;
            var p = cmdPos;
            var flags = (op.offset !== undefined ? 1 : 0)
                | (op.to !== undefined ? 2 : 0)
                | (op.to === "end" ? 4 : 0)
                | (op.smooth ? 8 : 0);
            u[p] = OP_SCROLL_TO;
            u[p + 1] = op.id;
            u[p + 2] = flags;
            jsnir.cmdF32[p + 3] = op.offset ?? 0;
            cmdPos = p + 4;
            return;
        }
        case "set": {
            reserveCmd(3);
            var u = jsnir.cmd;
            var p = cmdPos;
            u[p] = OP_SET;
            u[p + 1] = op.id;
            u[p + 2] = op.value;
            cmdPos = p + 3;
            return;
        }
        case "settext": {
            reserveCmd(3);
            var u = jsnir.cmd;
            var p = cmdPos;
            u[p] = OP_SET_TEXT;
            u[p + 1] = op.id;
            u[p + 2] = pushStr(op.text);
            cmdPos = p + 3;
            return;
        }
        case "setsource": {
            reserveCmd(3);
            var u = jsnir.cmd;
            var p = cmdPos;
            u[p] = OP_SET_SOURCE;
            u[p + 1] = op.id;
            u[p + 2] = pushStr(op.source);
            cmdPos = p + 3;
            return;
        }
        case "callbackguard": {
            reserveCmd(3);
            var u = jsnir.cmd;
            var p = cmdPos;
            u[p] = OP_CALLBACK_GUARD;
            u[p + 1] = op.callback;
            u[p + 2] = op.token === undefined ? 0xffffffff : pushStr(op.token);
            cmdPos = p + 3;
            return;
        }
    }
}
function send(ops: Operation[]): void {
    var jsonBatch: Operation[] | null = null;
    for (var i = 0; i < ops.length; i++) {
        var op = ops[i];
        if (canEncodeOp(op)) {
            if (jsonBatch !== null) {
                jsnir.batch(JSON.stringify({ ops: jsonBatch }));
                jsonBatch = null;
            }
            encodeOp(op);
        } else {
            if (jsonBatch === null) {
                flushCmd();
                jsonBatch = [op];
            } else {
                jsonBatch.push(op);
            }
        }
    }
    if (jsonBatch !== null) {
        jsnir.batch(JSON.stringify({ ops: jsonBatch }));
    } else {
        maybeFlushCmd();
    }
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
    var owner = currentScope;
    // A deferred first mount (Dynamic, For, Popover) can run after its owner was disposed in the same event: its
    // parent left the screen, so mounting would make zombie content whose later batches the UI rejects.
    if (owner.disposed) return { id: -1, remove() {} };
    var scope = childScope();
    var transient = scope.ids !== undefined;
    owner.children.push(scope);
    var ops: Operation[] = [];
    inScope(scope, function () {
        element.mount(parent, ops);
    });
    var first = ops[0];
    if (first === undefined || first.op !== "create") throw new Error("mountInto: the element created no node");
    if (before !== undefined) first.before = before;
    var id = first.id;
    if (!transient) {
        var created = ops.filter((op) => op.op === "create").map((op) => (op as { id: number }).id);
        // Freed on dispose, not only on remove: a parent's dispose must return the ids of mounts nested in it.
        (scope.cleanup ??= []).push(() => { for (var i = 0; i < created.length; i++) retiring.push(created[i]); });
    }
    send(ops);
    return {
        id: id,
        remove() {
            // Disposed with an ancestor: its nodes went with that ancestor's remove, and its ids may already belong
            // to new nodes, so a late remove (a stale reference) must send nothing.
            if (scope.disposed) {
                if (transient) owner.children.splice(owner.children.indexOf(scope), 1);
                return;
            }
            removing++;
            try {
                dispose(scope);
            } finally {
                // Even when a cleanup throws, the subtree leaves the screen.
                if (transient) owner.children.splice(owner.children.indexOf(scope), 1);
                emitRemove(id);
                removing--;
                // A remove nested in another's dispose must not free the outer's ids: its remove is not sent yet.
                if (removing === 0) while (retiring.length > 0) freeIds.push(retiring.pop() as number);
            }
        },
    };
}
/** Replaces a node's paint; properties with a transition animate from what is on screen. */
function setPaint(id: number, paint: Paint): void {
    emitSetPaint(id, paint);
}
function setSpans(id: number, spans: Span[]): void {
    send([{ op: "setspans", id: id, spans: spans }]);
}
/** Scrolls a `scroll` node (its id from a `ref`) to `offset` pixels, or to its `start` or `end`. `smooth` glides there. */
function scrollTo(id: number, to: { offset?: number; to?: ScrollEdge; smooth?: boolean }): void {
    emitScrollTo(id, to);
}
/** Calls `run` once after `ms` milliseconds. Rust keeps the timer; JS does not wait. */
function after(ms: number, run: () => void): void {
    var callback = register(function () {
        unregister(callback);
        run();
    });
    emitAfter(callback, ms);
}

// ---------- Components ----------

type DragPhase = "start" | "move" | "end";
/** Primary-button drag event in window logical pixels. `dx`/`dy` are the displacement from `startX`/`startY`. */
interface DragEvent {
    phase: DragPhase;
    x: number;
    y: number;
    startX: number;
    startY: number;
    dx: number;
    dy: number;
}

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
    /** A `scroll` box's offset changed: once per frame, with its offset and its content and box heights in pixels. */
    onScroll?: (s: { top: number; height: number; client: number }) => void;
    /** Primary-button drag starting on the box: `"start"` on press, `"move"` as the pointer moves, `"end"` on release or cancel. */
    onDrag?: (e: DragEvent) => void;
    disabled?: Source<boolean>;
    ref?: (id: number) => void;
    children?: Children;
}): Element {
    return {
        mount(parent, ops) {
            var id = newId();
            if (props.ref) props.ref(id);
            var style = bind(props.style ?? {}, function (s) {
                emitSetStyle(id, s);
            });
            var paint = bind(props.paint ?? {}, function (p) {
                setPaint(id, p);
            });
            var disabled = bind(props.disabled ?? false, function (d) {
                emitSetDisabled(id, d);
            });
            var press = props.onPress ? register(props.onPress) : 0;
            var onHover = props.onHover;
            var hover = onHover === undefined ? 0 : register(function (text) {
                (onHover as (inside: boolean) => void)(text === "1");
            });
            var onContextMenu = props.onContextMenu;
            var contextmenu = onContextMenu === undefined ? 0 : registerContextMenu(onContextMenu);
            var onDismiss = props.onDismiss;
            var dismiss = onDismiss === undefined ? 0 : register(function () {
                (onDismiss as () => void)();
            });
            var onScroll = props.onScroll;
            var scroll = onScroll === undefined ? 0 : registerScroll(onScroll);
            var onDrag = props.onDrag;
            var drag = onDrag === undefined ? 0 : registerDrag(onDrag);
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
                    scroll: scroll,
                    drag: drag,
                },
            });
            mountChildren(props.children, id, ops);
        },
    };
}
type BoxProps = Parameters<typeof Box>[0];
/** A `Box` that lays out its children in a row. */
function Row(props: BoxProps): Element {
    var style = props.style;
    return Box({
        ...props,
        style: function () {
            return { ...read(style ?? {}), direction: "row" as Axis };
        },
    });
}
/** A `Box` that lays out its children in a column. */
function Column(props: BoxProps): Element {
    var style = props.style;
    return Box({
        ...props,
        style: function () {
            return { ...read(style ?? {}), direction: "column" as Axis };
        },
    });
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
type VirtualPayload = {
    node: number;
    mount: string;
    entering: [string, unknown][];
    changed: [string, unknown][];
    evicted: string[];
};
var virtualMounts = new Map<number, (delta: VirtualPayload) => [string, number][]>();

/** Rust-indexed scrolling list. The embedder connects `name`; a row reads its latest payload via `item()`. */
function VirtualList<T>(props: { name: string; style?: Style; ref?: (id: number) => void;
    onScroll?: (s: { top: number; height: number; client: number }) => void; children: (key: string, item: () => T) => Element }): Element {
    return {
        mount(parent, ops) {
            var id = newId();
            props.ref?.(id);
            var scroll = props.onScroll === undefined ? 0 : registerScroll(props.onScroll);
            var mount: string | undefined;
            var rows = new Map<string, KeyedRow<T>>();
            ops.push({ op: "create", id, parent, node: { kind: "list", style: props.style, scroll } });
            ops.push({ op: "virtuallist", id, name: props.name });
            (currentScope.cleanup ??= []).push(() => {
                virtualMounts.delete(id);
                rows.forEach(row => dispose(row.scope));
                rows.clear();
            });
            virtualMounts.set(id, delta => {
                if (mount !== undefined && mount !== delta.mount) throw new Error("stale virtual List mount");
                mount = delta.mount;
                var bindings: [string, number][] = [];
                delta.evicted.forEach(key => {
                    var row = rows.get(key);
                    if (row === undefined) throw new Error("cannot evict a nonresident virtual row");
                    row.mounted.remove();
                    dispose(row.scope);
                    rows.delete(key);
                });
                delta.entering.forEach(([key, value]) => {
                    if (rows.has(key)) throw new Error("duplicate virtual row");
                    var scope = childScope(true);
                    var row = inScope(scope, () => {
                        var payload = signal(value as T);
                        var mounted = mountInto(id, props.children(key, () => payload.value));
                        return { signal: payload, mounted, scope };
                    });
                    rows.set(key, row);
                    bindings.push([key, row.mounted.id]);
                });
                delta.changed.forEach(([key, value]) => {
                    var row = rows.get(key);
                    if (row === undefined) throw new Error("cannot update a nonresident virtual row");
                    row.signal.value = value as T;
                });
                return bindings;
            });
        },
    };
}

function jsnirVirtualList(json: string): string {
    var delta = JSON.parse(json) as VirtualPayload;
    const deliver = virtualMounts.get(delta.node);
    if (deliver === undefined) throw new Error("unknown virtual List mount");
    return JSON.stringify(runEvent(() => deliver(delta)));
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
                emitSetText(id, v);
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
                emitSetSource(id, s);
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
                emitSet(id, v);
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
    /** Replaces text only if it still has the expected value. The caret moves to the end. */
    replace(text: string, expected?: string): void;
}
/** A field submit. The field may retain the text until the app accepts it. */
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
    /** Several lines: the text wraps, the field grows to its max height and then scrolls. */
    multiline?: boolean;
    /** In a multi-line field, Enter submits and Shift+Enter breaks the line, instead of Enter breaking it. */
    submitOnEnter?: boolean;
    retainOnSubmit?: boolean;
    /** Semantic purpose of the field (e.g. `"password"` masks glyphs with bullets and disables IME). */
    purpose?: InputPurpose;
    onSubmit?: (submit: TextSubmit) => void;
    /** Receives the text after each edit, as a DOM `input` event. */
    onChange?: (text: string) => void;
    /** Takes the focus when it mounts, as Discord's search fields do. */
    autoFocus?: boolean;
    ref?: (field: Field) => void;
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
                replace: function (text: string, expected?: string): void {
                    send([{ op: "setfield", id, text, expected: expected ?? field.text() }]);
                },
            };
            props.ref?.(field);
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
                    multiline: props.multiline,
                    submitOnEnter: props.submitOnEnter,
                    retainOnSubmit: props.retainOnSubmit,
                    purpose: props.purpose,
                    style: props.style,
                    font: props.font,
                    paint: paint,
                },
            });
            if (props.autoFocus) ops.push({ op: "focus", id: id });
        },
    };
}

/** Mounts `render(value)` and mounts it again, in place of the old one, whenever `value` changes. Takes the props of a `Box`. */
function Dynamic<T>({
    value: source,
    children: render,
    ...box
}: Omit<BoxProps, "children" | "ref"> & { value: Source<T>; children: (value: T) => Element }): Element {
    return Box({
        ...box,
        ref: function (id) {
            var current: Mounted | null = null;
            var first = true;
            // Children belong to the scope that mounted this element, so removing it removes them too.
            var scope = currentScope;
            var swap = function (value: T) {
                inScope(scope, function () {
                    if (current !== null) current.remove();
                    current = mountInto(id, render(value));
                });
            };
            var shown: { value: T } | null = null;
            effect(function () {
                var next = read(source);
                // The same value keeps its content: a rerun (another signal the source read changed) must not remount
                // it, which would recycle its node and callback ids for nothing.
                if (shown !== null && Object.is(shown.value, next)) return;
                shown = { value: next };
                // The first child waits until the batch that creates the container has been sent.
                if (first) {
                    first = false;
                    mountLater.push(function () {
                        swap(next);
                    });
                } else swap(next);
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
/** Mounts one child per item, keyed by `key`. Adding, removing or reordering items creates, removes or moves only those children. A child reads its item through `item()`, which changes when an item with the same key is replaced. Takes the props of a `Box`. */
function For<T>({
    each,
    key,
    children,
    ...box
}: Omit<BoxProps, "children" | "ref"> & {
    each: Source<T[]>;
    key: (item: T) => string | number;
    children: (item: () => T) => Element;
}): Element {
    return Box({
        ...box,
        ref: function (id) {
            // Children belong to the scope that mounted this list, so removing it removes them too.
            var scope = currentScope;
            var rows = new Map<string | number, KeyedRow<T>>();
            // Node ids of the children, in the order the tree has them.
            var order: number[] = [];
            var mountRow = function (item: T, before: number | undefined): KeyedRow<T> {
                var s = signal(item);
                var rowScope = childScope();
                currentScope.children.push(rowScope);
                var mounted = inScope(rowScope, function () {
                    return mountInto(id, children(function () {
                        return s.value;
                    }), before);
                });
                order.splice(placeAt(order, before), 0, mounted.id);
                return { signal: s, mounted: mounted, scope: rowScope };
            };
            var sync = function (items: T[]) {
                inScope(scope, function () {
                    var keys = items.map(key);
                    var fresh = new Set(keys);
                    if (fresh.size !== keys.length) throw new Error("For: duplicate key");
                    rows.forEach(function (row, rowKey) {
                        if (fresh.has(rowKey)) return;
                        // Out of the state first, so a sync re-entered from a cleanup cannot remove it twice;
                        // then off the screen before dispose, so its ids are freed only once its remove is sent.
                        rows.delete(rowKey);
                        order.splice(order.indexOf(row.mounted.id), 1);
                        scope.children.splice(scope.children.indexOf(row.scope), 1);
                        row.mounted.remove();
                        dispose(row.scope);
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
                                emitMove(row.mounted.id, before);
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
                var items = read(each);
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

type PopoverPlacement = { side: Side; anchorOffset: number };

/** A panel on top of the page, placed beside the node `anchor()` returns while `open` is true, 8 px from it. Escape or
 *  a press outside closes it (sets `open` to false); a press on the anchor does not, so the opener toggles it:
 *  `onPress={() => { open.value = !open.value; }}`. It is mounted in the element it is written in, so an anchor the
 *  panel names must exist when it opens. */
function Popover(props: {
    open: Signal<boolean>;
    anchor: () => number | undefined;
    side?: Side;
    align?: AnchorAlign;
    layer?: number;
    /** Distance between the anchor and the panel, in logical pixels (default 8). */
    gap?: number;
    style?: Style;
    paint?: Source<Paint>;
    /** After the final frame pass, when the resolved side or offset changes (including first placement).
     *  `side` is the side of the anchor. `anchorOffset` is the anchor center measured from the placed panel's
     *  left edge for top/bottom, or top edge for left/right, in logical pixels before panel paint transforms.
     *  The offset can fall outside the panel; clamp it against your panel size if the indicator needs clearance. */
    onPlacement?: (placement: PopoverPlacement) => void;
    children: Element;
}): Element {
    return {
        mount(parent) {
            var scope = currentScope;
            var current: Mounted | null = null;
            var show = function () {
                var node = props.anchor();
                if (node === undefined || current !== null) return;
                var anchor: Anchor = { node: node, side: props.side ?? "bottom", align: props.align ?? "start", gap: props.gap ?? 8, flip: true };
                current = mountInto(
                    parent as number,
                    {
                        mount(parent, ops) {
                            const onPlacement = props.onPlacement;
                            if (onPlacement !== undefined) {
                                var token = (typeof __jsnirPlacementEpoch === "string" ? __jsnirPlacementEpoch : "0")
                                    + ":" + nextPlacementToken++;
                                var callback = register(function (text) { onPlacement(JSON.parse(text)); });
                                placementTokens[callback] = token;
                                anchor.placement = { callback: callback, token: token };
                            }
                            Box({
                                style: { ...props.style, position: "fixed", layer: props.layer ?? 1, anchor: anchor },
                                paint: props.paint,
                                onDismiss: function () {
                                    props.open.value = false;
                                },
                                children: props.children,
                            }).mount(parent, ops);
                        },
                    },
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
/** Shows `text` beside its child while the pointer is over the child, after `delay` ms: a `Popover` on layer 1000.
 *  `font` and `textPaint` (its color, for one) style the text; `maxWidth` wraps it. */
function Tooltip(props: {
    text: Source<string>;
    side?: Side;
    delay?: number;
    font?: Font;
    textPaint?: Source<Paint>;
    maxWidth?: number;
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
                children: Text({
                    font: props.font,
                    paint: props.textPaint,
                    style: props.maxWidth === undefined ? undefined : { maxWidth: props.maxWidth },
                    children: props.text,
                }),
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
/** Sent by the embedding program (`Host::with_request_handler`): passes `json` to its handler and returns its answer. Throws when the program has no handler. Available while the app mounts. */
declare function jsnirRequest(json: string): string;
/**
 * Receives the messages the embedding Rust program sends with `Host::push`, parsed from JSON. The window itself sends `{"jsnirScale": n}` when it opens and whenever its scale factor changes (device pixels per logical pixel).
 * A live reload (`Host::reload`) gives the app a new handler, and the program is not told: a mount that needs data asks for it again, e.g. `jsnirRequest('{"cmd":"hello"}')`, and the program answers with `Host::push`.
 */
function onHost(handler: (message: any) => void): void {
    hostHandler = handler;
}
// Callback 0 is reserved for messages from Rust (`HOST_CALLBACK`); SDK callbacks start at 1.
function hostEvent(text: string): void {
    if (hostHandler !== undefined) hostHandler(JSON.parse(text));
}
// Rust calls `jsnirMount` once and `jsnirEvent` (or typed numeric dispatchers) per event, on the VM thread.
// The operations they emit form that call's batch.
function jsnirMount(): void {
    if (root === null) throw new Error("mount(App) was never called");
    inMount = true;
    try {
        var ops: Operation[] = [];
        root().mount(undefined, ops);
        send(ops);
        mountPending();
        flushCmd();
    } catch (e) {
        resetCmd();
        throw e;
    } finally {
        inMount = false;
    }
}
// Unlike other events, placement is unsolicited and may already be queued when a panel closes or reloads.
function jsnirPlacement(text: string): void {
    var event = JSON.parse(text) as PopoverPlacement & { callback: number; token: string };
    if (placementTokens[event.callback] !== event.token) return;
    jsnirEvent(event.callback, JSON.stringify({ side: event.side, anchorOffset: event.anchorOffset }), rowCallbacks.get(event.callback)?.token);
}
function jsnirEvent(callback: number, text: string, guard?: string): void {
    const row = rowCallbacks.get(callback);
    if (row?.token !== guard) return;
    const handler = callback === 0 ? hostEvent : handlers[callback - 1];
    // A press can arrive after its node was removed: there is nothing left to do.
    if (handler === undefined) return;
    runEvent(() => row === undefined ? handler(text) : inScope(row.scope, () => handler(text)));
}
var DRAG_PHASES: readonly DragPhase[] = ["start", "move", "end"];
function jsnirDrag(
    callback: number,
    phase: number,
    x: number,
    y: number,
    startX: number,
    startY: number,
    guard?: string,
): void {
    const row = rowCallbacks.get(callback);
    if (row?.token !== guard) return;
    const handler = dragHandlers[callback - 1];
    if (handler === undefined) return;
    const ev: DragEvent = {
        phase: DRAG_PHASES[phase] ?? "end",
        x,
        y,
        startX,
        startY,
        dx: x - startX,
        dy: y - startY,
    };
    runEvent(() => row === undefined ? handler(ev) : inScope(row.scope, () => handler(ev)));
}
function jsnirScroll(
    callback: number,
    top: number,
    height: number,
    client: number,
    guard?: string,
): void {
    const row = rowCallbacks.get(callback);
    if (row?.token !== guard) return;
    const handler = scrollHandlers[callback - 1];
    if (handler === undefined) return;
    const ev = { top, height, client };
    runEvent(() => row === undefined ? handler(ev) : inScope(row.scope, () => handler(ev)));
}
function jsnirContextMenu(
    callback: number,
    x: number,
    y: number,
    guard?: string,
): void {
    const row = rowCallbacks.get(callback);
    if (row?.token !== guard) return;
    const handler = contextMenuHandlers[callback - 1];
    if (handler === undefined) return;
    const ev = { x, y };
    runEvent(() => row === undefined ? handler(ev) : inScope(row.scope, () => handler(ev)));
}
function runEvent<T>(body: () => T): T {
    var previous = batching;
    batching = true;
    var result: T;
    try {
        try {
            result = body();
        } finally {
            batching = previous;
        }
        flush();
        mountPending();
        if (!previous) flushCmd();
    } catch (e) {
        if (!previous) resetCmd();
        throw e;
    }
    return result;
}
