//! Unified cross-platform native IME subsystem for JSNIR GPU windows.
//!
//! Automatically selects the highest-fidelity backend available for the host window:
//! - **Linux Wayland**: native [`wayland::WaylandTextInput`] (`zwp_text_input_v3` v1/v2) over an
//!   isolated `wl_event_queue` on `winit`'s `wl_display`, providing real `set_surrounding_text`,
//!   `set_text_change_cause`, `delete_surrounding_text`, semantic `content_hint`/`content_purpose`
//!   (`Normal`/`Password`/`Search`/`Email`/`Number` + `Multiline` + `PreeditShown`), and v2 submit
//!   actions without conflicting with `winit`.
//! - **Windows**: native [`windows::WindowsIme`] (`SetWindowSubclass` over `HWND`) providing
//!   multi-clause CJK attributes (`GCS_COMPATTR` → `EditorEvent::PreeditSegments`), contextual
//!   surrounding text (`IMR_DOCUMENTFEED`), selection reconversion (`IMR_RECONVERTSTRING`), exact
//!   candidate positioning (`IMR_QUERYCHARPOSITION`), immediate cancellation (`ImmNotifyIME`), and
//!   dynamic TSF `SetInputScope` (`msctf.dll`).
//! - **Linux X11 (XIM) / macOS / fallback**: upgraded `winit` IME controller with focus-switch
//!   detection (`focused_id`), deduplicated `ImeArea` and `ImePurpose`, and clean interruption.

pub mod wayland;
pub mod windows;

use crate::input::{ImeArea, ime_event};
use jsnir::{
    editor::{EditorEvent, SurroundingText},
    host::ImeState,
    tree::InputPurpose,
};
use winit::{
    event::Ime,
    window::{ImePurpose, Window},
};

/// Maps JSNIR's semantic `InputPurpose` to `winit`'s fallback `ImePurpose`.
pub fn winit_ime_purpose(purpose: InputPurpose) -> ImePurpose {
    match purpose {
        InputPurpose::Password => ImePurpose::Password,
        InputPurpose::Normal
        | InputPurpose::Search
        | InputPurpose::Email
        | InputPurpose::Number => ImePurpose::Normal,
    }
}

#[derive(Default)]
struct WinitFallback {
    allowed: bool,
    focused_id: Option<u16>,
    purpose: Option<ImePurpose>,
    area: ImeArea,
    surrounding: Option<SurroundingText>,
}

impl WinitFallback {
    fn sync(&mut self, window: &Window, window_focused: bool, state: &ImeState) {
        let allowed = window_focused && state.allowed;
        let purpose = winit_ime_purpose(state.purpose);
        let focus_switched = self.allowed && allowed && self.focused_id != state.focused_id;
        self.focused_id = state.focused_id;

        if self.purpose != Some(purpose) {
            window.set_ime_purpose(purpose);
            self.purpose = Some(purpose);
        }

        if focus_switched {
            // Reset OS preedit when moving focus directly from Input A to Input B.
            self.area.forget();
            window.set_ime_allowed(false);
            window.set_ime_allowed(true);
            window.set_ime_purpose(purpose);
        } else if allowed != self.allowed {
            window.set_ime_allowed(allowed);
            self.allowed = allowed;
            self.area.forget();
        }
        self.surrounding = if allowed {
            state.surrounding.clone()
        } else {
            None
        };
    }

    fn update_area(&mut self, window: &Window, area: Option<[f32; 4]>) {
        if let (true, Some(rect)) = (self.allowed, area) {
            self.area.update(window, rect);
        }
    }

    fn interrupt(&mut self, window: &Window) {
        if self.allowed {
            self.area.forget();
            window.set_ime_allowed(false);
            window.set_ime_allowed(true);
            if let Some(purpose) = self.purpose {
                window.set_ime_purpose(purpose);
            }
        }
    }
}

enum Backend {
    #[cfg(target_os = "linux")]
    Wayland(Box<wayland::WaylandTextInput>),
    #[cfg(target_os = "windows")]
    Windows(Box<windows::WindowsIme>),
    Winit(WinitFallback),
}

impl Default for Backend {
    fn default() -> Self {
        Self::Winit(WinitFallback::default())
    }
}

/// Cross-platform native IME controller owned by each GPU window.
///
/// Field ordering requirement: store `PlatformIme` **before** `Surface`/`Window` (or call
/// [`PlatformIme::detach`] before dropping the window) so native Wayland/Win32 hooks detach while
/// the underlying display/HWND is still live.
#[derive(Default)]
pub struct PlatformIme {
    backend: Backend,
    allowed: bool,
    surrounding: Option<SurroundingText>,
}

impl PlatformIme {
    /// Probes and attaches the best native IME backend for `window` (Wayland `zwp_text_input_v3`
    /// on Linux Wayland, IMM32+TSF on Windows, or upgraded `winit` fallback on X11/macOS).
    pub fn attach(&mut self, window: &Window) {
        self.detach();

        #[cfg(target_os = "linux")]
        if let Some(wl) = wayland::WaylandTextInput::try_attach(window) {
            self.backend = Backend::Wayland(Box::new(wl));
            return;
        }

        #[cfg(target_os = "windows")]
        if let Some(win) = windows::WindowsIme::try_attach(window) {
            self.backend = Backend::Windows(Box::new(win));
            return;
        }

        self.backend = Backend::Winit(WinitFallback::default());
    }

    /// Cleanly tears down native Wayland/Win32 hooks before the window/surface is dropped.
    pub fn detach(&mut self) {
        self.backend = Backend::Winit(WinitFallback::default());
        self.allowed = false;
        self.surrounding = None;
    }

    /// Name of the active IME backend (`"wayland-text-input-v3"`, `"windows-imm-tsf"`, or `"winit"`).
    pub fn backend_name(&self) -> &'static str {
        match &self.backend {
            #[cfg(target_os = "linux")]
            Backend::Wayland(_) => "wayland-text-input-v3",
            #[cfg(target_os = "windows")]
            Backend::Windows(_) => "windows-imm-tsf",
            Backend::Winit(_) => "winit",
        }
    }

    /// Returns `true` when IME composition is currently permitted on the focused field.
    pub fn allowed(&self) -> bool {
        self.allowed
    }

    /// Latest deduplicated surrounding text window.
    pub fn surrounding(&self) -> Option<&SurroundingText> {
        self.surrounding.as_ref()
    }

    /// Drains any pending native IME events (Wayland `zwp_text_input_v3` or Windows `WM_IME_*`).
    pub fn poll_events(&mut self) -> Vec<EditorEvent> {
        match &mut self.backend {
            #[cfg(target_os = "linux")]
            Backend::Wayland(wl) => wl.poll_events(),
            #[cfg(target_os = "windows")]
            Backend::Windows(win) => win.poll_events(),
            Backend::Winit(_) => Vec::new(),
        }
    }

    /// Translates a `winit::event::Ime` event when running on the `winit` fallback backend (X11/macOS).
    /// Ignored when a native backend (`WaylandTextInput` / `WindowsIme`) owns composition.
    pub fn handle_winit_ime(&mut self, event: Ime) -> Option<EditorEvent> {
        match &self.backend {
            Backend::Winit(_) => ime_event(event),
            #[cfg(target_os = "linux")]
            Backend::Wayland(_) => None,
            #[cfg(target_os = "windows")]
            Backend::Windows(_) => None,
        }
    }

    /// Synchronizes IME enablement, semantic purpose, multiline/submit hints, and surrounding text.
    pub fn sync(&mut self, window: &Window, window_focused: bool, state: ImeState) {
        self.allowed = window_focused && state.allowed;
        self.surrounding = if self.allowed {
            state.surrounding.clone()
        } else {
            None
        };
        match &mut self.backend {
            #[cfg(target_os = "linux")]
            Backend::Wayland(wl) => wl.sync(window_focused, &state),
            #[cfg(target_os = "windows")]
            Backend::Windows(win) => win.sync(window, window_focused, &state),
            Backend::Winit(fb) => fb.sync(window, window_focused, &state),
        }
    }

    /// Updates the candidate window anchor rectangle (in logical window coordinates).
    pub fn update_area(&mut self, window: &Window, area: Option<[f32; 4]>) {
        match &mut self.backend {
            #[cfg(target_os = "linux")]
            Backend::Wayland(wl) => wl.update_area(area),
            #[cfg(target_os = "windows")]
            Backend::Windows(win) => win.update_area(window, area),
            Backend::Winit(fb) => fb.update_area(window, area),
        }
    }

    /// Immediately resets active OS composition (e.g. after mouse click inside field or Escape).
    pub fn interrupt(&mut self, window: &Window) {
        match &mut self.backend {
            #[cfg(target_os = "linux")]
            Backend::Wayland(wl) => wl.interrupt(),
            #[cfg(target_os = "windows")]
            Backend::Windows(win) => win.interrupt(),
            Backend::Winit(fb) => fb.interrupt(window),
        }
    }
}
