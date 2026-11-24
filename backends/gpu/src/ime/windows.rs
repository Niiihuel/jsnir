//! Native Windows IMM32 + TSF (`InputScope`) IME backend.
//!
//! Architecture:
//! - Pure Win32 protocol encoders/decoders (`decode_win32_preedit`, `build_reconvert_string`,
//!   `input_scope_for_purpose`) compile and are unit-tested on all platforms (Linux, Windows, macOS).
//! - [`WindowsIme`] (`#[cfg(target_os = "windows")]`) attaches a clean `SetWindowSubclass` hook to
//!   `winit`'s `HWND` to deliver:
//!   - Multi-clause CJK preedit attributes (`GCS_COMPATTR` → `EditorEvent::PreeditSegments`).
//!   - Contextual surrounding text (`WM_IME_REQUEST` / `IMR_DOCUMENTFEED`) and selection
//!     reconversion (`IMR_RECONVERTSTRING` / `IMR_CONFIRMRECONVERTSTRING`).
//!   - Exact candidate/reconversion anchor positioning (`IMR_QUERYCHARPOSITION` + `ImmSetCandidateWindow`).
//!   - Clean immediate cancellation (`ImmNotifyIME(NI_COMPOSITIONSTR, CPS_CANCEL, 0)`).
//!   - Semantic TSF `InputScope` (`IS_PASSWORD`, `IS_SEARCH`, `IS_EMAIL_SMTPEMAILADDRESS`, `IS_NUMBER`)
//!     loaded dynamically from `msctf.dll`.

use jsnir::{
    editor::{EditorEvent, PreeditSegment, PreeditStyle, SurroundingText},
    tree::InputPurpose,
};

/// Win32 `GCS_COMPATTR` attribute constants.
pub mod attr {
    pub const INPUT: u8 = 0x00;
    pub const TARGET_CONVERTED: u8 = 0x01;
    pub const CONVERTED: u8 = 0x02;
    pub const TARGET_NOTCONVERTED: u8 = 0x03;
    pub const INPUT_ERROR: u8 = 0x04;
    pub const FIXEDCONVERTED: u8 = 0x05;
}

/// Win32 TSF `InputScope` enum values (`inputscope.h`).
pub mod input_scope {
    pub const DEFAULT: i32 = 0;
    pub const EMAIL_SMTP_ADDRESS: i32 = 5;
    pub const NUMBER: i32 = 29;
    pub const PASSWORD: i32 = 31;
    pub const SEARCH: i32 = 50;
}

/// Maps JSNIR's semantic `InputPurpose` to Win32 TSF `InputScope`.
pub fn input_scope_for_purpose(purpose: InputPurpose) -> i32 {
    match purpose {
        InputPurpose::Normal => input_scope::DEFAULT,
        InputPurpose::Password => input_scope::PASSWORD,
        InputPurpose::Search => input_scope::SEARCH,
        InputPurpose::Email => input_scope::EMAIL_SMTP_ADDRESS,
        InputPurpose::Number => input_scope::NUMBER,
    }
}

/// Converts a UTF-16 code-unit offset inside `text` into a Rust `char` index (clamped safely).
pub fn utf16_to_char_index(text: &str, utf16_offset: usize) -> usize {
    let mut u16_pos = 0usize;
    let mut char_pos = 0usize;
    for ch in text.chars() {
        let w = ch.len_utf16();
        if u16_pos + w > utf16_offset {
            break;
        }
        u16_pos += w;
        char_pos += 1;
    }
    char_pos
}

/// Decodes Win32 `GCS_COMPSTR` + `GCS_COMPATTR` + `GCS_CURSORPOS` into a multi-clause
/// [`EditorEvent::PreeditSegments`] (preserving active conversion clause thickness and caret).
pub fn decode_win32_preedit(
    text: &str,
    attrs: &[u8],
    cursor_utf16: Option<usize>,
) -> EditorEvent {
    if text.is_empty() {
        return EditorEvent::Preedit {
            text: String::new(),
            active: None,
        };
    }

    let cursor = cursor_utf16.map(|u16_idx| utf16_to_char_index(text, u16_idx));
    if attrs.is_empty() {
        let len = text.chars().count();
        return EditorEvent::PreeditSegments {
            text: text.to_owned(),
            segments: vec![PreeditSegment {
                range: 0..len,
                style: PreeditStyle::Underline,
            }],
            cursor,
        };
    }

    let mut segments: Vec<PreeditSegment> = Vec::new();
    let mut attr_idx = 0usize;
    let mut char_idx = 0usize;
    let mut current: Option<(usize, PreeditStyle)> = None;

    for ch in text.chars() {
        let code = attrs.get(attr_idx).copied().unwrap_or(attr::INPUT);
        let style = match code {
            attr::TARGET_CONVERTED | attr::TARGET_NOTCONVERTED => PreeditStyle::ThickUnderline,
            _ => PreeditStyle::Underline,
        };
        match current {
            Some((start, cur_style)) if cur_style == style => {
                let _ = start;
            }
            Some((start, cur_style)) => {
                segments.push(PreeditSegment {
                    range: start..char_idx,
                    style: cur_style,
                });
                current = Some((char_idx, style));
            }
            None => current = Some((char_idx, style)),
        }
        attr_idx += ch.len_utf16();
        char_idx += 1;
    }

    if let Some((start, style)) = current
        && start < char_idx
    {
        segments.push(PreeditSegment {
            range: start..char_idx,
            style,
        });
    }

    EditorEvent::PreeditSegments {
        text: text.to_owned(),
        segments,
        cursor,
    }
}

/// Size of the fixed Win32 `RECONVERTSTRING` header (`8 × u32 = 32 bytes`).
pub const RECONVERT_HEADER_BYTES: usize = 32;

/// Builds a binary Win32 `RECONVERTSTRING` buffer (32-byte header + NUL-terminated UTF-16LE text)
/// for answering `WM_IME_REQUEST` with `IMR_DOCUMENTFEED` or `IMR_RECONVERTSTRING`.
///
/// Layout per Win32 docs:
/// - `dwSize`: total byte size including header + UTF-16 string + NUL terminator.
/// - `dwVersion`: `0`.
/// - `dwStrLen`: length of `surrounding.text` in UTF-16 code units (excluding NUL).
/// - `dwStrOffset`: byte offset from start of struct to string (`32`).
/// - `dwCompStrLen` / `dwTargetStrLen`: length of active selection (or 0 at caret) in UTF-16 units.
/// - `dwCompStrOffset` / `dwTargetStrOffset`: **byte** offset from start of the UTF-16 string!
pub fn build_reconvert_string(surrounding: &SurroundingText, for_document_feed: bool) -> Vec<u8> {
    let utf16: Vec<u16> = surrounding.text.encode_utf16().collect();
    let str_len_u16 = utf16.len();
    let total_bytes = RECONVERT_HEADER_BYTES + (str_len_u16 + 1) * 2;

    let sel_lo = surrounding.cursor_utf16.min(surrounding.anchor_utf16).min(str_len_u16);
    let sel_hi = surrounding.cursor_utf16.max(surrounding.anchor_utf16).min(str_len_u16);

    // For `IMR_DOCUMENTFEED`, if there is no selection we report zero-length target at the caret.
    // For `IMR_RECONVERTSTRING`, if there is an active selection we expose it as the target range.
    let (comp_start_u16, comp_len_u16) = if for_document_feed || sel_lo == sel_hi {
        (sel_lo, sel_hi - sel_lo)
    } else {
        (sel_lo, sel_hi - sel_lo)
    };
    let comp_offset_bytes = (comp_start_u16 * 2) as u32;

    let mut buf = Vec::with_capacity(total_bytes);
    let header: [u32; 8] = [
        total_bytes as u32,       // dwSize
        0,                        // dwVersion
        str_len_u16 as u32,       // dwStrLen (UTF-16 code units)
        RECONVERT_HEADER_BYTES as u32, // dwStrOffset (bytes from struct start)
        comp_len_u16 as u32,      // dwCompStrLen (UTF-16 code units)
        comp_offset_bytes,        // dwCompStrOffset (bytes from string start)
        comp_len_u16 as u32,      // dwTargetStrLen (UTF-16 code units)
        comp_offset_bytes,        // dwTargetStrOffset (bytes from string start)
    ];
    for word in header {
        buf.extend_from_slice(&word.to_le_bytes());
    }
    for unit in utf16 {
        buf.extend_from_slice(&unit.to_le_bytes());
    }
    buf.extend_from_slice(&0u16.to_le_bytes());
    buf
}

// ---------- Windows live Win32 IMM32 + TSF implementation ----------

#[cfg(target_os = "windows")]
mod live {
    use super::*;
    use jsnir::host::ImeState;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::{
        ffi::{OsString, c_void},
        os::windows::ffi::OsStringExt,
        ptr::null_mut,
        sync::{Mutex, OnceLock},
    };
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::ClientToScreen,
        System::LibraryLoader::{GetProcAddress, LoadLibraryW},
        UI::{
            Input::Ime::{
                CANDIDATEFORM, CFS_CANDIDATEPOS, CFS_EXCLUDE, CFS_POINT, COMPOSITIONFORM,
                CPS_CANCEL, GCS_COMPATTR, GCS_COMPSTR, GCS_CURSORPOS, GCS_RESULTSTR,
                IACE_CHILDREN, IACE_DEFAULT, IMECHARPOSITION, IMR_CONFIRMRECONVERTSTRING,
                IMR_DOCUMENTFEED, IMR_QUERYCHARPOSITION, IMR_RECONVERTSTRING,
                ImmAssociateContextEx, ImmGetCompositionStringW, ImmGetContext, ImmNotifyIME,
                ImmReleaseContext, ImmSetCandidateWindow, ImmSetCompositionWindow,
                NI_COMPOSITIONSTR, RECONVERTSTRING,
            },
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::{
                GetSystemMetrics, SM_IMMENABLED, WM_IME_COMPOSITION, WM_IME_ENDCOMPOSITION,
                WM_IME_REQUEST, WM_IME_STARTCOMPOSITION, WM_NCDESTROY,
            },
        },
    };

    const SUBCLASS_ID: usize = 0x4A53_4E49; // "JSNI"

    type SetInputScopeFn = unsafe extern "system" fn(HWND, i32) -> i32;

    struct SharedWinIme {
        allowed: bool,
        purpose: InputPurpose,
        focused_id: Option<u16>,
        surrounding: Option<SurroundingText>,
        area_physical: Option<[i32; 4]>,
        outbox: Vec<EditorEvent>,
    }

    struct SubclassBox {
        shared: Mutex<SharedWinIme>,
    }

    static SET_INPUT_SCOPE: OnceLock<Option<SetInputScopeFn>> = OnceLock::new();

    fn apply_input_scope(hwnd: HWND, purpose: InputPurpose) {
        let scope = input_scope_for_purpose(purpose);
        // Load `SetInputScope` once from `msctf.dll` so minimal Windows installs never fail and
        // repeated focus switches incur zero LoadLibrary overhead or refcount leaks.
        let func = *SET_INPUT_SCOPE.get_or_init(|| {
            let dll: Vec<u16> = "msctf.dll\0".encode_utf16().collect();
            unsafe {
                let module = LoadLibraryW(dll.as_ptr());
                if module.is_null() {
                    return None;
                }
                let proc = GetProcAddress(module, b"SetInputScope\0".as_ptr())?;
                Some(std::mem::transmute::<_, SetInputScopeFn>(proc))
            }
        });
        if let Some(set_scope) = func {
            unsafe {
                let _ = set_scope(hwnd, scope);
            }
        }
    }

    unsafe fn imm_get_bytes(himc: *mut c_void, index: u32) -> Option<Vec<u8>> {
        let len = unsafe { ImmGetCompositionStringW(himc, index, null_mut(), 0) };
        if len < 0 {
            return None;
        }
        if len == 0 {
            return Some(Vec::new());
        }
        let mut buf = vec![0u8; len as usize];
        let written = unsafe {
            ImmGetCompositionStringW(himc, index, buf.as_mut_ptr().cast(), len as u32)
        };
        if written < 0 {
            return None;
        }
        buf.truncate(written as usize);
        Some(buf)
    }

    unsafe fn imm_get_string(himc: *mut c_void, index: u32) -> Option<String> {
        let bytes = unsafe { imm_get_bytes(himc, index)? };
        let (prefix, shorts, suffix) = unsafe { bytes.align_to::<u16>() };
        if prefix.is_empty() && suffix.is_empty() {
            OsString::from_wide(shorts).into_string().ok()
        } else {
            None
        }
    }

    unsafe extern "system" fn ime_subclass_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        uid_subclass: usize,
        ref_data: usize,
    ) -> LRESULT {
        let state_ptr = ref_data as *const SubclassBox;
        if state_ptr.is_null() {
            return unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
        }
        let state = unsafe { &*state_ptr };

        match msg {
            WM_IME_STARTCOMPOSITION => {
                // Suppress OS default floating composition window because JSNIR renders preedit inline.
                return 0;
            }
            WM_IME_COMPOSITION => {
                let himc = unsafe { ImmGetContext(hwnd) };
                if !himc.is_null() {
                    let flags = lparam as u32;
                    let mut events = Vec::new();
                    if flags == 0 {
                        events.push(EditorEvent::Preedit {
                            text: String::new(),
                            active: None,
                        });
                    } else {
                        if (flags & GCS_RESULTSTR) != 0
                            && let Some(text) = unsafe { imm_get_string(himc, GCS_RESULTSTR) }
                            && !text.is_empty()
                        {
                            events.push(EditorEvent::Commit(text));
                        }
                        if (flags & GCS_COMPSTR) != 0 {
                            let text =
                                unsafe { imm_get_string(himc, GCS_COMPSTR) }.unwrap_or_default();
                            let attrs = unsafe { imm_get_bytes(himc, GCS_COMPATTR) }
                                .unwrap_or_default();
                            let cursor_raw = unsafe {
                                ImmGetCompositionStringW(himc, GCS_CURSORPOS, null_mut(), 0)
                            };
                            let cursor = (cursor_raw >= 0).then_some(cursor_raw as usize);
                            events.push(decode_win32_preedit(&text, &attrs, cursor));
                        }
                    }
                    unsafe { ImmReleaseContext(hwnd, himc) };
                    if !events.is_empty() {
                        state.shared.lock().unwrap().outbox.extend(events);
                    }
                }
                // Returning 0 prevents winit's WndProc from emitting a duplicate simplified Ime event.
                return 0;
            }
            WM_IME_ENDCOMPOSITION => {
                state.shared.lock().unwrap().outbox.push(EditorEvent::Preedit {
                    text: String::new(),
                    active: None,
                });
                return 0;
            }
            WM_IME_REQUEST => {
                let req = wparam as u32;
                match req {
                    IMR_DOCUMENTFEED | IMR_RECONVERTSTRING => {
                        let guard = state.shared.lock().unwrap();
                        let Some(ref surr) = guard.surrounding else {
                            return 0;
                        };
                        let payload = build_reconvert_string(surr, req == IMR_DOCUMENTFEED);
                        if lparam == 0 {
                            return payload.len() as LRESULT;
                        }
                        let dst = lparam as *mut RECONVERTSTRING;
                        let cap = unsafe { (*dst).dwSize as usize };
                        if cap >= payload.len() {
                            unsafe {
                                std::ptr::copy_nonoverlapping(
                                    payload.as_ptr(),
                                    dst.cast::<u8>(),
                                    payload.len(),
                                );
                            }
                            return payload.len() as LRESULT;
                        }
                        return 0;
                    }
                    IMR_CONFIRMRECONVERTSTRING => {
                        return 1;
                    }
                    IMR_QUERYCHARPOSITION => {
                        if lparam == 0 {
                            return 0;
                        }
                        let guard = state.shared.lock().unwrap();
                        let Some([x, y, _w, h]) = guard.area_physical else {
                            return 0;
                        };
                        let mut pt = POINT { x, y };
                        unsafe {
                            ClientToScreen(hwnd, &mut pt);
                            let pos = &mut *(lparam as *mut IMECHARPOSITION);
                            pos.pt = pt;
                            pos.cLineHeight = h.max(1) as u32;
                            pos.rcDocument = RECT {
                                left: pt.x,
                                top: pt.y,
                                right: pt.x + 1,
                                bottom: pt.y + h.max(1),
                            };
                        }
                        return 1;
                    }
                    _ => {}
                }
            }
            WM_NCDESTROY => {
                unsafe {
                    RemoveWindowSubclass(hwnd, Some(ime_subclass_proc), uid_subclass);
                }
            }
            _ => {}
        }

        unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
    }

    /// Live Windows IMM32 + TSF backend attached to `winit`'s `HWND`.
    pub struct WindowsIme {
        hwnd: HWND,
        state: Box<SubclassBox>,
        last_scale: f64,
    }

    impl WindowsIme {
        pub fn try_attach(window: &winit::window::Window) -> Option<Self> {
            let hwnd = match window.window_handle().ok()?.as_raw() {
                RawWindowHandle::Win32(h) => h.hwnd.get() as HWND,
                _ => return None,
            };
            let state = Box::new(SubclassBox {
                shared: Mutex::new(SharedWinIme {
                    allowed: false,
                    purpose: InputPurpose::Normal,
                    focused_id: None,
                    surrounding: None,
                    area_physical: None,
                    outbox: Vec::new(),
                }),
            });
            let ptr = &*state as *const SubclassBox as usize;
            let ok = unsafe { SetWindowSubclass(hwnd, Some(ime_subclass_proc), SUBCLASS_ID, ptr) };
            if ok == 0 {
                return None;
            }
            // Start with IME disabled until a non-password field is focused.
            unsafe {
                if GetSystemMetrics(SM_IMMENABLED) != 0 {
                    ImmAssociateContextEx(hwnd, null_mut(), IACE_CHILDREN);
                }
            }
            Some(Self {
                hwnd,
                state,
                last_scale: window.scale_factor(),
            })
        }

        pub fn poll_events(&mut self) -> Vec<EditorEvent> {
            std::mem::take(&mut self.state.shared.lock().unwrap().outbox)
        }

        pub fn sync(&mut self, window: &winit::window::Window, window_focused: bool, ime: &ImeState) {
            self.last_scale = window.scale_factor();
            let allowed = window_focused && ime.allowed;
            let mut guard = self.state.shared.lock().unwrap();
            let focus_switched = guard.allowed && allowed && guard.focused_id != ime.focused_id;
            let allowed_changed = guard.allowed != allowed;
            let purpose_changed = guard.purpose != ime.purpose;

            guard.allowed = allowed;
            guard.purpose = ime.purpose;
            guard.focused_id = ime.focused_id;
            guard.surrounding = if allowed { ime.surrounding.clone() } else { None };
            drop(guard);

            if focus_switched {
                self.cancel_composition();
            }
            if purpose_changed {
                apply_input_scope(self.hwnd, ime.purpose);
            }
            if allowed_changed && unsafe { GetSystemMetrics(SM_IMMENABLED) != 0 } {
                let flags = if allowed { IACE_DEFAULT } else { IACE_CHILDREN };
                unsafe {
                    ImmAssociateContextEx(self.hwnd, null_mut(), flags);
                }
            }
        }

        pub fn update_area(&mut self, window: &winit::window::Window, area: Option<[f32; 4]>) {
            let Some([lx, ly, lw, lh]) = area else {
                self.state.shared.lock().unwrap().area_physical = None;
                return;
            };
            let scale = window.scale_factor();
            self.last_scale = scale;
            let phys = [
                (f64::from(lx) * scale).round() as i32,
                (f64::from(ly) * scale).round() as i32,
                (f64::from(lw) * scale).ceil().max(1.) as i32,
                (f64::from(lh) * scale).ceil().max(1.) as i32,
            ];
            {
                let mut guard = self.state.shared.lock().unwrap();
                if !guard.allowed || guard.area_physical == Some(phys) {
                    return;
                }
                guard.area_physical = Some(phys);
            }
            if unsafe { GetSystemMetrics(SM_IMMENABLED) == 0 } {
                return;
            }
            let himc = unsafe { ImmGetContext(self.hwnd) };
            if himc.is_null() {
                return;
            }
            let [x, y, w, h] = phys;
            let rc_area = RECT {
                left: x,
                top: y,
                right: x + w,
                bottom: y + h,
            };
            let candidate = CANDIDATEFORM {
                dwIndex: 0,
                dwStyle: CFS_EXCLUDE | CFS_CANDIDATEPOS,
                ptCurrentPos: POINT { x, y: y + h },
                rcArea: rc_area,
            };
            let composition = COMPOSITIONFORM {
                dwStyle: CFS_POINT,
                ptCurrentPos: POINT { x, y: y + h },
                rcArea: rc_area,
            };
            unsafe {
                ImmSetCompositionWindow(himc, &composition);
                ImmSetCandidateWindow(himc, &candidate);
                ImmReleaseContext(self.hwnd, himc);
            }
        }

        pub fn interrupt(&mut self) {
            self.cancel_composition();
        }

        fn cancel_composition(&self) {
            if unsafe { GetSystemMetrics(SM_IMMENABLED) == 0 } {
                return;
            }
            let himc = unsafe { ImmGetContext(self.hwnd) };
            if !himc.is_null() {
                unsafe {
                    ImmNotifyIME(himc, NI_COMPOSITIONSTR, CPS_CANCEL, 0);
                    ImmReleaseContext(self.hwnd, himc);
                }
            }
        }
    }

    impl Drop for WindowsIme {
        fn drop(&mut self) {
            unsafe {
                RemoveWindowSubclass(self.hwnd, Some(ime_subclass_proc), SUBCLASS_ID);
            }
        }
    }
}

#[cfg(target_os = "windows")]
pub use live::WindowsIme;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn win32_preedit_decodes_multiclause_cjk_attributes_and_surrogate_cursor() {
        // "日本😀語": 日(1 u16) + 本(1 u16) + 😀(2 u16 surrogate pair!) + 語(1 u16) = 5 UTF-16 units, 4 chars.
        let text = "日本😀語";
        let attrs = [
            attr::TARGET_CONVERTED, // 日
            attr::TARGET_CONVERTED, // 本
            attr::CONVERTED,        // 😀 high surrogate
            attr::CONVERTED,        // 😀 low surrogate
            attr::INPUT,            // 語
        ];
        // Cursor at UTF-16 offset 4 -> right after 😀 (char index 3).
        let ev = decode_win32_preedit(text, &attrs, Some(4));
        match ev {
            EditorEvent::PreeditSegments {
                text: out_text,
                segments,
                cursor,
            } => {
                assert_eq!(out_text, text);
                assert_eq!(cursor, Some(3));
                assert_eq!(
                    segments,
                    vec![
                        PreeditSegment {
                            range: 0..2,
                            style: PreeditStyle::ThickUnderline,
                        },
                        PreeditSegment {
                            range: 2..4,
                            style: PreeditStyle::Underline,
                        },
                    ]
                );
            }
            other => panic!("expected PreeditSegments, got {other:?}"),
        }
    }

    #[test]
    fn win32_reconvert_string_encodes_valid_utf16_header_and_byte_offsets() {
        let text = "hello 日本語!";
        let surr = SurroundingText {
            text: text.into(),
            cursor_byte: "hello 日本".len(),
            anchor_byte: "hello ".len(),
            cursor_utf16: 8,
            anchor_utf16: 6,
            window_byte_offset: 0,
        };
        let buf = build_reconvert_string(&surr, false);
        let read_u32 = |idx: usize| -> u32 {
            u32::from_le_bytes(buf[idx * 4..(idx + 1) * 4].try_into().unwrap())
        };
        let expected_u16_len = text.encode_utf16().count() as u32;
        assert_eq!(read_u32(0) as usize, buf.len()); // dwSize
        assert_eq!(read_u32(1), 0); // dwVersion
        assert_eq!(read_u32(2), expected_u16_len); // dwStrLen
        assert_eq!(read_u32(3), RECONVERT_HEADER_BYTES as u32); // dwStrOffset
        assert_eq!(read_u32(4), 2); // dwCompStrLen ("日本" = 2 UTF-16 units)
        assert_eq!(read_u32(5), 12); // dwCompStrOffset (6 UTF-16 units * 2 bytes = 12 bytes)
        assert_eq!(read_u32(6), 2); // dwTargetStrLen
        assert_eq!(read_u32(7), 12); // dwTargetStrOffset

        // Verify NUL-terminated UTF-16 payload roundtrips cleanly.
        let units: Vec<u16> = buf[RECONVERT_HEADER_BYTES..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(units.last(), Some(&0));
        let decoded = String::from_utf16(&units[..units.len() - 1]).unwrap();
        assert_eq!(decoded, text);
    }

    #[test]
    fn win32_input_scope_maps_every_semantic_purpose() {
        assert_eq!(input_scope_for_purpose(InputPurpose::Normal), input_scope::DEFAULT);
        assert_eq!(input_scope_for_purpose(InputPurpose::Password), input_scope::PASSWORD);
        assert_eq!(input_scope_for_purpose(InputPurpose::Search), input_scope::SEARCH);
        assert_eq!(input_scope_for_purpose(InputPurpose::Email), input_scope::EMAIL_SMTP_ADDRESS);
        assert_eq!(input_scope_for_purpose(InputPurpose::Number), input_scope::NUMBER);
    }
}
