//! Platform input shared by every window: keys and input-method events for the line editor, the
//! clipboard, and click counting for word and line selection.
use jsnir::{
    editor::{Direction, EditorEvent, PointSelection, SurroundingText},
    host::ImeState,
    tree::InputPurpose,
};
use std::time::{Duration, Instant};
use winit::{
    event::{Ime, KeyEvent},
    keyboard::{Key, ModifiersState, NamedKey},
    window::ImePurpose,
};

/// What a key press means for a focused line editor.
pub enum KeyInput {
    Edit(EditorEvent),
    /// Paste: the window reads the clipboard, after applying the edits queued before it.
    Paste,
}

/// Maps a key press to an editor action, with the platform's shortcuts.
pub fn editor_key(key: &KeyEvent, mods: ModifiersState) -> Option<KeyInput> {
    let command = if cfg!(target_os = "macos") {
        mods.super_key()
    } else {
        mods.control_key() && !mods.alt_key()
    };
    let word = if cfg!(target_os = "macos") {
        mods.alt_key()
    } else {
        command
    };
    let edit = |event| Some(KeyInput::Edit(event));
    let text = || {
        key.text
            .as_ref()
            .map(|text| KeyInput::Edit(EditorEvent::Insert(text.to_string())))
    };
    match key.logical_key.as_ref() {
        Key::Character(character) if command => match character.to_ascii_lowercase().as_str() {
            "a" => edit(EditorEvent::SelectAll),
            "c" => edit(EditorEvent::Copy),
            "x" => edit(EditorEvent::Cut),
            "v" => Some(KeyInput::Paste),
            "z" if mods.shift_key() => edit(EditorEvent::Redo),
            "z" => edit(EditorEvent::Undo),
            "y" => edit(EditorEvent::Redo),
            _ => None,
        },
        Key::Named(name) => {
            let direction = match name {
                NamedKey::ArrowLeft if word => Some(Direction::WordLeft),
                NamedKey::ArrowRight if word => Some(Direction::WordRight),
                NamedKey::ArrowLeft => Some(Direction::Left),
                NamedKey::ArrowRight => Some(Direction::Right),
                NamedKey::ArrowUp => Some(Direction::Up),
                NamedKey::ArrowDown => Some(Direction::Down),
                NamedKey::PageUp => Some(Direction::PageUp),
                NamedKey::PageDown => Some(Direction::PageDown),
                NamedKey::Home if command => Some(Direction::TextStart),
                NamedKey::End if command => Some(Direction::TextEnd),
                NamedKey::Home => Some(Direction::Start),
                NamedKey::End => Some(Direction::End),
                _ => None,
            };
            if let Some(direction) = direction {
                return edit(EditorEvent::Move {
                    direction,
                    extend: mods.shift_key(),
                });
            }
            match name {
                NamedKey::Backspace | NamedKey::Delete => edit(EditorEvent::Delete {
                    backward: name == NamedKey::Backspace,
                    word,
                }),
                NamedKey::Enter if !key.repeat && mods.is_empty() => edit(EditorEvent::Enter),
                NamedKey::Enter if !key.repeat && mods.shift_key() && !command => {
                    edit(EditorEvent::LineBreak)
                }
                NamedKey::Escape => edit(EditorEvent::Cancel),
                NamedKey::Space => text(),
                _ => None,
            }
        }
        _ => text(),
    }
}

/// Converts a winit input-method event. winit gives UTF-8 byte ranges; the editor counts characters.
pub fn ime_event(event: Ime) -> Option<EditorEvent> {
    let event = match event {
        Ime::Preedit(text, selection) => {
            let active = selection.and_then(|(a, b)| {
                Some(text.get(..a.min(b))?.chars().count()..text.get(..a.max(b))?.chars().count())
            });
            EditorEvent::Preedit { text, active }
        }
        Ime::Commit(text) => EditorEvent::Commit(text),
        Ime::Disabled => EditorEvent::Preedit {
            text: String::new(),
            active: None,
        },
        Ime::Enabled => return None,
    };
    Some(event)
}

/// The system clipboard, opened on first use. Errors are reported and the operation is skipped.
#[derive(Default)]
pub struct Clipboard(Option<arboard::Clipboard>);
impl Clipboard {
    fn with<T>(
        &mut self,
        operation: impl FnOnce(&mut arboard::Clipboard) -> Result<T, arboard::Error>,
    ) -> Option<T> {
        let result = (|| {
            if self.0.is_none() {
                self.0 = Some(arboard::Clipboard::new()?);
            }
            operation(self.0.as_mut().unwrap())
        })();
        result.map_err(|error| eprintln!("clipboard: {error}")).ok()
    }
    pub fn text(&mut self) -> Option<String> {
        self.with(arboard::Clipboard::get_text)
    }
    pub fn set_text(&mut self, text: String) {
        self.with(|clipboard| clipboard.set_text(text));
    }
}

/// Counts consecutive clicks in the same spot: one places the caret, two select a word, three everything.
#[derive(Default)]
pub struct Clicks(Option<(Instant, [f32; 2], u8)>);
impl Clicks {
    pub fn select(&mut self, point: [f32; 2], shift: bool) -> PointSelection {
        let now = Instant::now();
        let count = self
            .0
            .filter(|&(time, position, _)| {
                now.duration_since(time) <= Duration::from_millis(450)
                    && (position[0] - point[0]).abs() < 6.
                    && (position[1] - point[1]).abs() < 6.
            })
            .map_or(1, |(_, _, count)| count % 3 + 1);
        self.0 = Some((now, point, count));
        match count {
            3 => PointSelection::All,
            2 => PointSelection::Word,
            _ if shift => PointSelection::Extend,
            _ => PointSelection::Place,
        }
    }
}

/// Where the input-method window goes. On Wayland every `set_ime_cursor_area` commits text-input
/// state, so the area is sent only when it changes; enabling the input method again forgets it.
#[derive(Default)]
pub struct ImeArea(Option<[f32; 4]>);
impl ImeArea {
    pub fn update(&mut self, window: &winit::window::Window, [x, y, w, h]: [f32; 4]) {
        if self.0 != Some([x, y, w, h]) {
            self.0 = Some([x, y, w, h]);
            window.set_ime_cursor_area(
                winit::dpi::LogicalPosition::new(x, y),
                winit::dpi::LogicalSize::new(w, h),
            );
        }
    }
    pub fn forget(&mut self) {
        self.0 = None;
    }
}

/// Maps JSNIR's semantic `InputPurpose` to winit's `ImePurpose` (Linux Wayland/X11 & Windows TSF).
pub fn winit_ime_purpose(purpose: InputPurpose) -> ImePurpose {
    match purpose {
        InputPurpose::Password => ImePurpose::Password,
        InputPurpose::Normal
        | InputPurpose::Search
        | InputPurpose::Email
        | InputPurpose::Number => ImePurpose::Normal,
    }
}

/// Cross-platform IME controller for a native window (Linux Wayland `zwp_text_input_v3` / X11 XIM,
/// Windows TSF / IMM32, macOS). Deduplicates every state transition so Wayland never receives
/// redundant `commit` requests that flicker candidate popups.
#[derive(Default)]
pub struct PlatformIme {
    allowed: bool,
    purpose: Option<ImePurpose>,
    area: ImeArea,
    surrounding: Option<SurroundingText>,
}

impl PlatformIme {
    pub fn allowed(&self) -> bool {
        self.allowed
    }

    /// Synchronizes IME enablement, semantic purpose, and surrounding text cache with the host.
    pub fn sync(&mut self, window: &winit::window::Window, window_focused: bool, state: ImeState) {
        let allowed = window_focused && state.allowed;
        let purpose = winit_ime_purpose(state.purpose);
        if self.purpose != Some(purpose) {
            window.set_ime_purpose(purpose);
            self.purpose = Some(purpose);
        }
        if allowed != self.allowed {
            window.set_ime_allowed(allowed);
            self.allowed = allowed;
            self.area.forget();
        }
        self.surrounding = if allowed { state.surrounding } else { None };
    }

    /// Updates the candidate window anchor rectangle when IME is active.
    pub fn update_area(&mut self, window: &winit::window::Window, area: Option<[f32; 4]>) {
        if let (true, Some(rect)) = (self.allowed, area) {
            self.area.update(window, rect);
        }
    }

    /// Resets active OS preedit (e.g. after mouse click or Escape) while preserving purpose.
    pub fn interrupt(&mut self, window: &winit::window::Window) {
        if self.allowed {
            self.area.forget();
            window.set_ime_allowed(false);
            window.set_ime_allowed(true);
            if let Some(purpose) = self.purpose {
                window.set_ime_purpose(purpose);
            }
        }
    }

    /// Latest deduplicated surrounding text window (ready for native Wayland/TSF extensions).
    pub fn surrounding(&self) -> Option<&SurroundingText> {
        self.surrounding.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn winit_preedit_turns_utf8_byte_ranges_into_editor_characters() {
        let text = "é茶";
        let event = ime_event(Ime::Preedit(text.into(), Some((text.len(), 2))));
        assert!(
            matches!(event, Some(EditorEvent::Preedit { active: Some(range), .. }) if range == (1..2))
        );
        let event = ime_event(Ime::Preedit(text.into(), Some((1, text.len()))));
        assert!(matches!(
            event,
            Some(EditorEvent::Preedit { active: None, .. })
        ));
    }
}
