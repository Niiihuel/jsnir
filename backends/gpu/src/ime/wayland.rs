//! Native Linux Wayland `zwp_text_input_v3` (v1 & v2) IME backend.
//!
//! Architecture:
//! - [`WaylandImeProtocol`] is a pure, deterministic state machine implementing the full
//!   `zwp_text_input_v3` double-buffered protocol (`enter`/`leave`, `enable`/`disable`,
//!   `set_surrounding_text`, `set_text_change_cause`, `set_content_type`, `set_cursor_rectangle`,
//!   `delete_surrounding_text`, `commit_string`, `preedit_string`, `done(serial)`, and v2
//!   `action::submit`). Because it is pure Rust, it is unit-tested on every platform.
//! - [`WaylandTextInput`] (Linux only) attaches to `winit`'s `wl_display` via
//!   `wayland_backend::sys::client::Backend::from_foreign_display` with its own isolated
//!   `EventQueue`, so it never steals or duplicates events from `winit` (whose own
//!   `zwp_text_input_v3` stays permanently disabled while this backend is active).

use jsnir::{
    editor::{EditorEvent, SurroundingText},
    host::{ImeChangeCause, ImeState},
    tree::InputPurpose,
};

/// `zwp_text_input_v3::content_hint` bitflags (matching the official Wayland XML specification).
pub mod hint {
    pub const NONE: u32 = 0x0;
    pub const COMPLETION: u32 = 0x1;
    pub const SPELLCHECK: u32 = 0x2;
    pub const LOWERCASE: u32 = 0x8;
    pub const HIDDEN_TEXT: u32 = 0x40;
    pub const SENSITIVE_DATA: u32 = 0x80;
    pub const LATIN: u32 = 0x100;
    pub const MULTILINE: u32 = 0x200;
    /// Version 2 hint: client renders preedit text inline.
    pub const PREEDIT_SHOWN: u32 = 0x1000;
}

/// `zwp_text_input_v3::content_purpose` enum values (matching the official Wayland XML specification).
pub mod purpose {
    pub const NORMAL: u32 = 0;
    pub const NUMBER: u32 = 3;
    pub const EMAIL: u32 = 6;
    pub const PASSWORD: u32 = 8;
}

/// Outgoing `zwp_text_input_v3` requests produced by [`WaylandImeProtocol`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaylandRequest {
    Enable,
    Disable,
    SetContentType { hint_bits: u32, purpose_code: u32 },
    SetSurroundingText { text: String, cursor: i32, anchor: i32 },
    SetTextChangeCause { input_method: bool },
    SetCursorRectangle { x: i32, y: i32, width: i32, height: i32 },
    /// Version 2 request (`set_available_actions`).
    SetAvailableActions { submit: bool },
    Commit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingPreedit {
    text: String,
    cursor_begin: i32,
    cursor_end: i32,
}

#[derive(Debug, Default)]
struct PendingIncoming {
    preedit: Option<PendingPreedit>,
    commit: Option<String>,
    delete_surrounding: Option<(u32, u32)>,
    submit_action: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ContentSpec {
    hint_bits: u32,
    purpose_code: u32,
    submit_action: bool,
}

/// Maps JSNIR's semantic input metadata into `zwp_text_input_v3` content hint + purpose bits.
pub fn wayland_content_type(
    input_purpose: InputPurpose,
    multiline: bool,
    protocol_version: u32,
) -> (u32, u32) {
    let preedit_flag = if protocol_version >= 2 {
        hint::PREEDIT_SHOWN
    } else {
        hint::NONE
    };
    let ml_flag = if multiline {
        hint::MULTILINE
    } else {
        hint::NONE
    };
    match input_purpose {
        InputPurpose::Normal => (
            hint::COMPLETION | hint::SPELLCHECK | ml_flag | preedit_flag,
            purpose::NORMAL,
        ),
        InputPurpose::Search => (
            hint::COMPLETION | hint::SPELLCHECK | preedit_flag,
            purpose::NORMAL,
        ),
        InputPurpose::Email => (hint::LOWERCASE | preedit_flag, purpose::EMAIL),
        InputPurpose::Number => (hint::LATIN | preedit_flag, purpose::NUMBER),
        InputPurpose::Password => (hint::SENSITIVE_DATA | hint::HIDDEN_TEXT, purpose::PASSWORD),
    }
}

/// Converts a logical floating-point caret rect `[x, y, w, h]` into surface-local integer pixels.
fn quantize_rect(rect: [f32; 4]) -> [i32; 4] {
    [
        rect[0].round() as i32,
        rect[1].round() as i32,
        rect[2].ceil().max(1.) as i32,
        rect[3].ceil().max(1.) as i32,
    ]
}

/// Pure `zwp_text_input_v3` protocol state machine (fully unit-testable without a compositor).
pub struct WaylandImeProtocol {
    version: u32,
    entered: bool,
    enabled: bool,
    has_active_preedit: bool,
    focused_id: Option<u16>,
    last_content: Option<ContentSpec>,
    last_surrounding: Option<SurroundingText>,
    last_area: Option<[i32; 4]>,
    pending_area: Option<[i32; 4]>,
    commit_serial: u32,
    /// Latest desired host state (re-sent cleanly whenever `enter` arrives after `leave`).
    desired: Option<ImeState>,
    incoming: PendingIncoming,
}

impl WaylandImeProtocol {
    pub fn new(version: u32) -> Self {
        Self {
            version: version.max(1),
            entered: false,
            enabled: false,
            has_active_preedit: false,
            focused_id: None,
            last_content: None,
            last_surrounding: None,
            last_area: None,
            pending_area: None,
            commit_serial: 0,
            desired: None,
            incoming: PendingIncoming::default(),
        }
    }

    pub fn entered(&self) -> bool {
        self.entered
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn commit_serial(&self) -> u32 {
        self.commit_serial
    }

    /// Handles `zwp_text_input_v3.enter` for our target window surface.
    pub fn on_enter(&mut self) -> Vec<WaylandRequest> {
        self.entered = true;
        // Spec: after `enter`, all prior compositor state is invalidated and must be resent.
        self.enabled = false;
        self.last_content = None;
        self.last_surrounding = None;
        self.last_area = None;
        self.incoming = PendingIncoming::default();

        let mut out = Vec::new();
        if let Some(state) = self.desired.clone()
            && state.allowed
        {
            self.emit_enable_sequence(&state, &mut out);
        }
        out
    }

    /// Handles `zwp_text_input_v3.leave` for our target window surface.
    /// Returns outgoing cleanup requests and any editor events needed (clearing active preedit).
    pub fn on_leave(&mut self) -> (Vec<WaylandRequest>, Vec<EditorEvent>) {
        self.entered = false;
        self.incoming = PendingIncoming::default();
        self.last_content = None;
        self.last_surrounding = None;
        self.last_area = None;

        let mut reqs = Vec::new();
        if self.enabled {
            self.enabled = false;
            reqs.push(WaylandRequest::Disable);
            self.push_commit(&mut reqs);
        }

        let mut events = Vec::new();
        if self.has_active_preedit {
            self.has_active_preedit = false;
            events.push(EditorEvent::Preedit {
                text: String::new(),
                active: None,
            });
        }
        (reqs, events)
    }

    /// Buffers `zwp_text_input_v3.preedit_string` until `done`.
    pub fn on_preedit_string(&mut self, text: Option<String>, cursor_begin: i32, cursor_end: i32) {
        self.incoming.preedit = Some(PendingPreedit {
            text: text.unwrap_or_default(),
            cursor_begin,
            cursor_end,
        });
    }

    /// Buffers `zwp_text_input_v3.commit_string` until `done`.
    pub fn on_commit_string(&mut self, text: Option<String>) {
        self.incoming.commit = text;
    }

    /// Buffers `zwp_text_input_v3.delete_surrounding_text` until `done`.
    pub fn on_delete_surrounding_text(&mut self, before_length: u32, after_length: u32) {
        if before_length > 0 || after_length > 0 {
            self.incoming.delete_surrounding = Some((before_length, after_length));
        }
    }

    /// Buffers `zwp_text_input_v3.action(submit)` (version 2) until `done`.
    pub fn on_action_submit(&mut self) {
        self.incoming.submit_action = true;
    }

    /// Applies buffered incoming events atomically in the exact 7-step order required by
    /// `zwp_text_input_v3.done(serial)`.
    pub fn on_done(&mut self, _serial: u32) -> Vec<EditorEvent> {
        let pending = std::mem::take(&mut self.incoming);
        if !self.entered || !self.enabled {
            return Vec::new();
        }

        let mut events = Vec::with_capacity(4);
        let has_delete = pending.delete_surrounding.is_some();
        let has_commit = pending.commit.is_some();
        let next_preedit_nonempty = pending
            .preedit
            .as_ref()
            .is_some_and(|p| !p.text.is_empty());

        // Step 1: Replace existing preedit with the cursor before deleting/committing or when cleared.
        if self.has_active_preedit && (has_delete || has_commit || !next_preedit_nonempty) {
            events.push(EditorEvent::Preedit {
                text: String::new(),
                active: None,
            });
            self.has_active_preedit = false;
        }

        // Step 2: Delete requested surrounding UTF-8 bytes around the cursor/selection.
        if let Some((before_length, after_length)) = pending.delete_surrounding {
            events.push(EditorEvent::DeleteSurroundingBytes {
                before_bytes: before_length as usize,
                after_bytes: after_length as usize,
            });
        }

        // Step 3: Insert committed text with the cursor at its end.
        if let Some(text) = pending.commit
            && !text.is_empty()
        {
            events.push(EditorEvent::Commit(text));
        }

        // Steps 5 & 6: Insert new preedit text and place cursor / highlight inside it.
        if let Some(preedit) = pending.preedit
            && !preedit.text.is_empty()
        {
            let active = decode_wayland_preedit_cursor(
                &preedit.text,
                preedit.cursor_begin,
                preedit.cursor_end,
            );
            events.push(EditorEvent::Preedit {
                text: preedit.text,
                active,
            });
            self.has_active_preedit = true;
        }

        // Step 7: Perform requested v2 action (submit).
        if pending.submit_action {
            events.push(EditorEvent::Enter);
        }

        events
    }

    /// Synchronizes host `ImeState` (focus, purpose, multiline, surrounding text, change cause)
    /// into deduplicated `zwp_text_input_v3` requests.
    pub fn plan_sync(&mut self, window_focused: bool, state: &ImeState) -> Vec<WaylandRequest> {
        let mut effective = state.clone();
        if !window_focused {
            effective.allowed = false;
            effective.surrounding = None;
        }
        self.desired = Some(effective.clone());

        let mut out = Vec::new();
        if !self.entered {
            self.focused_id = effective.focused_id;
            return out;
        }

        if !effective.allowed {
            self.focused_id = effective.focused_id;
            if self.enabled {
                self.enabled = false;
                self.has_active_preedit = false;
                self.last_content = None;
                self.last_surrounding = None;
                self.last_area = None;
                out.push(WaylandRequest::Disable);
                self.push_commit(&mut out);
            }
            return out;
        }

        // Spec: `enable` MUST be issued again whenever focus moves to a different text input!
        let focus_switched = self.enabled && self.focused_id != effective.focused_id;
        self.focused_id = effective.focused_id;

        if !self.enabled || focus_switched {
            if focus_switched {
                out.push(WaylandRequest::Disable);
                self.push_commit(&mut out);
                self.has_active_preedit = false;
            }
            self.emit_enable_sequence(&effective, &mut out);
            return out;
        }

        // Incremental update within the same focused field: send ONLY what changed.
        let mut dirty = false;

        let content = self.content_spec(&effective);
        if self.last_content != Some(content) {
            out.push(WaylandRequest::SetContentType {
                hint_bits: content.hint_bits,
                purpose_code: content.purpose_code,
            });
            if self.version >= 2 {
                out.push(WaylandRequest::SetAvailableActions {
                    submit: content.submit_action,
                });
            }
            self.last_content = Some(content);
            dirty = true;
        }

        if let Some(ref surr) = effective.surrounding
            && self.last_surrounding.as_ref() != Some(surr)
        {
            out.push(WaylandRequest::SetSurroundingText {
                text: surr.text.clone(),
                cursor: surr.cursor_byte as i32,
                anchor: surr.anchor_byte as i32,
            });
            out.push(WaylandRequest::SetTextChangeCause {
                input_method: effective.change_cause == ImeChangeCause::InputMethod,
            });
            self.last_surrounding = Some(surr.clone());
            dirty = true;
        }

        if let Some(area) = self.pending_area
            && self.last_area != Some(area)
        {
            out.push(WaylandRequest::SetCursorRectangle {
                x: area[0],
                y: area[1],
                width: area[2],
                height: area[3],
            });
            self.last_area = Some(area);
            dirty = true;
        }

        if dirty {
            self.push_commit(&mut out);
        }
        out
    }

    /// Updates the logical caret/preedit cursor rectangle, emitting a commit only if it changed.
    pub fn plan_area(&mut self, area: Option<[f32; 4]>) -> Vec<WaylandRequest> {
        let q = area.map(quantize_rect);
        self.pending_area = q;
        let mut out = Vec::new();
        if self.entered
            && self.enabled
            && let Some(rect) = q
            && self.last_area != Some(rect)
        {
            out.push(WaylandRequest::SetCursorRectangle {
                x: rect[0],
                y: rect[1],
                width: rect[2],
                height: rect[3],
            });
            self.last_area = Some(rect);
            self.push_commit(&mut out);
        }
        out
    }

    /// Interrupts active composition (e.g. mouse click inside text, Escape, or JS reset) by
    /// issuing `disable` + `commit` and immediately re-enabling with fresh surrounding text.
    pub fn plan_interrupt(&mut self) -> Vec<WaylandRequest> {
        let mut out = Vec::new();
        self.has_active_preedit = false;
        if !self.entered || !self.enabled {
            return out;
        }
        out.push(WaylandRequest::Disable);
        self.push_commit(&mut out);
        self.enabled = false;
        self.last_content = None;
        self.last_surrounding = None;
        self.last_area = None;

        if let Some(state) = self.desired.clone()
            && state.allowed
        {
            self.emit_enable_sequence(&state, &mut out);
        }
        out
    }

    fn content_spec(&self, state: &ImeState) -> ContentSpec {
        let (hint_bits, purpose_code) =
            wayland_content_type(state.purpose, state.multiline, self.version);
        ContentSpec {
            hint_bits,
            purpose_code,
            submit_action: state.submit_on_enter,
        }
    }

    fn emit_enable_sequence(&mut self, state: &ImeState, out: &mut Vec<WaylandRequest>) {
        out.push(WaylandRequest::Enable);
        let content = self.content_spec(state);
        out.push(WaylandRequest::SetContentType {
            hint_bits: content.hint_bits,
            purpose_code: content.purpose_code,
        });
        if self.version >= 2 {
            out.push(WaylandRequest::SetAvailableActions {
                submit: content.submit_action,
            });
        }
        if let Some(ref surr) = state.surrounding {
            out.push(WaylandRequest::SetSurroundingText {
                text: surr.text.clone(),
                cursor: surr.cursor_byte as i32,
                anchor: surr.anchor_byte as i32,
            });
            out.push(WaylandRequest::SetTextChangeCause {
                input_method: state.change_cause == ImeChangeCause::InputMethod,
            });
        }
        if let Some(area) = self.pending_area {
            out.push(WaylandRequest::SetCursorRectangle {
                x: area[0],
                y: area[1],
                width: area[2],
                height: area[3],
            });
            self.last_area = Some(area);
        } else {
            self.last_area = None;
        }
        self.push_commit(out);
        self.enabled = true;
        self.focused_id = state.focused_id;
        self.last_content = Some(content);
        self.last_surrounding = state.surrounding.clone();
    }

    fn push_commit(&mut self, out: &mut Vec<WaylandRequest>) {
        self.commit_serial = self.commit_serial.wrapping_add(1);
        out.push(WaylandRequest::Commit);
    }
}

/// Converts Wayland `preedit_string` UTF-8 byte offsets into `LineEditor` character indices.
/// Returns `None` when both offsets are `-1` (hidden cursor per spec) or out of bounds.
pub fn decode_wayland_preedit_cursor(
    text: &str,
    cursor_begin: i32,
    cursor_end: i32,
) -> Option<std::ops::Range<usize>> {
    if cursor_begin < 0 && cursor_end < 0 {
        return None;
    }
    let begin = usize::try_from(cursor_begin).ok()?;
    let end = usize::try_from(cursor_end).unwrap_or(begin);
    let lo = begin.min(end);
    let hi = begin.max(end);
    let lo_char = text.get(..lo)?.chars().count();
    let hi_char = text.get(..hi)?.chars().count();
    Some(lo_char..hi_char)
}

// ---------- Linux Wayland live client implementation ----------

#[cfg(target_os = "linux")]
mod live {
    use super::*;
    use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
    use std::ffi::c_void;
    use wayland_backend::sys::client::Backend;
    use wayland_client::{
        Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
        protocol::{wl_registry, wl_seat},
    };
    use wayland_protocols::wp::text_input::zv3::client::{
        zwp_text_input_manager_v3::ZwpTextInputManagerV3,
        zwp_text_input_v3::{
            self, Action, ChangeCause, ContentHint, ContentPurpose, ZwpTextInputV3,
        },
    };

    struct DispatchState {
        target_surface: *const c_void,
        seat: Option<wl_seat::WlSeat>,
        manager: Option<ZwpTextInputManagerV3>,
        manager_version: u32,
        text_input: Option<ZwpTextInputV3>,
        protocol: WaylandImeProtocol,
        outbox: Vec<EditorEvent>,
    }

    impl Dispatch<wl_registry::WlRegistry, ()> for DispatchState {
        fn event(
            state: &mut Self,
            registry: &wl_registry::WlRegistry,
            event: wl_registry::Event,
            _: &(),
            _: &Connection,
            qh: &QueueHandle<Self>,
        ) {
            if let wl_registry::Event::Global {
                name,
                interface,
                version,
            } = event
            {
                if interface == wl_seat::WlSeat::interface().name && state.seat.is_none() {
                    let seat = registry.bind::<wl_seat::WlSeat, _, _>(name, version.min(1), qh, ());
                    state.seat = Some(seat);
                } else if interface == ZwpTextInputManagerV3::interface().name
                    && state.manager.is_none()
                {
                    let ver = version.min(2);
                    let mgr = registry.bind::<ZwpTextInputManagerV3, _, _>(name, ver, qh, ());
                    state.manager = Some(mgr);
                    state.manager_version = ver;
                }
            }
        }
    }

    impl Dispatch<wl_seat::WlSeat, ()> for DispatchState {
        fn event(
            _: &mut Self,
            _: &wl_seat::WlSeat,
            _: wl_seat::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<ZwpTextInputManagerV3, ()> for DispatchState {
        fn event(
            _: &mut Self,
            _: &ZwpTextInputManagerV3,
            _: <ZwpTextInputManagerV3 as Proxy>::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<ZwpTextInputV3, ()> for DispatchState {
        fn event(
            state: &mut Self,
            proxy: &ZwpTextInputV3,
            event: zwp_text_input_v3::Event,
            _: &(),
            conn: &Connection,
            _: &QueueHandle<Self>,
        ) {
            match event {
                zwp_text_input_v3::Event::Enter { surface } => {
                    if surface.id().as_ptr() as *const c_void == state.target_surface {
                        let reqs = state.protocol.on_enter();
                        apply_requests(proxy, state.manager_version, &reqs);
                        let _ = conn.flush();
                    }
                }
                zwp_text_input_v3::Event::Leave { surface } => {
                    if surface.id().as_ptr() as *const c_void == state.target_surface {
                        let (reqs, events) = state.protocol.on_leave();
                        apply_requests(proxy, state.manager_version, &reqs);
                        let _ = conn.flush();
                        state.outbox.extend(events);
                    }
                }
                zwp_text_input_v3::Event::PreeditString {
                    text,
                    cursor_begin,
                    cursor_end,
                } => {
                    state
                        .protocol
                        .on_preedit_string(text, cursor_begin, cursor_end);
                }
                zwp_text_input_v3::Event::CommitString { text } => {
                    state.protocol.on_commit_string(text);
                }
                zwp_text_input_v3::Event::DeleteSurroundingText {
                    before_length,
                    after_length,
                } => {
                    state
                        .protocol
                        .on_delete_surrounding_text(before_length, after_length);
                }
                zwp_text_input_v3::Event::Done { serial } => {
                    let events = state.protocol.on_done(serial);
                    state.outbox.extend(events);
                }
                zwp_text_input_v3::Event::Action { action, .. } => {
                    if matches!(action, WEnum::Value(Action::Submit)) {
                        state.protocol.on_action_submit();
                    }
                }
                _ => {}
            }
        }
    }

    fn apply_requests(proxy: &ZwpTextInputV3, version: u32, reqs: &[WaylandRequest]) {
        for req in reqs {
            match req {
                WaylandRequest::Enable => proxy.enable(),
                WaylandRequest::Disable => proxy.disable(),
                WaylandRequest::SetContentType {
                    hint_bits,
                    purpose_code,
                } => {
                    let hint = ContentHint::from_bits_truncate(*hint_bits);
                    let purpose = match *purpose_code {
                        purpose::NUMBER => ContentPurpose::Number,
                        purpose::EMAIL => ContentPurpose::Email,
                        purpose::PASSWORD => ContentPurpose::Password,
                        _ => ContentPurpose::Normal,
                    };
                    proxy.set_content_type(hint, purpose);
                }
                WaylandRequest::SetSurroundingText {
                    text,
                    cursor,
                    anchor,
                } => {
                    proxy.set_surrounding_text(text.clone(), *cursor, *anchor);
                }
                WaylandRequest::SetTextChangeCause { input_method } => {
                    proxy.set_text_change_cause(if *input_method {
                        ChangeCause::InputMethod
                    } else {
                        ChangeCause::Other
                    });
                }
                WaylandRequest::SetCursorRectangle {
                    x,
                    y,
                    width,
                    height,
                } => {
                    proxy.set_cursor_rectangle(*x, *y, *width, *height);
                }
                WaylandRequest::SetAvailableActions { submit } => {
                    if version >= 2 {
                        let actions: Vec<u8> = if *submit {
                            (Action::Submit as u32).to_ne_bytes().to_vec()
                        } else {
                            Vec::new()
                        };
                        proxy.set_available_actions(actions);
                    }
                }
                WaylandRequest::Commit => proxy.commit(),
            }
        }
    }

    /// Live Linux Wayland `zwp_text_input_v3` backend attached to `winit`'s `wl_display`.
    pub struct WaylandTextInput {
        conn: Connection,
        event_queue: EventQueue<DispatchState>,
        state: DispatchState,
        // Keep registry alive for clean teardown.
        _registry: wl_registry::WlRegistry,
    }

    impl WaylandTextInput {
        /// Attempts to attach a native `zwp_text_input_v3` instance to a Wayland `winit` window.
        /// Returns `None` on X11, headless, or compositors without `zwp_text_input_manager_v3`.
        pub fn try_attach(window: &winit::window::Window) -> Option<Self> {
            let display_ptr = match window.display_handle().ok()?.as_raw() {
                RawDisplayHandle::Wayland(h) => h.display.as_ptr(),
                _ => return None,
            };
            let surface_ptr = match window.window_handle().ok()?.as_raw() {
                RawWindowHandle::Wayland(h) => h.surface.as_ptr() as *const c_void,
                _ => return None,
            };

            // SAFETY: `winit`'s `Window` / `EventLoop` owns the live `wl_display`; `PlatformIme`
            // is dropped before `Surface` / `EventLoop`, and `from_foreign_display` never closes it.
            let backend = unsafe { Backend::from_foreign_display(display_ptr.cast()) };
            let conn = Connection::from_backend(backend);
            let mut event_queue = conn.new_event_queue::<DispatchState>();
            let qh = event_queue.handle();
            let registry = conn.display().get_registry(&qh, ());

            let mut state = DispatchState {
                target_surface: surface_ptr,
                seat: None,
                manager: None,
                manager_version: 1,
                text_input: None,
                protocol: WaylandImeProtocol::new(1),
                outbox: Vec::new(),
            };

            // Roundtrip 1: discover `wl_seat` + `zwp_text_input_manager_v3`.
            if event_queue.roundtrip(&mut state).is_err() {
                return None;
            }
            let (Some(seat), Some(manager)) = (&state.seat, &state.manager) else {
                return None;
            };
            state.protocol = WaylandImeProtocol::new(state.manager_version);
            let text_input = manager.get_text_input(seat, &qh, ());
            state.text_input = Some(text_input);

            // Roundtrip 2: receive initial `enter` event if the surface already has keyboard focus.
            let _ = event_queue.roundtrip(&mut state);

            // Keep winit's own internal zwp_text_input_v3 disabled so events are never duplicated.
            window.set_ime_allowed(false);

            Some(Self {
                conn,
                event_queue,
                state,
                _registry: registry,
            })
        }

        pub fn poll_events(&mut self) -> Vec<EditorEvent> {
            let _ = self.event_queue.dispatch_pending(&mut self.state);
            std::mem::take(&mut self.state.outbox)
        }

        pub fn sync(&mut self, window_focused: bool, ime_state: &ImeState) {
            let _ = self.event_queue.dispatch_pending(&mut self.state);
            let reqs = self.state.protocol.plan_sync(window_focused, ime_state);
            self.send_requests(&reqs);
        }

        pub fn update_area(&mut self, area: Option<[f32; 4]>) {
            let reqs = self.state.protocol.plan_area(area);
            self.send_requests(&reqs);
        }

        pub fn interrupt(&mut self) {
            let reqs = self.state.protocol.plan_interrupt();
            self.send_requests(&reqs);
        }

        fn send_requests(&mut self, reqs: &[WaylandRequest]) {
            if reqs.is_empty() {
                return;
            }
            if let Some(ref proxy) = self.state.text_input {
                apply_requests(proxy, self.state.manager_version, reqs);
                let _ = self.conn.flush();
            }
        }
    }

    impl Drop for WaylandTextInput {
        fn drop(&mut self) {
            if let Some(proxy) = self.state.text_input.take() {
                if self.state.protocol.enabled() {
                    proxy.disable();
                    proxy.commit();
                }
                proxy.destroy();
            }
            if let Some(manager) = self.state.manager.take() {
                manager.destroy();
            }
            let _ = self.conn.flush();
        }
    }
}

#[cfg(target_os = "linux")]
pub use live::WaylandTextInput;

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_state(focused_id: Option<u16>, text: &str, cursor_byte: usize) -> ImeState {
        let allowed = focused_id.is_some();
        ImeState {
            focused_id,
            allowed,
            purpose: InputPurpose::Normal,
            multiline: false,
            submit_on_enter: true,
            change_cause: ImeChangeCause::Other,
            revision: 1,
            surrounding: allowed.then(|| SurroundingText {
                text: text.into(),
                cursor_byte,
                anchor_byte: cursor_byte,
                cursor_utf16: text[..cursor_byte].encode_utf16().count(),
                anchor_utf16: text[..cursor_byte].encode_utf16().count(),
                window_byte_offset: 0,
            }),
        }
    }

    #[test]
    fn wayland_done_applies_delete_commit_preedit_and_submit_in_spec_order() {
        let mut proto = WaylandImeProtocol::new(2);
        let st = sample_state(Some(1), "hello茶", "hello茶".len());
        let _ = proto.plan_sync(true, &st);
        let enter_reqs = proto.on_enter();
        assert!(enter_reqs.contains(&WaylandRequest::Enable));
        assert!(enter_reqs.contains(&WaylandRequest::SetAvailableActions { submit: true }));
        assert_eq!(proto.commit_serial(), 1);

        // First preedit with cursor at byte 3 ("に" = 3 UTF-8 bytes -> char 1).
        proto.on_preedit_string(Some("にほん".into()), 3, 6);
        let ev1 = proto.on_done(1);
        assert_eq!(ev1.len(), 1);
        assert!(matches!(
            &ev1[0],
            EditorEvent::Preedit { text, active: Some(r) } if text == "にほん" && *r == (1..2)
        ));

        // Atomic done with delete_surrounding + commit + next preedit (hidden cursor -1/-1) + submit.
        proto.on_delete_surrounding_text(3, 0);
        proto.on_commit_string(Some("日本".into()));
        proto.on_preedit_string(Some("語".into()), -1, -1);
        proto.on_action_submit();
        let ev2 = proto.on_done(1);

        // Exact 7-step order: clear preedit -> delete surrounding bytes -> commit -> new preedit -> submit.
        assert_eq!(ev2.len(), 5, "got {ev2:?}");
        assert!(matches!(&ev2[0], EditorEvent::Preedit { text, active: None } if text.is_empty()));
        assert!(matches!(
            &ev2[1],
            EditorEvent::DeleteSurroundingBytes {
                before_bytes: 3,
                after_bytes: 0
            }
        ));
        assert!(matches!(&ev2[2], EditorEvent::Commit(t) if t == "日本"));
        assert!(matches!(&ev2[3], EditorEvent::Preedit { text, active: None } if text == "語"));
        assert!(matches!(&ev2[4], EditorEvent::Enter));
    }

    #[test]
    fn wayland_deduplicates_state_and_resets_when_switching_fields() {
        let mut proto = WaylandImeProtocol::new(1);
        let _ = proto.on_enter();

        let f1 = sample_state(Some(10), "abc", 3);
        let reqs1 = proto.plan_sync(true, &f1);
        assert!(reqs1.contains(&WaylandRequest::Enable));
        assert!(reqs1.contains(&WaylandRequest::SetSurroundingText {
            text: "abc".into(),
            cursor: 3,
            anchor: 3,
        }));
        assert_eq!(reqs1.last(), Some(&WaylandRequest::Commit));

        // Identical state produces ZERO outgoing requests (prevents candidate popup flicker).
        assert!(proto.plan_sync(true, &f1).is_empty());

        // Incremental surrounding text change caused by IME sends only surrounding + cause + commit.
        let mut f1_ime = sample_state(Some(10), "abc茶", "abc茶".len());
        f1_ime.change_cause = ImeChangeCause::InputMethod;
        let reqs2 = proto.plan_sync(true, &f1_ime);
        assert!(!reqs2.contains(&WaylandRequest::Enable));
        assert!(reqs2.contains(&WaylandRequest::SetTextChangeCause {
            input_method: true
        }));
        assert_eq!(reqs2.last(), Some(&WaylandRequest::Commit));

        // Switching to another TextInput (id 10 -> id 20) MUST disable+commit then enable+commit.
        let mut f2 = sample_state(Some(20), "user@host", 9);
        f2.purpose = InputPurpose::Email;
        let reqs3 = proto.plan_sync(true, &f2);
        assert_eq!(reqs3[0], WaylandRequest::Disable);
        assert_eq!(reqs3[1], WaylandRequest::Commit);
        assert_eq!(reqs3[2], WaylandRequest::Enable);
        assert!(reqs3.contains(&WaylandRequest::SetContentType {
            hint_bits: hint::LOWERCASE,
            purpose_code: purpose::EMAIL,
        }));

        // Focusing a password field disables IME completely.
        let mut pw = sample_state(Some(30), "", 0);
        pw.purpose = InputPurpose::Password;
        pw.allowed = false;
        pw.surrounding = None;
        let reqs4 = proto.plan_sync(true, &pw);
        assert_eq!(reqs4, vec![WaylandRequest::Disable, WaylandRequest::Commit]);
    }

    #[test]
    fn wayland_leave_clears_active_preedit_and_re_enter_restores_state() {
        let mut proto = WaylandImeProtocol::new(2);
        let _ = proto.on_enter();
        let st = sample_state(Some(5), "hola", 4);
        let _ = proto.plan_sync(true, &st);
        let _ = proto.plan_area(Some([12.4, 48.2, 1.0, 18.6]));

        proto.on_preedit_string(Some("かんじ".into()), 0, 9);
        let _ = proto.on_done(proto.commit_serial());

        let (reqs, events) = proto.on_leave();
        assert_eq!(reqs, vec![WaylandRequest::Disable, WaylandRequest::Commit]);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            EditorEvent::Preedit { text, active: None } if text.is_empty()
        ));

        // Re-entering automatically restores Enable + ContentType + Surrounding + CursorRect + Commit.
        let restore = proto.on_enter();
        assert!(restore.contains(&WaylandRequest::Enable));
        assert!(restore.contains(&WaylandRequest::SetCursorRectangle {
            x: 12,
            y: 48,
            width: 1,
            height: 19,
        }));
        assert_eq!(restore.last(), Some(&WaylandRequest::Commit));
    }
}
