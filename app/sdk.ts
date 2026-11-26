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
    effects?: Effect[];
    callbacks?: number[];
    children?: Scope[];
    // Ids of nodes created directly in this mounted subtree, retired on dispose.
    createdIds?: number[];
    // Virtual rows are transient: neither payloads nor local signals belong in reload state.
    ids?: number[];
    cleanup?: Array<() => void>;
    // Set by dispose: the scope's nodes left the screen with an ancestor's, or are about to with its own remove.
    disposed?: boolean;
}
function childScope(transient = currentScope.ids !== undefined): Scope {
    // Born under a disposed scope (a deferred mount that runs after its owner left), it is disposed too: nothing it
    // would mount has a parent on screen any more.
    return {
        effects: undefined,
        callbacks: undefined,
        children: undefined,
        createdIds: undefined,
        ids: transient ? [] : undefined,
        cleanup: undefined,
        disposed: currentScope.disposed,
    };
}
var currentScope: Scope = {};
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
    var effs = scope.effects;
    if (effs !== undefined) {
        for (var i = 0; i < effs.length; i++) {
            var e = effs[i];
            e.disposed = true;
            e.fn = null;
            e.src = null;
            e.apply = null;
            e.owner = null;
        }
        effs.length = 0;
        scope.effects = undefined;
    }
    var cbs = scope.callbacks;
    if (cbs !== undefined) {
        for (var j = 0; j < cbs.length; j++) {
            var id = cbs[j];
            if (id !== 0) {
                var guarded = rowCallbacks.get(id);
                if (guarded === undefined || guarded.scope === scope) unregister(id);
            }
        }
        cbs.length = 0;
        scope.callbacks = undefined;
    }
    var kids = scope.children;
    if (kids !== undefined) {
        for (var k = 0; k < kids.length; k++) dispose(kids[k]);
        kids.length = 0;
        scope.children = undefined;
    }
    var cleanups = scope.cleanup;
    if (cleanups !== undefined) {
        for (var m = 0; m < cleanups.length; m++) {
            // A throw here would drop the whole event's batch, removes included: report it and go on.
            try { cleanups[m](); } catch (error) { report(error); }
        }
        scope.cleanup = undefined;
    }
    var cids = scope.createdIds;
    if (cids !== undefined) {
        for (var ci = 0; ci < cids.length; ci++) retiring.push(cids[ci]);
        scope.createdIds = undefined;
    }
    var tids = scope.ids;
    if (tids !== undefined) {
        for (var ti = 0; ti < tids.length; ti++) retiring.push(tids[ti]);
        scope.ids = undefined;
    }
}

// ---------- Signals ----------

// Plain object (not a closure) so Effects never form self-reference cycles in QuickJS ref-counting
// and node bindings (`bindId`) allocate zero closures.
interface Effect {
    fn: (() => void) | null;
    src: any;
    id: number;
    apply: ((id: number, value: any) => void) | null;
    owner: Scope | null;
    disposed: boolean;
    queued: boolean;
}
var observer: Effect | null = null;
var queued: Effect[] = [];
var queueHead = 0;
var batching = false;

/** Reactive value. Reading `value` inside an effect subscribes to it; assigning it updates its dependents. */
interface Signal<T> {
    value: T;
    update(change: (current: T) => T): void;
}
/** A fixed value, a signal, or a function that reads signals. */
type Source<T> = T | Signal<T> | (() => T);

// Class-based non-restorable signal: zero closures, zero reference cycles, lazy subscriber list,
// and O(1) `instanceof` check without Symbol lookups.
class LocalSignal<T> implements Signal<T> {
    _v: T;
    _subs: Effect[] | null;
    constructor(initial: T) {
        this._v = initial;
        this._subs = null;
    }
    get value(): T {
        if (observer !== null) {
            var subs = this._subs;
            if (subs === null) this._subs = [observer];
            else if (subs.indexOf(observer) < 0) subs.push(observer);
        }
        return this._v;
    }
    set value(next: T) {
        if (next === this._v) return;
        this._v = next;
        var subs = this._subs;
        if (subs === null) return;
        var write = 0;
        for (var i = 0; i < subs.length; i++) {
            var sub = subs[i];
            if (!sub.disposed) {
                if (write !== i) subs[write] = sub;
                write++;
                schedule(sub);
            }
        }
        if (write !== subs.length) subs.length = write;
    }
    update(change: (current: T) => T): void {
        this.value = change(this._v);
    }
}

function signal<T>(initial: T, key?: string): Signal<T> {
    var s: LocalSignal<T> = new LocalSignal<T>(
        currentScope.ids !== undefined
            ? initial
            : restore(initial, function () { return s._v; }, key),
    );
    return s;
}
function runEffect(e: Effect): void {
    var owner = e.owner;
    if (owner === null) return;
    var prevObserver = observer;
    var prevScope = currentScope;
    observer = e;
    currentScope = owner;
    try {
        if (e.apply !== null) {
            var src = e.src;
            var v = typeof src === "function" ? src() : src.value;
            e.apply(e.id, v);
        } else if (e.fn !== null) {
            e.fn();
        }
    } finally {
        observer = prevObserver;
        currentScope = prevScope;
    }
}
function schedule(e: Effect): void {
    if (!e.queued) {
        e.queued = true;
        queued.push(e);
    }
    if (!batching) flush();
}
// Runs each queued effect once. Inside an event it waits until the callback ends.
function flush(): void {
    var previous = batching;
    batching = true;
    try {
        while (queueHead < queued.length) {
            var next = queued[queueHead++];
            next.queued = false;
            if (!next.disposed) runEffect(next);
        }
    } finally {
        if (queueHead >= queued.length) {
            queued.length = 0;
            queueHead = 0;
        }
        batching = previous;
    }
}
function effect(body: () => void): void {
    // A rerun can come from any flush, even one inside another subtree's mount: it runs in the scope that made it, so
    // what it creates belongs there and not to whatever scope happened to be current.
    var owner = currentScope;
    var e: Effect = {
        fn: body,
        src: null,
        id: 0,
        apply: null,
        owner: owner,
        disposed: false,
        queued: false,
    };
    (owner.effects ??= []).push(e);
    if (owner.disposed) {
        e.disposed = true;
        e.fn = null;
        e.owner = null;
        return;
    }
    runEffect(e);
}

/** Releases a resource when the component's mounted scope is removed. */
function onCleanup(release: () => void): void {
    (currentScope.cleanup ??= []).push(release);
}
function isSignal<T>(source: unknown): source is Signal<T> {
    return source instanceof LocalSignal;
}
function read<T>(source: Source<T>): T {
    if (typeof source === "function") return (source as () => T)();
    if (source instanceof LocalSignal) return source.value;
    return source as T;
}
/** Initial value of a source. If it is reactive, `apply` receives every later change. */
function bind<T>(source: Source<T>, apply: (value: T) => void): T {
    if (typeof source !== "function" && !(source instanceof LocalSignal)) return source as T;
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
/** Closure-free reactive binding directly targeting a node id and top-level emitter function. */
function bindId<T>(source: Source<T>, id: number, apply: (id: number, value: T) => void): T {
    if (typeof source !== "function" && !(source instanceof LocalSignal)) return source as T;
    var owner = currentScope;
    var e: Effect = {
        fn: null,
        src: source,
        id: id,
        apply: apply,
        owner: owner,
        disposed: false,
        queued: false,
    };
    (owner.effects ??= []).push(e);
    if (owner.disposed) {
        e.disposed = true;
        e.src = null;
        e.apply = null;
        e.owner = null;
        return undefined as unknown as T;
    }
    var prevObserver = observer;
    var prevScope = currentScope;
    observer = e;
    currentScope = owner;
    try {
        return typeof source === "function" ? (source as () => T)() : (source as LocalSignal<T>).value;
    } finally {
        observer = prevObserver;
        currentScope = prevScope;
    }
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

/** What a component returns: it streams its node and children directly into the binary command buffer. */
interface Element {
    mount(parent: number | undefined, before: number | undefined, created: number[] | null): number;
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
    (currentScope.callbacks ??= []).push(id);
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
        if (callbacks !== undefined) callbacks[callbacks.indexOf(id)] = 0; // do not unregister a future owner of this id
        emitCallbackGuard(id, undefined);
    }
    if (placementTokens[id] !== undefined) delete placementTokens[id];
    freeCallbacks.push(id);
}

// ---------- Binary Command Buffer ----------
// Every JSNIR op and node creation is streamed directly into the shared `jsnir.cmd` / `jsnir.cmdF32`
// ArrayBuffer (with strings in `cmdStrs`) in a single pass and flushed via `jsnir.commit` /
// `jsnir.commitStrings` without intermediate JS Operation/Node objects, `JSON.stringify`, or `serde_json`.
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
var OP_CREATE_INPUT = 20;
var OP_CREATE_LIST = 21;
var OP_SET_FIELD = 22;
var OP_SET_SPANS = 23;
var OP_VIRTUAL_LIST = 24;
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
var PROP_CODES: { [k: string]: number | undefined } = {
    opacity: 0,
    x: 1,
    y: 2,
    scale: 3,
    brightness: 4,
    background: 5,
    edge: 6,
    color: 7,
    draw: 8,
    left: 9,
    width: 10,
    height: 11,
    radius: 12,
    scaleX: 13,
    scaleY: 14,
};
var RESIZE_CODES: { [k: string]: number | undefined } = {
    clip: 0,
    crossfade: 1,
};
var EASING_CODES: { [k: string]: number | undefined } = {
    linear: 0,
    ease: 1,
    easeOut: 2,
    easeInOut: 3,
    spring: 4,
    bezier: 5,
};
var SIDE_CODES: { [k: string]: number | undefined } = {
    top: 0,
    right: 1,
    bottom: 2,
    left: 3,
};
var A11Y_ROLE_CODES: { [k: string]: number | undefined } = {
    generic: 0,
    group: 1,
    region: 2,
    navigation: 3,
    main: 4,
    toolbar: 5,
    button: 6,
    link: 7,
    checkBox: 8,
    switch: 9,
    radioButton: 10,
    tab: 11,
    tabList: 12,
    tabPanel: 13,
    menu: 14,
    menuItem: 15,
    dialog: 16,
    alertDialog: 17,
    alert: 18,
    status: 19,
    tooltip: 20,
    heading: 21,
    label: 22,
    list: 23,
    listItem: 24,
    tree: 25,
    treeItem: 26,
    scrollView: 27,
    slider: 28,
    progressIndicator: 29,
    image: 30,
    terminal: 31,
};
var A11Y_LIVE_CODES: { [k: string]: number | undefined } = {
    off: 0,
    polite: 1,
    assertive: 2,
};
var ANCHOR_ALIGN_CODES: { [k: string]: number | undefined } = {
    start: 0,
    center: 1,
    end: 2,
};
var PURPOSE_CODES: { [k: string]: number | undefined } = {
    normal: 0,
    password: 1,
    search: 2,
    email: 3,
    number: 4,
};
// Fixed-size ArrayBuffer views installed once per VM: cache locally to avoid global property lookups per op.
var cmdU32 = jsnir.cmd;
var cmdF32 = jsnir.cmdF32;
var cmdCap = cmdU32.length;
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
function reserveCmd(words: number): boolean {
    if (words > cmdCap) return false;
    if (cmdPos + words > cmdCap) flushCmd();
    return true;
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
    return (v & 0xffff) === v;
}
function sendJsonOp(op: Operation): void {
    flushCmd();
    jsnir.batch(JSON.stringify({ ops: [op] }));
}
/** Sends raw operations through the binary command buffer (falling back to JSON only if unencodable). */
function send(ops: Operation[]): void {
    for (var i = 0; i < ops.length; i++) {
        var op = ops[i];
        switch (op.op) {
            case "setstyle": emitSetStyle(op.id, op.style); break;
            case "setpaint": emitSetPaint(op.id, op.paint); break;
            case "settext": emitSetText(op.id, op.text); break;
            case "setfield": emitSetField(op.id, op.text, op.expected); break;
            case "setspans": emitSetSpans(op.id, op.spans); break;
            case "setsource": emitSetSource(op.id, op.source); break;
            case "set": emitSet(op.id, op.value); break;
            case "setdisabled": emitSetDisabled(op.id, op.disabled); break;
            case "remove": emitRemove(op.id); break;
            case "move": emitMove(op.id, op.before); break;
            case "focus": emitFocus(op.id); break;
            case "after": emitAfter(op.callback, op.ms); break;
            case "scrollto": emitScrollTo(op.id, op); break;
            case "virtuallist": emitVirtualList(op.id, op.name); break;
            case "callbackguard": emitCallbackGuard(op.callback, op.token); break;
            case "create": {
                var n = op.node;
                switch (n.kind) {
                    case "box":
                        emitCreateBox(op.id, op.parent, op.before, n.style, n.paint, n.press ?? 0, n.disabled ?? false, n.hover ?? 0, n.contextmenu ?? 0, n.dismiss ?? 0, n.scroll ?? 0, n.drag ?? 0);
                        break;
                    case "text":
                        emitCreateText(op.id, op.parent, op.before, n.text, n.style, n.font, n.paint, n.lines, n.align, n.spans);
                        break;
                    case "icon":
                        emitCreateIcon(op.id, op.parent, op.before, n.icon, n.size, n.stroke, n.style, n.paint);
                        break;
                    case "image":
                        emitCreateImage(op.id, op.parent, op.before, n.source, n.style, n.paint);
                        break;
                    case "label":
                        emitCreateLabel(op.id, op.parent, op.before, n.value);
                        break;
                    case "button":
                        emitCreateButton(op.id, op.parent, op.before, n.callback);
                        break;
                    case "list":
                        emitCreateList(op.id, op.parent, op.before, n.scroll ?? 0, n.style);
                        break;
                    case "input":
                        emitCreateInput(op.id, op.parent, op.before, n.text, n.selection, n.submit ?? 0, n.change ?? 0, n.placeholder, n.placeholderColor, n.multiline, n.submitOnEnter, n.retainOnSubmit, n.purpose, n.style, n.font, n.paint);
                        break;
                    case "column":
                        for (var attempt = 0; attempt < 2; attempt++) {
                            if (cmdPos + 16 > cmdCap) flushCmd();
                            var sp = cmdPos;
                            var ss = cmdStrs.length;
                            var p = sp + 5;
                            var flags = 0;
                            if (n.style !== undefined) { flags = 1; p = tryWriteStyle(p, n.style, 0); }
                            if (p >= 0) {
                                cmdU32[sp] = OP_CREATE_COLUMN;
                                cmdU32[sp + 1] = op.id;
                                cmdU32[sp + 2] = op.parent === undefined ? 0xffffffff : op.parent;
                                cmdU32[sp + 3] = op.before === undefined ? 0xffffffff : op.before;
                                cmdU32[sp + 4] = flags;
                                cmdPos = p;
                                break;
                            }
                            cmdPos = sp;
                            cmdStrs.length = ss;
                            if (p === -2 && sp > 0) { flushCmd(); continue; }
                            sendJsonOp(op);
                            break;
                        }
                        break;
                }
                break;
            }
            default:
                sendJsonOp(op);
                break;
        }
    }
    maybeFlushCmd();
}
// Direct compile-time JSAtom bitmask encoders matching `src/contract.rs`.
// Benchmarked 2.15x faster in QuickJS than `for..in` tagged iteration (zero iterator/string allocations).
// Return value: next write offset `>= 0`, `-1` for invalid/unencodable value (JSON fallback),
// `-2` for buffer full (caller flushes and retries once at offset 0).
function tryWriteStyle(start: number, s: Style | undefined, forcedDir: number): number {
    var u = cmdU32;
    if (s === undefined) {
        if (start + 2 > cmdCap) return -2;
        u[start] = 0;
        u[start + 1] = forcedDir === 1 ? 1 : 0;
        return start + 2;
    }
    if (start + 30 > cmdCap) return -2;
    var f = cmdF32;
    var p = start + 2;
    var mask = 0;
    var flags = forcedDir === 1 ? 1 : 0;
    if (forcedDir === 0) {
        var dir = s.direction;
        if (dir !== undefined) {
            if (dir === "row") flags |= 1;
            else if (dir !== "column") return -1;
        }
    }
    var align = s.align;
    if (align !== undefined) {
        var ac = align === "stretch" ? 1 : align === "start" ? 2 : align === "center" ? 3 : align === "end" ? 4 : -1;
        if (ac < 0) return -1;
        flags |= ac << 1;
    }
    var justify = s.justify;
    if (justify !== undefined) {
        var jc = justify === "start" ? 0 : justify === "center" ? 1 : justify === "end" ? 2 : justify === "between" ? 3 : -1;
        if (jc < 0) return -1;
        flags |= jc << 4;
    }
    var pos = s.position;
    if (pos !== undefined) {
        var pc = pos === "relative" ? 0 : pos === "absolute" ? 1 : pos === "fixed" ? 2 : -1;
        if (pc < 0) return -1;
        flags |= pc << 6;
    }
    if (s.clip) flags |= 1 << 8;
    if (s.scroll) flags |= 1 << 9;
    var sa = s.scrollAnchor;
    if (sa !== undefined) {
        if (sa === "end") flags |= 1 << 10;
        else if (sa !== "start") return -1;
    }
    if (s.group) flags |= 1 << 11;

    var v: any;
    v = s.width; if (v !== undefined) { mask |= 1 << 0; f[p++] = v; }
    v = s.height; if (v !== undefined) { mask |= 1 << 1; f[p++] = v; }
    v = s.grow; if (v !== undefined) { mask |= 1 << 2; f[p++] = v; }
    v = s.shrink; if (v !== undefined) { mask |= 1 << 3; f[p++] = v; }
    v = s.basis; if (v !== undefined) { mask |= 1 << 4; f[p++] = v; }
    v = s.padding;
    if (v !== undefined) {
        mask |= 1 << 5;
        if (typeof v === "number") {
            f[p] = v; f[p + 1] = v; f[p + 2] = v; f[p + 3] = v; p += 4;
        } else if (Array.isArray(v) && v.length === 4) {
            f[p] = v[0]; f[p + 1] = v[1]; f[p + 2] = v[2]; f[p + 3] = v[3]; p += 4;
        } else return -1;
    }
    v = s.margin;
    if (v !== undefined) {
        mask |= 1 << 6;
        if (typeof v === "number") {
            f[p] = v; f[p + 1] = v; f[p + 2] = v; f[p + 3] = v; p += 4;
        } else if (Array.isArray(v) && v.length === 4) {
            f[p] = v[0]; f[p + 1] = v[1]; f[p + 2] = v[2]; f[p + 3] = v[3]; p += 4;
        } else return -1;
    }
    v = s.gap; if (v !== undefined) { mask |= 1 << 7; f[p++] = v; }
    v = s.left; if (v !== undefined) { mask |= 1 << 8; f[p++] = v; }
    v = s.top; if (v !== undefined) { mask |= 1 << 9; f[p++] = v; }
    v = s.follow;
    if (v !== undefined) {
        if ((v & 0xffff) !== v) return -1;
        mask |= 1 << 10; u[p++] = v;
    }
    v = s.layer; if (v !== undefined) { mask |= 1 << 11; u[p++] = (v | 0) >>> 0; }
    v = s.minWidth; if (v !== undefined) { mask |= 1 << 12; f[p++] = v; }
    v = s.minHeight; if (v !== undefined) { mask |= 1 << 13; f[p++] = v; }
    v = s.maxWidth; if (v !== undefined) { mask |= 1 << 14; f[p++] = v; }
    v = s.maxHeight; if (v !== undefined) { mask |= 1 << 15; f[p++] = v; }
    v = s.anchor;
    if (v !== undefined) {
        var a = v as Anchor;
        var aNode = a.node ?? 0;
        if ((aNode & 0xffff) !== aNode) return -1;
        var side = a.side === undefined ? 0 : SIDE_CODES[a.side];
        var aalign = a.align === undefined ? 0 : ANCHOR_ALIGN_CODES[a.align];
        if (side === undefined || aalign === undefined) return -1;
        var aflags = side | (aalign << 2) | (a.flip ? (1 << 4) : 0);
        var pl = a.placement;
        if (pl !== undefined) {
            var plCb = pl.callback ?? 0;
            if ((plCb & 0xffff) !== plCb) return -1;
            aflags |= 1 << 5;
        }
        mask |= 1 << 16;
        u[p++] = aNode;
        u[p++] = aflags;
        f[p++] = a.gap ?? 0;
        if (pl !== undefined) {
            u[p++] = pl.callback ?? 0;
            u[p++] = pushStr(pl.token ?? "");
        }
    }
    u[start] = mask;
    u[start + 1] = flags;
    return p;
}
function tryWriteVariant(start: number, val: Variant): number {
    if (start + 18 > cmdCap) return -2;
    var u = cmdU32;
    var f = cmdF32;
    var p = start + 1;
    var mask = 0;
    var v: any;
    v = val.background; if (v !== undefined) { mask |= 1 << 0; u[p++] = v >>> 0; }
    v = val.gradient;
    if (v !== undefined) {
        if (!Array.isArray(v) || v.length !== 2) return -1;
        mask |= 1 << 1; u[p++] = v[0] >>> 0; u[p++] = v[1] >>> 0;
    }
    v = val.brightness; if (v !== undefined) { mask |= 1 << 2; f[p++] = v; }
    v = val.scale; if (v !== undefined) { mask |= 1 << 3; f[p++] = v; }
    v = val.opacity; if (v !== undefined) { mask |= 1 << 4; f[p++] = v; }
    v = val.x; if (v !== undefined) { mask |= 1 << 5; f[p++] = v; }
    v = val.y; if (v !== undefined) { mask |= 1 << 6; f[p++] = v; }
    v = val.color; if (v !== undefined) { mask |= 1 << 7; u[p++] = v >>> 0; }
    v = val.edge; if (v !== undefined) { mask |= 1 << 8; u[p++] = v >>> 0; }
    v = val.height; if (v !== undefined) { mask |= 1 << 9; f[p++] = v; }
    v = val.width; if (v !== undefined) { mask |= 1 << 10; f[p++] = v; }
    v = val.underline; if (v !== undefined) { mask |= 1 << 11; u[p++] = v ? 1 : 0; }
    v = val.cursor;
    if (v !== undefined) {
        var cc = CURSOR_CODES[v];
        if (cc === undefined) return -1;
        mask |= 1 << 12; u[p++] = cc;
    }
    v = val.scaleX; if (v !== undefined) { mask |= 1 << 13; f[p++] = v; }
    v = val.scaleY; if (v !== undefined) { mask |= 1 << 14; f[p++] = v; }
    u[start] = mask;
    return p;
}
function tryWritePaint(start: number, paint: Paint): number {
    if (start + 26 > cmdCap) return -2;
    var u = cmdU32;
    var f = cmdF32;
    var p = start + 1;
    var mask = 0;
    var v: any;
    v = paint.background; if (v !== undefined) { mask |= 1 << 0; u[p++] = v >>> 0; }
    v = paint.gradient;
    if (v !== undefined) {
        if (!Array.isArray(v) || v.length !== 2) return -1;
        mask |= 1 << 1; u[p++] = v[0] >>> 0; u[p++] = v[1] >>> 0;
    }
    v = paint.radius;
    if (v !== undefined) {
        mask |= 1 << 2;
        if (typeof v === "number") {
            f[p] = v; f[p + 1] = v; f[p + 2] = v; f[p + 3] = v; p += 4;
        } else if (Array.isArray(v) && v.length === 4) {
            f[p] = v[0]; f[p + 1] = v[1]; f[p + 2] = v[2]; f[p + 3] = v[3]; p += 4;
        } else return -1;
    }
    v = paint.edge; if (v !== undefined) { mask |= 1 << 3; u[p++] = v >>> 0; }
    v = paint.edgeWidth; if (v !== undefined) { mask |= 1 << 4; f[p++] = v; }
    v = paint.edgeSides;
    if (v !== undefined) {
        if (!Array.isArray(v) || v.length !== 4) return -1;
        mask |= 1 << 5;
        f[p] = v[0]; f[p + 1] = v[1]; f[p + 2] = v[2]; f[p + 3] = v[3]; p += 4;
    }
    v = paint.highlight; if (v !== undefined) { mask |= 1 << 6; u[p++] = v >>> 0; }
    v = paint.opacity; if (v !== undefined) { mask |= 1 << 7; f[p++] = v; }
    v = paint.x; if (v !== undefined) { mask |= 1 << 8; f[p++] = v; }
    v = paint.y; if (v !== undefined) { mask |= 1 << 9; f[p++] = v; }
    v = paint.scale; if (v !== undefined) { mask |= 1 << 10; f[p++] = v; }
    v = paint.brightness; if (v !== undefined) { mask |= 1 << 11; f[p++] = v; }
    v = paint.color; if (v !== undefined) { mask |= 1 << 12; u[p++] = v >>> 0; }
    v = paint.draw; if (v !== undefined) { mask |= 1 << 13; f[p++] = v; }
    v = paint.cursor;
    if (v !== undefined) {
        var cc = CURSOR_CODES[v];
        if (cc === undefined) return -1;
        mask |= 1 << 14; u[p++] = cc;
    }
    v = paint.hover;
    if (v !== undefined) {
        mask |= 1 << 15;
        p = tryWriteVariant(p, v);
        if (p < 0) return p;
    }
    v = paint.active;
    if (v !== undefined) {
        mask |= 1 << 16;
        p = tryWriteVariant(p, v);
        if (p < 0) return p;
    }
    v = paint.focus;
    if (v !== undefined) {
        mask |= 1 << 17;
        p = tryWriteVariant(p, v);
        if (p < 0) return p;
    }
    v = paint.mask;
    if (v !== undefined) {
        if (p + 1 > cmdCap) return -2;
        mask |= 1 << 18;
        u[p++] = pushStr(v);
    }
    v = paint.shadows;
    if (v !== undefined && v.length > 0) {
        var sh = v as Shadow[];
        if (p + 1 + sh.length * 4 > cmdCap) return -2;
        mask |= 1 << 19;
        u[p++] = sh.length;
        for (var i = 0; i < sh.length; i++) {
            var s = sh[i];
            f[p++] = s.y ?? 0; f[p++] = s.blur ?? 0; f[p++] = s.spread ?? 0; u[p++] = (s.color ?? 0) >>> 0;
        }
    }
    v = paint.transitions;
    if (v !== undefined && v.length > 0) {
        var trs = v as Transition[];
        if (p + 1 + trs.length * 8 > cmdCap) return -2;
        mask |= 1 << 20;
        u[p++] = trs.length;
        for (var i = 0; i < trs.length; i++) {
            var t = trs[i];
            var prop = t.property === undefined ? 0 : PROP_CODES[t.property];
            var ease = t.easing === undefined ? 0 : EASING_CODES[t.easing];
            if (prop === undefined || ease === undefined) return -1;
            var hasBez = t.bezier !== undefined;
            if (hasBez && (!Array.isArray(t.bezier) || t.bezier.length !== 4)) return -1;
            u[p++] = prop | (ease << 4) | (hasBez ? (1 << 7) : 0);
            f[p++] = t.duration ?? 0; f[p++] = t.delay ?? 0; f[p++] = t.hold ?? 0;
            if (hasBez) {
                var b = t.bezier as [number, number, number, number];
                f[p++] = b[0]; f[p++] = b[1]; f[p++] = b[2]; f[p++] = b[3];
            }
        }
    }
    v = paint.animations;
    if (v !== undefined) {
        var anims = v as Animation[];
        if (p + 1 > cmdCap) return -2;
        mask |= 1 << 21;
        u[p++] = anims.length;
        for (var i = 0; i < anims.length; i++) {
            var a = anims[i];
            var kfs = a.keyframes ?? [];
            if (p + 10 + kfs.length * 4 > cmdCap) return -2;
            var prop = a.property === undefined ? 0 : PROP_CODES[a.property];
            var ease = a.easing === undefined ? 0 : EASING_CODES[a.easing];
            if (prop === undefined || ease === undefined) return -1;
            var hasBez = a.bezier !== undefined;
            if (hasBez && (!Array.isArray(a.bezier) || a.bezier.length !== 4)) return -1;
            var hasIter = a.iterations !== undefined;
            u[p++] = prop | (ease << 4) | (hasBez ? (1 << 7) : 0) | (hasIter ? (1 << 8) : 0) | (a.alternate ? (1 << 9) : 0);
            f[p++] = a.duration ?? 0; f[p++] = a.delay ?? 0;
            if (hasBez) {
                var b = a.bezier as [number, number, number, number];
                f[p++] = b[0]; f[p++] = b[1]; f[p++] = b[2]; f[p++] = b[3];
            }
            if (hasIter) f[p++] = a.iterations as number;
            u[p++] = kfs.length;
            for (var ki = 0; ki < kfs.length; ki++) {
                var kf = kfs[ki];
                u[p++] = (kf.value !== undefined ? 1 : 0) | (kf.color !== undefined ? 2 : 0);
                f[p++] = kf.offset ?? 0;
                if (kf.value !== undefined) f[p++] = kf.value;
                if (kf.color !== undefined) u[p++] = kf.color >>> 0;
            }
        }
    }
    v = paint.starting;
    if (v !== undefined) {
        if (p + 9 > cmdCap) return -2;
        var st = v as Starting;
        mask |= 1 << 22;
        var stStart = p++;
        var smask = 0;
        if (st.opacity !== undefined) { smask |= 1 << 0; f[p++] = st.opacity; }
        if (st.x !== undefined) { smask |= 1 << 1; f[p++] = st.x; }
        if (st.y !== undefined) { smask |= 1 << 2; f[p++] = st.y; }
        if (st.scale !== undefined) { smask |= 1 << 3; f[p++] = st.scale; }
        if (st.draw !== undefined) { smask |= 1 << 4; f[p++] = st.draw; }
        if (st.scaleX !== undefined) { smask |= 1 << 5; f[p++] = st.scaleX; }
        if (st.scaleY !== undefined) { smask |= 1 << 6; f[p++] = st.scaleY; }
        u[stStart] = smask;
    }
    v = paint.scrollbar;
    if (v !== undefined) {
        if (p + 8 > cmdCap) return -2;
        var sb = v as Scrollbar;
        mask |= 1 << 23;
        u[p++] = (sb.track !== undefined ? 1 : 0) | (sb.autoHide ? 2 : 0);
        f[p++] = sb.width ?? 0;
        u[p++] = (sb.thumb ?? 0) >>> 0;
        f[p++] = sb.radius ?? 0;
        f[p++] = sb.minThumb ?? 0;
        f[p++] = sb.inset ?? 0;
        if (sb.track !== undefined) u[p++] = sb.track >>> 0;
    }
    v = paint.exiting;
    if (v !== undefined) {
        if (p + 8 > cmdCap) return -2;
        var ex = v as Exiting;
        mask |= 1 << 24;
        var exStart = p++;
        var emask = 0;
        if (ex.opacity !== undefined) { emask |= 1 << 0; f[p++] = ex.opacity; }
        if (ex.x !== undefined) { emask |= 1 << 1; f[p++] = ex.x; }
        if (ex.y !== undefined) { emask |= 1 << 2; f[p++] = ex.y; }
        if (ex.scale !== undefined) { emask |= 1 << 3; f[p++] = ex.scale; }
        if (ex.scaleX !== undefined) { emask |= 1 << 4; f[p++] = ex.scaleX; }
        if (ex.scaleY !== undefined) { emask |= 1 << 5; f[p++] = ex.scaleY; }
        u[exStart] = emask;
    }
    v = paint.a11y;
    if (v !== undefined) {
        if (p + 10 > cmdCap) return -2;
        var a11y = v as A11y;
        var roleCode: number | undefined = undefined;
        if (a11y.role !== undefined) {
            roleCode = A11Y_ROLE_CODES[a11y.role];
            if (roleCode === undefined) return -1;
        }
        var liveCode: number | undefined = undefined;
        if (a11y.live !== undefined) {
            liveCode = A11Y_LIVE_CODES[a11y.live];
            if (liveCode === undefined) return -1;
        }
        var levelVal = a11y.level;
        if (levelVal !== undefined && ((levelVal & 0xffff) !== levelVal)) return -1;
        mask |= 1 << 25;
        var aStart = p++;
        var amask = 0;
        if (roleCode !== undefined) { amask |= 1 << 0; u[p++] = roleCode; }
        if (a11y.label !== undefined) { amask |= 1 << 1; u[p++] = pushStr(a11y.label); }
        if (a11y.description !== undefined) { amask |= 1 << 2; u[p++] = pushStr(a11y.description); }
        if (a11y.value !== undefined) { amask |= 1 << 3; u[p++] = pushStr(a11y.value); }
        if (a11y.checked !== undefined) { amask |= 1 << 4; if (a11y.checked) amask |= 1 << 16; }
        if (a11y.selected !== undefined) { amask |= 1 << 5; if (a11y.selected) amask |= 1 << 17; }
        if (a11y.expanded !== undefined) { amask |= 1 << 6; if (a11y.expanded) amask |= 1 << 18; }
        if (a11y.hidden) amask |= 1 << 7;
        if (a11y.modal) amask |= 1 << 8;
        if (levelVal !== undefined) { amask |= 1 << 9; u[p++] = levelVal; }
        if (liveCode !== undefined) { amask |= 1 << 10; u[p++] = liveCode; }
        if (a11y.focusable !== undefined) { amask |= 1 << 11; if (a11y.focusable) amask |= 1 << 19; }
        if (a11y.numericValue !== undefined) { amask |= 1 << 12; f[p++] = a11y.numericValue; }
        if (a11y.minValue !== undefined) { amask |= 1 << 13; f[p++] = a11y.minValue; }
        if (a11y.maxValue !== undefined) { amask |= 1 << 14; f[p++] = a11y.maxValue; }
        u[aStart] = amask;
    }
    var resizeCode: number | undefined = undefined;
    if (paint.resize !== undefined) {
        resizeCode = RESIZE_CODES[paint.resize];
        if (resizeCode === undefined) return -1;
    }
    if (p + 3 > cmdCap) return -2;
    v = paint.scaleX; if (v !== undefined) { mask |= 1 << 26; f[p++] = v; }
    v = paint.scaleY; if (v !== undefined) { mask |= 1 << 27; f[p++] = v; }
    if (resizeCode !== undefined) { mask |= 1 << 28; u[p++] = resizeCode; }
    u[start] = mask;
    return p;
}
function tryWriteFont(start: number, font: Font): number {
    if (start + 6 > cmdCap) return -2;
    var u = cmdU32;
    var f = cmdF32;
    var p = start + 1;
    var mask = 0;
    var v: any;
    v = font.size; if (v !== undefined) { mask |= 1 << 0; f[p++] = v; }
    v = font.weight;
    if (v !== undefined) {
        if ((v & 0xffff) !== v) return -1;
        mask |= 1 << 1; u[p++] = v;
    }
    v = font.lineHeight; if (v !== undefined) { mask |= 1 << 2; f[p++] = v; }
    v = font.letterSpacing; if (v !== undefined) { mask |= 1 << 3; f[p++] = v; }
    v = font.family; if (v !== undefined) { mask |= 1 << 4; u[p++] = pushStr(v); }
    u[start] = mask;
    return p;
}
function tryWriteSpans(start: number, spans: Span[]): number {
    if (start + 1 > cmdCap) return -2;
    var u = cmdU32;
    var f = cmdF32;
    var p = start;
    u[p++] = spans.length;
    for (var i = 0; i < spans.length; i++) {
        if (p + 12 > cmdCap) return -2;
        var sp = spans[i];
        var hdr = p++;
        var mask = 0;
        var v: any;
        v = sp.text; if (v !== undefined && v !== "") { mask |= 1 << 0; u[p++] = pushStr(v); }
        v = sp.color; if (v !== undefined) { mask |= 1 << 1; u[p++] = v >>> 0; }
        v = sp.weight;
        if (v !== undefined) {
            if ((v & 0xffff) !== v) return -1;
            mask |= 1 << 2; u[p++] = v;
        }
        v = sp.size; if (v !== undefined) { mask |= 1 << 3; f[p++] = v; }
        v = sp.family; if (v !== undefined) { mask |= 1 << 4; u[p++] = pushStr(v); }
        if (sp.underline) mask |= 1 << 5;
        if (sp.italic) mask |= 1 << 6;
        if (sp.strike) mask |= 1 << 7;
        v = sp.background; if (v !== undefined) { mask |= 1 << 8; u[p++] = v >>> 0; }
        v = sp.backgroundRadius; if (v !== undefined) { mask |= 1 << 9; f[p++] = v; }
        v = sp.image;
        if (v !== undefined) {
            mask |= 1 << 10;
            u[p++] = pushStr(v.source ?? "");
            f[p++] = v.width ?? 0;
            f[p++] = v.height ?? 0;
        }
        u[hdr] = mask;
    }
    return p;
}
function emitSet(id: number, value: number): void {
    if (!isU16(value)) {
        flushCmd();
        jsnir.set(id, value);
        return;
    }
    reserveCmd(3);
    var p = cmdPos;
    cmdU32[p] = OP_SET;
    cmdU32[p + 1] = id;
    cmdU32[p + 2] = value;
    cmdPos = p + 3;
    maybeFlushCmd();
}
function emitSetDisabled(id: number, disabled: boolean): void {
    reserveCmd(3);
    var p = cmdPos;
    cmdU32[p] = OP_SET_DISABLED;
    cmdU32[p + 1] = id;
    cmdU32[p + 2] = disabled ? 1 : 0;
    cmdPos = p + 3;
    maybeFlushCmd();
}
function emitRemove(id: number): void {
    reserveCmd(2);
    var p = cmdPos;
    cmdU32[p] = OP_REMOVE;
    cmdU32[p + 1] = id;
    cmdPos = p + 2;
    maybeFlushCmd();
}
function emitMove(id: number, before: number | undefined): void {
    reserveCmd(3);
    var p = cmdPos;
    cmdU32[p] = OP_MOVE;
    cmdU32[p + 1] = id;
    cmdU32[p + 2] = before === undefined ? 0xffffffff : before;
    cmdPos = p + 3;
    maybeFlushCmd();
}
function emitFocus(id: number): void {
    reserveCmd(2);
    var p = cmdPos;
    cmdU32[p] = OP_FOCUS;
    cmdU32[p + 1] = id;
    cmdPos = p + 2;
    maybeFlushCmd();
}
function emitAfter(callback: number, ms: number): void {
    reserveCmd(3);
    var p = cmdPos;
    cmdU32[p] = OP_AFTER;
    cmdU32[p + 1] = callback;
    cmdF32[p + 2] = ms;
    cmdPos = p + 3;
    maybeFlushCmd();
}
function emitScrollTo(id: number, to: { offset?: number; to?: ScrollEdge; smooth?: boolean }): void {
    reserveCmd(4);
    var p = cmdPos;
    var flags = (to.offset !== undefined ? 1 : 0)
        | (to.to !== undefined ? 2 : 0)
        | (to.to === "end" ? 4 : 0)
        | (to.smooth ? 8 : 0);
    cmdU32[p] = OP_SCROLL_TO;
    cmdU32[p + 1] = id;
    cmdU32[p + 2] = flags;
    cmdF32[p + 3] = to.offset ?? 0;
    cmdPos = p + 4;
    maybeFlushCmd();
}
function emitSetText(id: number, text: string): void {
    reserveCmd(3);
    var p = cmdPos;
    cmdU32[p] = OP_SET_TEXT;
    cmdU32[p + 1] = id;
    cmdU32[p + 2] = pushStr(text);
    cmdPos = p + 3;
    maybeFlushCmd();
}
function emitSetField(id: number, text: string, expected: string): void {
    reserveCmd(4);
    var p = cmdPos;
    cmdU32[p] = OP_SET_FIELD;
    cmdU32[p + 1] = id;
    cmdU32[p + 2] = pushStr(text);
    cmdU32[p + 3] = pushStr(expected);
    cmdPos = p + 4;
    maybeFlushCmd();
}
function emitSetSpans(id: number, spans: Span[]): void {
    for (var attempt = 0; attempt < 2; attempt++) {
        if (cmdPos + 8 > cmdCap) flushCmd();
        var savedPos = cmdPos;
        var savedStrs = cmdStrs.length;
        cmdU32[savedPos] = OP_SET_SPANS;
        cmdU32[savedPos + 1] = id;
        var end = tryWriteSpans(savedPos + 2, spans);
        if (end >= 0) {
            cmdPos = end;
            maybeFlushCmd();
            return;
        }
        cmdPos = savedPos;
        cmdStrs.length = savedStrs;
        if (end === -2 && savedPos > 0) { flushCmd(); continue; }
        break;
    }
    sendJsonOp({ op: "setspans", id: id, spans: spans });
}
function emitSetSource(id: number, source: string): void {
    reserveCmd(3);
    var p = cmdPos;
    cmdU32[p] = OP_SET_SOURCE;
    cmdU32[p + 1] = id;
    cmdU32[p + 2] = pushStr(source);
    cmdPos = p + 3;
    maybeFlushCmd();
}
function emitCallbackGuard(callback: number, token: string | undefined): void {
    reserveCmd(3);
    var p = cmdPos;
    cmdU32[p] = OP_CALLBACK_GUARD;
    cmdU32[p + 1] = callback;
    cmdU32[p + 2] = token === undefined ? 0xffffffff : pushStr(token);
    cmdPos = p + 3;
    maybeFlushCmd();
}
function emitVirtualList(id: number, name: string): void {
    reserveCmd(3);
    var p = cmdPos;
    cmdU32[p] = OP_VIRTUAL_LIST;
    cmdU32[p + 1] = id;
    cmdU32[p + 2] = pushStr(name);
    cmdPos = p + 3;
    maybeFlushCmd();
}
function emitSetStyle(id: number, s: Style, forcedDir = 0): void {
    for (var attempt = 0; attempt < 2; attempt++) {
        if (cmdPos + 16 > cmdCap) flushCmd();
        var savedPos = cmdPos;
        var savedStrs = cmdStrs.length;
        cmdU32[savedPos] = OP_SET_STYLE;
        cmdU32[savedPos + 1] = id;
        var end = tryWriteStyle(savedPos + 2, s, forcedDir);
        if (end >= 0) {
            cmdPos = end;
            maybeFlushCmd();
            return;
        }
        cmdPos = savedPos;
        cmdStrs.length = savedStrs;
        if (end === -2 && savedPos > 0) { flushCmd(); continue; }
        break;
    }
    var styleOut = forcedDir === 1 ? { ...s, direction: "row" as Axis } : forcedDir === 2 ? { ...s, direction: "column" as Axis } : s;
    sendJsonOp({ op: "setstyle", id: id, style: styleOut });
}
function emitSetStyleRow(id: number, s: Style): void { emitSetStyle(id, s, 1); }
function emitSetStyleCol(id: number, s: Style): void { emitSetStyle(id, s, 2); }
function emitSetPaint(id: number, paint: Paint): void {
    for (var attempt = 0; attempt < 2; attempt++) {
        if (cmdPos + 16 > cmdCap) flushCmd();
        var savedPos = cmdPos;
        var savedStrs = cmdStrs.length;
        cmdU32[savedPos] = OP_SET_PAINT;
        cmdU32[savedPos + 1] = id;
        var end = tryWritePaint(savedPos + 2, paint);
        if (end >= 0) {
            cmdPos = end;
            maybeFlushCmd();
            return;
        }
        cmdPos = savedPos;
        cmdStrs.length = savedStrs;
        if (end === -2 && savedPos > 0) { flushCmd(); continue; }
        break;
    }
    sendJsonOp({ op: "setpaint", id: id, paint: paint });
}
function emitCreateBox(
    id: number,
    parent: number | undefined,
    before: number | undefined,
    style: Style | undefined,
    paint: Paint | undefined,
    press: number,
    disabled: boolean,
    hover: number,
    contextmenu: number,
    dismiss: number,
    scroll: number,
    drag: number,
    forcedDir = 0,
): void {
    var hasStyle = style !== undefined || forcedDir === 1;
    for (var attempt = 0; attempt < 2; attempt++) {
        if (cmdPos + 24 > cmdCap) flushCmd();
        var savedPos = cmdPos;
        var savedStrs = cmdStrs.length;
        var p = savedPos + 5;
        var flags = disabled ? 1 : 0;
        if (press) { flags |= 1 << 1; cmdU32[p++] = press; }
        if (hover) { flags |= 1 << 2; cmdU32[p++] = hover; }
        if (contextmenu) { flags |= 1 << 3; cmdU32[p++] = contextmenu; }
        if (dismiss) { flags |= 1 << 4; cmdU32[p++] = dismiss; }
        if (scroll) { flags |= 1 << 5; cmdU32[p++] = scroll; }
        if (drag) { flags |= 1 << 6; cmdU32[p++] = drag; }
        if (hasStyle) {
            flags |= 1 << 7;
            p = tryWriteStyle(p, style, forcedDir);
        }
        if (p >= 0 && paint !== undefined) {
            flags |= 1 << 8;
            p = tryWritePaint(p, paint);
        }
        if (p >= 0) {
            cmdU32[savedPos] = OP_CREATE_BOX;
            cmdU32[savedPos + 1] = id;
            cmdU32[savedPos + 2] = parent === undefined ? 0xffffffff : parent;
            cmdU32[savedPos + 3] = before === undefined ? 0xffffffff : before;
            cmdU32[savedPos + 4] = flags;
            cmdPos = p;
            return;
        }
        cmdPos = savedPos;
        cmdStrs.length = savedStrs;
        if (p === -2 && savedPos > 0) { flushCmd(); continue; }
        break;
    }
    var styleOut = forcedDir === 1 ? { ...style, direction: "row" as Axis } : forcedDir === 2 ? { ...style, direction: "column" as Axis } : style;
    sendJsonOp({
        op: "create", id: id, parent: parent, before: before,
        node: { kind: "box", style: styleOut, paint: paint, press: press, disabled: disabled, hover: hover, contextmenu: contextmenu, dismiss: dismiss, scroll: scroll, drag: drag },
    });
}
function emitCreateText(
    id: number,
    parent: number | undefined,
    before: number | undefined,
    text: string,
    style: Style | undefined,
    font: Font | undefined,
    paint: Paint | undefined,
    lines: number | undefined,
    align: TextAlign | undefined,
    spans: Span[] | undefined,
): void {
    var alignCode = align === undefined || align === "start" ? 0 : align === "center" ? 1 : align === "end" ? 2 : -1;
    if (alignCode >= 0 && (lines === undefined || isU16(lines))) {
        for (var attempt = 0; attempt < 2; attempt++) {
            if (cmdPos + 24 > cmdCap) flushCmd();
            var savedPos = cmdPos;
            var savedStrs = cmdStrs.length;
            var textIdx = pushStr(text);
            var p = savedPos + 6;
            var flags = alignCode;
            if (lines !== undefined) { flags |= 1 << 2; cmdU32[p++] = lines; }
            if (font !== undefined) { flags |= 1 << 3; p = tryWriteFont(p, font); }
            if (p >= 0 && style !== undefined) { flags |= 1 << 4; p = tryWriteStyle(p, style, 0); }
            if (p >= 0 && paint !== undefined) { flags |= 1 << 5; p = tryWritePaint(p, paint); }
            if (p >= 0 && spans !== undefined) { flags |= 1 << 6; p = tryWriteSpans(p, spans); }
            if (p >= 0) {
                cmdU32[savedPos] = OP_CREATE_TEXT;
                cmdU32[savedPos + 1] = id;
                cmdU32[savedPos + 2] = parent === undefined ? 0xffffffff : parent;
                cmdU32[savedPos + 3] = before === undefined ? 0xffffffff : before;
                cmdU32[savedPos + 4] = textIdx;
                cmdU32[savedPos + 5] = flags;
                cmdPos = p;
                return;
            }
            cmdPos = savedPos;
            cmdStrs.length = savedStrs;
            if (p === -2 && savedPos > 0) { flushCmd(); continue; }
            break;
        }
    }
    sendJsonOp({
        op: "create", id: id, parent: parent, before: before,
        node: { kind: "text", text: text, style: style, font: font, paint: paint, lines: lines, align: align, spans: spans },
    });
}
function emitCreateIcon(
    id: number,
    parent: number | undefined,
    before: number | undefined,
    icon: IconName,
    size: number,
    stroke: number,
    style: Style | undefined,
    paint: Paint | undefined,
): void {
    var code = ICON_CODES[icon];
    if (code !== undefined) {
        for (var attempt = 0; attempt < 2; attempt++) {
            if (cmdPos + 24 > cmdCap) flushCmd();
            var savedPos = cmdPos;
            var savedStrs = cmdStrs.length;
            var p = savedPos + 8;
            var flags = 0;
            if (style !== undefined) { flags |= 1; p = tryWriteStyle(p, style, 0); }
            if (p >= 0 && paint !== undefined) { flags |= 2; p = tryWritePaint(p, paint); }
            if (p >= 0) {
                cmdU32[savedPos] = OP_CREATE_ICON;
                cmdU32[savedPos + 1] = id;
                cmdU32[savedPos + 2] = parent === undefined ? 0xffffffff : parent;
                cmdU32[savedPos + 3] = before === undefined ? 0xffffffff : before;
                cmdU32[savedPos + 4] = code;
                cmdF32[savedPos + 5] = size;
                cmdF32[savedPos + 6] = stroke;
                cmdU32[savedPos + 7] = flags;
                cmdPos = p;
                return;
            }
            cmdPos = savedPos;
            cmdStrs.length = savedStrs;
            if (p === -2 && savedPos > 0) { flushCmd(); continue; }
            break;
        }
    }
    sendJsonOp({
        op: "create", id: id, parent: parent, before: before,
        node: { kind: "icon", icon: icon, size: size, stroke: stroke, style: style, paint: paint },
    });
}
function emitCreateImage(
    id: number,
    parent: number | undefined,
    before: number | undefined,
    source: string,
    style: Style | undefined,
    paint: Paint | undefined,
): void {
    for (var attempt = 0; attempt < 2; attempt++) {
        if (cmdPos + 24 > cmdCap) flushCmd();
        var savedPos = cmdPos;
        var savedStrs = cmdStrs.length;
        var srcIdx = pushStr(source);
        var p = savedPos + 6;
        var flags = 0;
        if (style !== undefined) { flags |= 1; p = tryWriteStyle(p, style, 0); }
        if (p >= 0 && paint !== undefined) { flags |= 2; p = tryWritePaint(p, paint); }
        if (p >= 0) {
            cmdU32[savedPos] = OP_CREATE_IMAGE;
            cmdU32[savedPos + 1] = id;
            cmdU32[savedPos + 2] = parent === undefined ? 0xffffffff : parent;
            cmdU32[savedPos + 3] = before === undefined ? 0xffffffff : before;
            cmdU32[savedPos + 4] = srcIdx;
            cmdU32[savedPos + 5] = flags;
            cmdPos = p;
            return;
        }
        cmdPos = savedPos;
        cmdStrs.length = savedStrs;
        if (p === -2 && savedPos > 0) { flushCmd(); continue; }
        break;
    }
    sendJsonOp({
        op: "create", id: id, parent: parent, before: before,
        node: { kind: "image", source: source, style: style, paint: paint },
    });
}
function emitCreateLabel(id: number, parent: number | undefined, before: number | undefined, value: number): void {
    if (!isU16(value)) {
        sendJsonOp({ op: "create", id: id, parent: parent, before: before, node: { kind: "label", value: value } });
        return;
    }
    reserveCmd(5);
    var p = cmdPos;
    cmdU32[p] = OP_CREATE_LABEL;
    cmdU32[p + 1] = id;
    cmdU32[p + 2] = parent === undefined ? 0xffffffff : parent;
    cmdU32[p + 3] = before === undefined ? 0xffffffff : before;
    cmdU32[p + 4] = value;
    cmdPos = p + 5;
}
function emitCreateButton(id: number, parent: number | undefined, before: number | undefined, callback: number): void {
    if (!isU16(callback)) {
        sendJsonOp({ op: "create", id: id, parent: parent, before: before, node: { kind: "button", callback: callback } });
        return;
    }
    reserveCmd(5);
    var p = cmdPos;
    cmdU32[p] = OP_CREATE_BUTTON;
    cmdU32[p + 1] = id;
    cmdU32[p + 2] = parent === undefined ? 0xffffffff : parent;
    cmdU32[p + 3] = before === undefined ? 0xffffffff : before;
    cmdU32[p + 4] = callback;
    cmdPos = p + 5;
}
function emitCreateList(
    id: number,
    parent: number | undefined,
    before: number | undefined,
    scroll: number,
    style: Style | undefined,
): void {
    if (isU16(scroll)) {
        for (var attempt = 0; attempt < 2; attempt++) {
            if (cmdPos + 16 > cmdCap) flushCmd();
            var savedPos = cmdPos;
            var savedStrs = cmdStrs.length;
            var p = savedPos + 5;
            var flags = 0;
            if (scroll) { flags |= 1; cmdU32[p++] = scroll; }
            if (style !== undefined) { flags |= 2; p = tryWriteStyle(p, style, 0); }
            if (p >= 0) {
                cmdU32[savedPos] = OP_CREATE_LIST;
                cmdU32[savedPos + 1] = id;
                cmdU32[savedPos + 2] = parent === undefined ? 0xffffffff : parent;
                cmdU32[savedPos + 3] = before === undefined ? 0xffffffff : before;
                cmdU32[savedPos + 4] = flags;
                cmdPos = p;
                return;
            }
            cmdPos = savedPos;
            cmdStrs.length = savedStrs;
            if (p === -2 && savedPos > 0) { flushCmd(); continue; }
            break;
        }
    }
    sendJsonOp({ op: "create", id: id, parent: parent, before: before, node: { kind: "list", style: style, scroll: scroll } });
}
function emitCreateInput(
    id: number,
    parent: number | undefined,
    before: number | undefined,
    text: string | undefined,
    selection: [number, number] | undefined,
    submit: number,
    change: number,
    placeholder: string | undefined,
    placeholderColor: number | undefined,
    multiline: boolean | undefined,
    submitOnEnter: boolean | undefined,
    retainOnSubmit: boolean | undefined,
    purpose: InputPurpose | undefined,
    style: Style | undefined,
    font: Font | undefined,
    paint: Paint | undefined,
): void {
    var purposeCode = purpose === undefined ? 0 : PURPOSE_CODES[purpose];
    if (purposeCode !== undefined) {
        for (var attempt = 0; attempt < 2; attempt++) {
            if (cmdPos + 24 > cmdCap) flushCmd();
            var savedPos = cmdPos;
            var savedStrs = cmdStrs.length;
            var p = savedPos + 5;
            var flags = (multiline ? 1 : 0)
                | (submitOnEnter ? (1 << 1) : 0)
                | (retainOnSubmit ? (1 << 2) : 0)
                | (purposeCode << 3);
            if (text !== undefined && text !== "") { flags |= 1 << 6; cmdU32[p++] = pushStr(text); }
            if (selection !== undefined) {
                flags |= 1 << 7;
                cmdU32[p++] = selection[0] >>> 0;
                cmdU32[p++] = selection[1] >>> 0;
            }
            if (submit) { flags |= 1 << 8; cmdU32[p++] = submit; }
            if (change) { flags |= 1 << 9; cmdU32[p++] = change; }
            if (placeholder !== undefined && placeholder !== "") { flags |= 1 << 10; cmdU32[p++] = pushStr(placeholder); }
            if (placeholderColor !== undefined) { flags |= 1 << 11; cmdU32[p++] = placeholderColor >>> 0; }
            if (font !== undefined) { flags |= 1 << 12; p = tryWriteFont(p, font); }
            if (p >= 0 && style !== undefined) { flags |= 1 << 13; p = tryWriteStyle(p, style, 0); }
            if (p >= 0 && paint !== undefined) { flags |= 1 << 14; p = tryWritePaint(p, paint); }
            if (p >= 0) {
                cmdU32[savedPos] = OP_CREATE_INPUT;
                cmdU32[savedPos + 1] = id;
                cmdU32[savedPos + 2] = parent === undefined ? 0xffffffff : parent;
                cmdU32[savedPos + 3] = before === undefined ? 0xffffffff : before;
                cmdU32[savedPos + 4] = flags;
                cmdPos = p;
                return;
            }
            cmdPos = savedPos;
            cmdStrs.length = savedStrs;
            if (p === -2 && savedPos > 0) { flushCmd(); continue; }
            break;
        }
    }
    sendJsonOp({
        op: "create", id: id, parent: parent, before: before,
        node: {
            kind: "input",
            text: text,
            selection: selection,
            submit: submit,
            change: change,
            placeholder: placeholder,
            placeholderColor: placeholderColor,
            multiline: multiline,
            submitOnEnter: submitOnEnter,
            retainOnSubmit: retainOnSubmit,
            purpose: purpose,
            style: style,
            font: font,
            paint: paint,
        },
    });
}
function mountChildren(children: Children | undefined, parent: number, created: number[] | null): void {
    if (Array.isArray(children)) for (var i = 0; i < children.length; i++) mountChildren(children[i], parent, created);
    else if (children) children.mount(parent, undefined, created);
}
var EMPTY_PROPS: any = {};
// JSX factory (`jsxFactory`). Uses `arguments` so leaf calls allocate no rest array in QuickJS,
// and fast-paths built-in Box/Row/Column/Text elements without `{ ...props, children }` object cloning.
function h(component: (props: any) => Element, props: object | null, ...children: unknown[]): Element;
function h(component: (props: any) => Element, props: object | null): Element {
    var n = arguments.length;
    var p: any = props === null ? EMPTY_PROPS : props;
    if (n <= 2) {
        if (component === Box) return new BoxElement(p, 0, p.children);
        if (component === Row) return new BoxElement(p, 1, p.children);
        if (component === Column) return new BoxElement(p, 2, p.children);
        if (component === Text) return new TextElement(p, p.children);
        return component(p);
    }
    var nested: any;
    if (n === 3) {
        nested = arguments[2];
    } else {
        var arr = new Array(n - 2);
        for (var i = 2; i < n; i++) arr[i - 2] = arguments[i];
        nested = arr;
    }
    if (component === Box) return new BoxElement(p, 0, nested);
    if (component === Row) return new BoxElement(p, 1, nested);
    if (component === Column) return new BoxElement(p, 2, nested);
    if (component === Text) return new TextElement(p, nested);
    return component(props === null ? { children: nested } : { ...props, children: nested });
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
    (owner.children ??= []).push(scope);
    var created: number[] | null = transient ? null : [];
    var prevScope = currentScope;
    currentScope = scope;
    var id: number;
    try {
        id = element.mount(parent, before, created);
    } finally {
        currentScope = prevScope;
    }
    if (id === -1) throw new Error("mountInto: the element created no node");
    if (created !== null) scope.createdIds = created;
    maybeFlushCmd();
    return {
        id: id,
        remove() {
            // Disposed with an ancestor: its nodes went with that ancestor's remove, and its ids may already belong
            // to new nodes, so a late remove (a stale reference) must send nothing.
            if (scope.disposed) {
                if (transient && owner.children !== undefined) owner.children.splice(owner.children.indexOf(scope), 1);
                return;
            }
            removing++;
            try {
                dispose(scope);
            } finally {
                // Even when a cleanup throws, the subtree leaves the screen.
                if (transient && owner.children !== undefined) owner.children.splice(owner.children.indexOf(scope), 1);
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
    emitSetSpans(id, spans);
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

class BoxElement implements Element {
    props: BoxProps;
    forcedDir: number;
    children: Children | undefined;
    constructor(props: BoxProps, forcedDir: number, children: Children | undefined) {
        this.props = props;
        this.forcedDir = forcedDir;
        this.children = children;
    }
    mount(parent: number | undefined, before: number | undefined, created: number[] | null): number {
        var props = this.props;
        var forcedDir = this.forcedDir;
        var id = newId();
        var styleProp = props.style;
        var style = styleProp === undefined ? undefined : bindId(
            styleProp,
            id,
            forcedDir === 1 ? emitSetStyleRow : forcedDir === 2 ? emitSetStyleCol : emitSetStyle,
        );
        var paintProp = props.paint;
        var paint = paintProp === undefined ? undefined : bindId(paintProp, id, emitSetPaint);
        var disabledProp = props.disabled;
        var disabled = disabledProp === undefined ? false : bindId(disabledProp, id, emitSetDisabled);
        var press = props.onPress !== undefined ? register(props.onPress) : 0;
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
        emitCreateBox(id, parent, before, style, paint, press, disabled, hover, contextmenu, dismiss, scroll, drag, forcedDir);
        if (created !== null) created.push(id);
        if (props.ref !== undefined) props.ref(id);
        var kids = this.children;
        if (kids !== undefined) mountChildren(kids, id, created);
        return id;
    }
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
    return new BoxElement(props, 0, props.children);
}
type BoxProps = Parameters<typeof Box>[0];
/** A `Box` that lays out its children in a row. */
function Row(props: BoxProps): Element {
    return new BoxElement(props, 1, props.children);
}
/** A `Box` that lays out its children in a column. */
function Column(props: BoxProps): Element {
    return new BoxElement(props, 2, props.children);
}
/** Scrolling list. `children` builds each item; new items arrive with `items.push`. */
function List<T>(props: { items: ListSignal<T>; style?: Style; children: (item: T) => Element }): Element {
    return {
        mount(parent, before, created) {
            var id = newId();
            emitCreateList(id, parent, before, 0, props.style);
            if (created !== null) created.push(id);
            var items = props.items.items;
            for (var i = 0; i < items.length; i++) props.children(items[i]).mount(id, undefined, created);
            (listeners.get(props.items) as Array<(item: T) => void>).push(function (item) {
                props.children(item).mount(id, undefined, null);
                maybeFlushCmd();
            });
            return id;
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
        mount(parent, before, created) {
            var id = newId();
            var scroll = props.onScroll === undefined ? 0 : registerScroll(props.onScroll);
            var mount: string | undefined;
            var rows = new Map<string, KeyedRow<T>>();
            emitCreateList(id, parent, before, scroll, props.style);
            emitVirtualList(id, props.name);
            if (created !== null) created.push(id);
            props.ref?.(id);
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
                        var payload = new LocalSignal<T>(value as T);
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
            return id;
        },
    };
}

function jsnirVirtualList(json: string): string {
    var delta = JSON.parse(json) as VirtualPayload;
    const deliver = virtualMounts.get(delta.node);
    if (deliver === undefined) throw new Error("unknown virtual List mount");
    return JSON.stringify(runEvent(() => deliver(delta)));
}

type TextProps = {
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
};
class TextElement implements Element {
    props: TextProps;
    content: Source<string>;
    constructor(props: TextProps, content: Source<string>) {
        this.props = props;
        this.content = content;
    }
    mount(parent: number | undefined, before: number | undefined, created: number[] | null): number {
        var props = this.props;
        var id = newId();
        var text = bindId(this.content, id, emitSetText);
        var spansProp = props.spans;
        var spans = spansProp === undefined ? undefined : bindId(spansProp, id, emitSetSpans);
        var paintProp = props.paint;
        var paint = paintProp === undefined ? undefined : bindId(paintProp, id, emitSetPaint);
        emitCreateText(id, parent, before, text, props.style, props.font, paint, props.lines, props.align, spans);
        if (created !== null) created.push(id);
        return id;
    }
}
function Text(props: TextProps): Element {
    return new TextElement(props, props.children);
}
type IconProps = { name: IconName; size: number; stroke: number; style?: Style; paint?: Source<Paint> };
class IconElement implements Element {
    props: IconProps;
    constructor(props: IconProps) { this.props = props; }
    mount(parent: number | undefined, before: number | undefined, created: number[] | null): number {
        var props = this.props;
        var id = newId();
        var paint = props.paint === undefined ? undefined : bindId(props.paint, id, emitSetPaint);
        emitCreateIcon(id, parent, before, props.name, props.size, props.stroke, props.style, paint);
        if (created !== null) created.push(id);
        return id;
    }
}
/** A built-in stroke icon, `size` logical pixels square. */
function Icon(props: IconProps): Element {
    return new IconElement(props);
}
type ImageProps = { source: Source<string>; style?: Style; paint?: Source<Paint> };
class ImageElement implements Element {
    props: ImageProps;
    constructor(props: ImageProps) { this.props = props; }
    mount(parent: number | undefined, before: number | undefined, created: number[] | null): number {
        var props = this.props;
        var id = newId();
        var source = bindId(props.source, id, emitSetSource);
        var paint = props.paint === undefined ? undefined : bindId(props.paint, id, emitSetPaint);
        emitCreateImage(id, parent, before, source, props.style, paint);
        if (created !== null) created.push(id);
        return id;
    }
}
/** An image the embedding program supplied by `source` (`Host::set_image`), stretched to its box. */
function Image(props: ImageProps): Element {
    return new ImageElement(props);
}
class LabelElement implements Element {
    value: Source<number>;
    constructor(value: Source<number>) { this.value = value; }
    mount(parent: number | undefined, before: number | undefined, created: number[] | null): number {
        var id = newId();
        var value = bindId(this.value, id, emitSet);
        emitCreateLabel(id, parent, before, value);
        if (created !== null) created.push(id);
        return id;
    }
}
/** Three-digit numeric label (0 to 999). */
function Label(props: { value: Source<number> }): Element {
    return new LabelElement(props.value);
}
class ButtonElement implements Element {
    onClick: () => void;
    constructor(onClick: () => void) { this.onClick = onClick; }
    mount(parent: number | undefined, before: number | undefined, created: number[] | null): number {
        var id = newId();
        var fn = this.onClick;
        var callback = register(function () { fn(); });
        emitCreateButton(id, parent, before, callback);
        if (created !== null) created.push(id);
        return id;
    }
}
function Button(props: { onClick: () => void }): Element {
    return new ButtonElement(props.onClick);
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
type TextInputProps = {
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
    /** Moves keyboard focus to this input when it mounts. */
    autoFocus?: boolean;
    ref?: (field: Field) => void;
};
class TextInputElement implements Element {
    props: TextInputProps;
    constructor(props: TextInputProps) { this.props = props; }
    mount(parent: number | undefined, before: number | undefined, created: number[] | null): number {
        var props = this.props;
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
                emitSetField(id, text, expected ?? (jsnir.readText(id) ?? ""));
            },
        };
        var onSubmit = props.onSubmit;
        var submit = onSubmit === undefined ? 0 : register(function (text) {
            (onSubmit as (submit: TextSubmit) => void)({ text: text, field: field });
        });
        var change = props.onChange !== undefined ? register(props.onChange) : 0;
        var paint = props.paint === undefined ? undefined : bindId(props.paint, id, emitSetPaint);
        var selection: [number, number] | undefined =
            props.value === undefined ? undefined : [props.value.length, props.value.length];
        emitCreateInput(
            id,
            parent,
            before,
            props.value,
            selection,
            submit,
            change,
            props.placeholder,
            props.placeholderColor,
            props.multiline,
            props.submitOnEnter,
            props.retainOnSubmit,
            props.purpose,
            props.style,
            props.font,
            paint,
        );
        if (created !== null) created.push(id);
        if (props.autoFocus) emitFocus(id);
        if (props.ref !== undefined) props.ref(field);
        return id;
    }
}
function TextInput(props: TextInputProps): Element {
    return new TextInputElement(props);
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
var mountLaterHead = 0;
function mountPending(): void {
    try {
        while (mountLaterHead < mountLater.length) {
            var fn = mountLater[mountLaterHead++];
            fn();
        }
    } finally {
        if (mountLaterHead >= mountLater.length) {
            mountLater.length = 0;
            mountLaterHead = 0;
        }
    }
}

/** One child of a `For`: its item signal, root node id, owned scope, and created ids. */
interface ForRow<T> {
    signal: Signal<T>;
    id: number;
    scope: Scope;
}
/** One child of a `VirtualList`: its item signal, mounted subtree and scope. */
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
            var rows = new Map<string | number, ForRow<T>>();
            // Node ids of the children, in the order the tree has them.
            var order: number[] = [];
            var mountRow = function (item: T, before: number | undefined): ForRow<T> {
                var s = new LocalSignal<T>(item);
                var rowScope = childScope();
                var transient = rowScope.ids !== undefined;
                (scope.children ??= []).push(rowScope);
                var created: number[] | null = transient ? null : [];
                var prevScope = currentScope;
                currentScope = rowScope;
                var rowId: number;
                try {
                    rowId = children(function () {
                        return s.value;
                    }).mount(id, before, created);
                } finally {
                    currentScope = prevScope;
                }
                if (rowId === -1) throw new Error("mountInto: the element created no node");
                if (created !== null) rowScope.createdIds = created;
                return { signal: s, id: rowId, scope: rowScope };
            };
            var sync = function (items: T[]) {
                inScope(scope, function () {
                    var len = items.length;
                    var keys = new Array<string | number>(len);
                    var fresh = new Set<string | number>();
                    for (var ki = 0; ki < len; ki++) {
                        var k = key(items[ki]);
                        keys[ki] = k;
                        fresh.add(k);
                    }
                    if (fresh.size !== len) throw new Error("For: duplicate key");
                    // Remove departed rows from state in O(N) before running any cleanup callbacks,
                    // so re-entrant syncs see consistent state and removals never pay O(N^2) splice cost.
                    var toRemove: ForRow<T>[] = [];
                    var rmIds: Set<number> | null = null;
                    var rmScopes: Set<Scope> | null = null;
                    rows.forEach(function (entry, rowKey) {
                        if (fresh.has(rowKey)) return;
                        rows.delete(rowKey);
                        if (rmIds === null) {
                            rmIds = new Set();
                            rmScopes = new Set();
                        }
                        toRemove.push(entry);
                        rmIds.add(entry.id);
                        (rmScopes as Set<Scope>).add(entry.scope);
                    });
                    if (toRemove.length > 0) {
                        var deadIds = rmIds as unknown as Set<number>;
                        var deadScopes = rmScopes as unknown as Set<Scope>;
                        if (rows.size === 0) {
                            order.length = 0;
                        } else {
                            var ow = 0;
                            for (var oi = 0; oi < order.length; oi++) {
                                if (!deadIds.has(order[oi])) order[ow++] = order[oi];
                            }
                            order.length = ow;
                        }
                        var kids = scope.children;
                        if (kids !== undefined) {
                            var sw = 0;
                            for (var si = 0; si < kids.length; si++) {
                                if (!deadScopes.has(kids[si])) kids[sw++] = kids[si];
                            }
                            kids.length = sw;
                        }
                        removing++;
                        try {
                            for (var ri = 0; ri < toRemove.length; ri++) {
                                var dead = toRemove[ri];
                                if (!dead.scope.disposed) {
                                    dispose(dead.scope);
                                    emitRemove(dead.id);
                                }
                            }
                        } finally {
                            removing--;
                            if (removing === 0) while (retiring.length > 0) freeIds.push(retiring.pop() as number);
                        }
                    }
                    if (order.length === 0) {
                        // Fast O(N) forward mount for initial renders and full list swaps.
                        for (var fi = 0; fi < len; fi++) {
                            var added = mountRow(items[fi], undefined);
                            rows.set(keys[fi], added);
                            order.push(added.id);
                        }
                        maybeFlushCmd();
                        return;
                    }
                    // From the last item back, each child goes before the one placed after it: a kept child
                    // moves only when it is not already immediately before the next surviving old child.
                    var newOrder = new Array<number>(len);
                    var before: number | undefined = undefined;
                    var nextOld: number | undefined = undefined;
                    for (var i = len - 1; i >= 0; i--) {
                        var existing = rows.get(keys[i]);
                        if (existing === undefined) {
                            existing = mountRow(items[i], before);
                            rows.set(keys[i], existing);
                        } else {
                            existing.signal.value = items[i];
                            var rowId = existing.id;
                            var pos = order.indexOf(rowId);
                            if (order[pos + 1] !== nextOld) {
                                emitMove(rowId, before);
                                order.splice(pos, 1);
                                order.splice(placeAt(order, nextOld), 0, rowId);
                            }
                            nextOld = rowId;
                        }
                        newOrder[i] = existing.id;
                        before = existing.id;
                    }
                    order = newOrder;
                    maybeFlushCmd();
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
                        mount(parent, before, created) {
                            const onPlacement = props.onPlacement;
                            if (onPlacement !== undefined) {
                                var token = (typeof __jsnirPlacementEpoch === "string" ? __jsnirPlacementEpoch : "0")
                                    + ":" + nextPlacementToken++;
                                var callback = register(function (text) { onPlacement(JSON.parse(text)); });
                                placementTokens[callback] = token;
                                anchor.placement = { callback: callback, token: token };
                            }
                            return Box({
                                style: { ...props.style, position: "fixed", layer: props.layer ?? 1, anchor: anchor },
                                paint: props.paint,
                                onDismiss: function () {
                                    props.open.value = false;
                                },
                                children: props.children,
                            }).mount(parent, before, created);
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
            return -1;
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
        root().mount(undefined, undefined, null);
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
