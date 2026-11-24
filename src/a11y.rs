//! Native accessibility via AccessKit: builds a live semantic tree from `Host`,
//! computes minimal incremental `TreeUpdate` diffs while assistive technologies are active,
//! and dispatches platform `ActionRequest`s (focus, click, edit, selection, scroll, context menu).
use crate::{
    editor::{EditorEvent, mask_text},
    host::Host,
    text::plain_text,
    tree::{A11y, A11yLive, A11yRole, InputPurpose, Node, Position},
};
use accesskit::{
    Action, ActionData, ActionRequest, Affine, Live, Node as AkNode, NodeId, Rect as AkRect, Role,
    ScrollUnit, TextDirection, TextPosition, TextSelection, Toggled, TreeId, TreeInfo, TreeUpdate,
};
use std::collections::HashMap;
use unicode_segmentation::UnicodeSegmentation;

/// Root AccessKit `NodeId` representing the native window container (`Role::Window`).
pub const WINDOW_NODE_ID: NodeId = NodeId(0);

/// Converts a JSNIR tree node ID (`1..=65535`) into its AccessKit `NodeId`.
#[inline]
pub const fn node_id(id: u16) -> NodeId {
    NodeId(id as u64)
}

/// Synthetic `Role::TextRun` child `NodeId` for line `line_idx` inside `Node::Input(input_id)`.
#[inline]
pub const fn text_run_id(input_id: u16, line_idx: usize) -> NodeId {
    NodeId((1u64 << 32) | ((input_id as u64) << 16) | ((line_idx as u64) & 0xffff))
}

/// Decoded target of an AccessKit `NodeId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Window,
    Node(u16),
    InputTextRun { input_id: u16, line_idx: usize },
}

/// Decodes a `NodeId` produced by this module back into a window, tree node, or input text run.
pub const fn decode_node_id(id: NodeId) -> Option<Target> {
    let raw = id.0;
    if raw == 0 {
        return Some(Target::Window);
    }
    if raw <= u16::MAX as u64 {
        return Some(Target::Node(raw as u16));
    }
    if (raw >> 32) == 1 {
        let input_id = ((raw >> 16) & 0xffff) as u16;
        let line_idx = (raw & 0xffff) as usize;
        if input_id != 0 {
            return Some(Target::InputTextRun { input_id, line_idx });
        }
    }
    None
}

/// Incremental diff cache for published AccessKit nodes.
/// Remains empty (zero allocations / zero per-frame work) whenever no assistive technology is active.
#[derive(Default)]
pub(crate) struct A11yCache {
    initialized: bool,
    nodes: HashMap<NodeId, AkNode>,
    focus: Option<NodeId>,
}

impl A11yCache {
    pub(crate) fn reset(&mut self) {
        self.initialized = false;
        self.nodes.clear();
        self.focus = None;
    }
}

fn map_role(role: A11yRole) -> Role {
    match role {
        A11yRole::Generic => Role::GenericContainer,
        A11yRole::Group => Role::Group,
        A11yRole::Region => Role::Region,
        A11yRole::Navigation => Role::Navigation,
        A11yRole::Main => Role::Main,
        A11yRole::Toolbar => Role::Toolbar,
        A11yRole::Button => Role::Button,
        A11yRole::Link => Role::Link,
        A11yRole::CheckBox => Role::CheckBox,
        A11yRole::Switch => Role::Switch,
        A11yRole::RadioButton => Role::RadioButton,
        A11yRole::Tab => Role::Tab,
        A11yRole::TabList => Role::TabList,
        A11yRole::TabPanel => Role::TabPanel,
        A11yRole::Menu => Role::Menu,
        A11yRole::MenuItem => Role::MenuItem,
        A11yRole::Dialog => Role::Dialog,
        A11yRole::AlertDialog => Role::AlertDialog,
        A11yRole::Alert => Role::Alert,
        A11yRole::Status => Role::Status,
        A11yRole::Tooltip => Role::Tooltip,
        A11yRole::Heading => Role::Heading,
        A11yRole::Label => Role::Label,
        A11yRole::List => Role::List,
        A11yRole::ListItem => Role::ListItem,
        A11yRole::Tree => Role::Tree,
        A11yRole::TreeItem => Role::TreeItem,
        A11yRole::ScrollView => Role::ScrollView,
        A11yRole::Slider => Role::Slider,
        A11yRole::ProgressIndicator => Role::ProgressIndicator,
        A11yRole::Image => Role::Image,
        A11yRole::Terminal => Role::Terminal,
    }
}

fn map_live(live: A11yLive) -> Live {
    match live {
        A11yLive::Off => Live::Off,
        A11yLive::Polite => Live::Polite,
        A11yLive::Assertive => Live::Assertive,
    }
}

fn infer_role(node: &Node, a11y: Option<&A11y>) -> Role {
    if let Some(role) = a11y.and_then(|a| a.role) {
        return map_role(role);
    }
    match node {
        Node::Input {
            purpose, multiline, ..
        } => match purpose {
            InputPurpose::Password => Role::PasswordInput,
            InputPurpose::Search => Role::SearchInput,
            InputPurpose::Email => Role::EmailInput,
            InputPurpose::Number => Role::NumberInput,
            InputPurpose::Normal if *multiline => Role::MultilineTextInput,
            InputPurpose::Normal => Role::TextInput,
        },
        Node::Button { .. } => Role::Button,
        Node::Text { .. } | Node::Label { .. } => Role::Label,
        Node::Image { .. } | Node::Icon { .. } => Role::Image,
        Node::List { .. } => Role::List,
        Node::Box { press, style, .. } => {
            if *press != 0 {
                Role::Button
            } else if style.scroll {
                Role::ScrollView
            } else {
                Role::GenericContainer
            }
        }
        Node::Column { .. } => Role::GenericContainer,
    }
}

fn role_uses_child_labels(role: Role) -> bool {
    matches!(
        role,
        Role::Button
            | Role::DefaultButton
            | Role::Link
            | Role::CheckBox
            | Role::Switch
            | Role::RadioButton
            | Role::Tab
            | Role::MenuItem
            | Role::Heading
            | Role::Tooltip
    )
}

fn ak_rect([x, y, w, h]: [f32; 4]) -> AkRect {
    AkRect::new(
        f64::from(x),
        f64::from(y),
        f64::from(x + w.max(0.)),
        f64::from(y + h.max(0.)),
    )
}

/// Splits text into AccessKit hard lines: every non-final line keeps its trailing `\n`,
/// and an empty string produces one empty line so caret position `(run 0, 0)` is always valid.
fn split_text_runs(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return vec![""];
    }
    let mut lines = Vec::new();
    let mut start = 0;
    for (idx, ch) in text.char_indices() {
        if ch == '\n' {
            lines.push(&text[start..=idx]);
            start = idx + 1;
        }
    }
    if start < text.len() || text.ends_with('\n') {
        lines.push(&text[start..]);
    }
    lines
}

/// Computes sorted character start indices of words within a single text run line.
fn word_starts_for_line(line: &str) -> Vec<u8> {
    if line.is_empty() {
        return Vec::new();
    }
    let mut byte_to_char = Vec::with_capacity(line.len() + 1);
    let mut char_idx = 0usize;
    for (b, ch) in line.char_indices() {
        while byte_to_char.len() < b {
            byte_to_char.push(char_idx);
        }
        byte_to_char.push(char_idx);
        char_idx += 1;
        let _ = ch;
    }
    byte_to_char.push(char_idx);

    let mut starts = Vec::new();
    let mut prev = None;
    for (b, _) in line.split_word_bound_indices() {
        if let Some(&c) = byte_to_char.get(b)
            && c <= u8::MAX as usize
            && prev != Some(c as u8)
        {
            starts.push(c as u8);
            prev = Some(c as u8);
        }
    }
    starts
}

/// Maps a global character offset `offset` into `(line_idx, character_index_in_line)` across `lines`.
fn global_char_to_position(input_id: u16, lines: &[&str], offset: usize) -> TextPosition {
    let mut remaining = offset;
    for (line_idx, line) in lines.iter().enumerate() {
        let len = line.chars().count();
        let has_newline = line.ends_with('\n');
        // AccessKit rule: on a line ending with `\n`, caret at end-of-line sits ON `\n` (len - 1),
        // while `len` belongs to index 0 of the following line.
        if has_newline {
            if remaining < len {
                return TextPosition {
                    node: text_run_id(input_id, line_idx),
                    character_index: remaining,
                };
            }
            remaining -= len;
        } else if remaining <= len || line_idx + 1 == lines.len() {
            return TextPosition {
                node: text_run_id(input_id, line_idx),
                character_index: remaining.min(len),
            };
        } else {
            remaining -= len;
        }
    }
    TextPosition {
        node: text_run_id(input_id, 0),
        character_index: 0,
    }
}

/// Inverse of `global_char_to_position`: resolves a `TextPosition` on `input_id` back to a global char offset.
fn position_to_global_char(input_id: u16, lines: &[&str], pos: TextPosition) -> Option<usize> {
    let Target::InputTextRun {
        input_id: target_input,
        line_idx,
    } = decode_node_id(pos.node)?
    else {
        return None;
    };
    if target_input != input_id || line_idx >= lines.len() {
        return None;
    }
    let mut offset = 0usize;
    for line in &lines[..line_idx] {
        offset += line.chars().count();
    }
    let line_len = lines[line_idx].chars().count();
    Some(offset + pos.character_index.min(line_len))
}

impl Host {
    /// Builds the full AccessKit snapshot of the current UI tree into `out` and returns the resolved focus `NodeId`.
    fn snapshot_a11y(
        &self,
        title: &str,
        size: [f32; 2],
        scale: f32,
        out: &mut HashMap<NodeId, AkNode>,
    ) -> NodeId {
        let mut window = AkNode::new(Role::Window);
        if !title.is_empty() {
            window.set_label(title);
        }
        window.set_bounds(AkRect::new(
            0.,
            0.,
            f64::from(size[0].max(0.)),
            f64::from(size[1].max(0.)),
        ));
        if scale.is_finite() && scale > 0. && (scale - 1.).abs() > 1e-4 {
            window.set_transform(Affine::scale(f64::from(scale)));
        }

        if let Some(root) = self.tree.root
            && self.emit_a11y_node(root, out)
        {
            window.set_children(vec![node_id(root)]);
        }
        out.insert(WINDOW_NODE_ID, window);

        // AccessKit requires `focus` to always name a node present in the tree (falling back to WINDOW_NODE_ID).
        match self.focused_node().map(node_id) {
            Some(fid) if out.contains_key(&fid) => fid,
            _ => WINDOW_NODE_ID,
        }
    }

    fn emit_a11y_node(&self, id: u16, out: &mut HashMap<NodeId, AkNode>) -> bool {
        let Some(node) = self.tree.nodes.get(&id) else {
            return false;
        };
        if !self.row_can_receive(id) {
            return false;
        }
        let paint = node.paint();
        let a11y = paint.and_then(|p| p.a11y.as_deref());
        if a11y.is_some_and(|a| a.hidden) {
            return false;
        }
        if paint.is_some_and(|p| p.opacity.is_some_and(|o| o <= 0.001)) {
            return false;
        }
        // Hide anchored fixed overlays whose anchor is missing or off-screen (when rendered).
        if let Some(style) = node.style()
            && style.position == Position::Fixed
            && let Some(anchor) = &style.anchor
        {
            if self.tree.nodes.get(&anchor.node).is_none() {
                return false;
            }
            if !self.screen.is_empty() && self.screen_rect(id).is_none() {
                return false;
            }
        }

        let role = infer_role(node, a11y);
        let mut ak = AkNode::new(role);

        let rect = self.screen_rect(id).unwrap_or_else(|| self.tree.rect(id));
        ak.set_bounds(ak_rect(rect));

        // Structural children first (so we know which descendant Text/Label nodes were actually emitted).
        let mut child_ids: Vec<NodeId> = Vec::new();
        match node {
            Node::Input {
                text,
                selection,
                placeholder,
                purpose,
                ..
            } => {
                let (raw_text, sel) = if self.focused_input() == Some(id) {
                    self.a11y_focused_editor_text()
                } else {
                    (text.clone(), *selection)
                };
                let display = if *purpose == InputPurpose::Password {
                    mask_text(&raw_text)
                } else {
                    raw_text
                };
                let lines = split_text_runs(&display);
                let content = self.input_content(id);
                let line_h = if lines.is_empty() {
                    content[3]
                } else {
                    (content[3] / lines.len() as f32).max(16.)
                };
                for (line_idx, line) in lines.iter().enumerate() {
                    let run_id = text_run_id(id, line_idx);
                    let mut run = AkNode::new(Role::TextRun);
                    run.set_value(*line);
                    run.set_text_direction(TextDirection::LeftToRight);
                    let char_lens: Vec<u8> =
                        line.chars().map(|c| c.len_utf8() as u8).collect();
                    run.set_character_lengths(char_lens);
                    let words = word_starts_for_line(line);
                    if !words.is_empty() {
                        run.set_word_starts(words);
                    }
                    let y = content[1] + line_idx as f32 * line_h;
                    run.set_bounds(ak_rect([content[0], y, content[2], line_h]));
                    out.insert(run_id, run);
                    child_ids.push(run_id);
                }
                ak.set_value(display.as_str());
                let anchor = global_char_to_position(id, &lines, sel[0] as usize);
                let focus = global_char_to_position(id, &lines, sel[1] as usize);
                ak.set_text_selection(TextSelection { anchor, focus });
                if !placeholder.is_empty() {
                    ak.set_placeholder(placeholder.as_str());
                }
            }
            Node::List { .. } if self.a11y_virtual_rows(id).is_some() => {
                if let Some(rows) = self.a11y_virtual_rows(id) {
                    for &row in rows {
                        if self.emit_a11y_node(row, out) {
                            child_ids.push(node_id(row));
                        }
                    }
                }
            }
            Node::Column { .. } | Node::Box { .. } | Node::List { .. } => {
                for &c in self.tree.children(id) {
                    if self.emit_a11y_node(c, out) {
                        child_ids.push(node_id(c));
                    }
                }
            }
            _ => {}
        }
        if !child_ids.is_empty() {
            ak.set_children(child_ids);
        }

        // Value for text/label nodes (explicit `a11y.value` overrides below).
        match node {
            Node::Text { text, spans, .. } => {
                if let Some(spans) = spans.as_deref().filter(|s| !s.is_empty()) {
                    ak.set_value(plain_text(spans));
                } else {
                    ak.set_value(text.as_str());
                }
            }
            Node::Label { value } => {
                ak.set_value(value.to_string());
            }
            _ => {}
        }

        // Accessible label: explicit `a11y.label`, or auto `labelled_by` descendant text for controls.
        if let Some(label) = a11y.and_then(|a| a.label.as_deref()) {
            ak.set_label(label);
        } else if role_uses_child_labels(role) {
            let mut label_nodes = Vec::new();
            self.collect_child_label_ids(id, out, &mut label_nodes);
            if !label_nodes.is_empty() {
                ak.set_labelled_by(label_nodes);
            }
        }

        // Explicit `A11y` metadata properties.
        if let Some(a) = a11y {
            if let Some(desc) = &a.description {
                ak.set_description(desc.as_str());
            }
            if let Some(val) = &a.value {
                ak.set_value(val.as_str());
            }
            if let Some(checked) = a.checked {
                ak.set_toggled(if checked {
                    Toggled::True
                } else {
                    Toggled::False
                });
            }
            if let Some(selected) = a.selected {
                ak.set_selected(selected);
            }
            if let Some(expanded) = a.expanded {
                ak.set_expanded(expanded);
            }
            if a.modal {
                ak.set_modal();
            }
            if let Some(level) = a.level
                && level > 0
            {
                // AccessKit's `level` is 0-based (ARIA `aria-level` is 1-based).
                ak.set_level(usize::from(level - 1));
            }
            if let Some(live) = a.live {
                ak.set_live(map_live(live));
            }
            if let Some(num) = a.numeric_value {
                ak.set_numeric_value(f64::from(num));
            }
            if let Some(min) = a.min_value {
                ak.set_min_numeric_value(f64::from(min));
            }
            if let Some(max) = a.max_value {
                ak.set_max_numeric_value(f64::from(max));
            }
        }

        // Disabled, clipping, and scroll ranges.
        if matches!(node, Node::Box { disabled: true, .. }) {
            ak.set_disabled();
        }
        if node.style().is_some_and(|s| s.clip || s.scroll) {
            ak.set_clips_children();
        }
        if let Some((offset, max)) = self.a11y_scroll_range(id) {
            ak.set_scroll_y(offset);
            ak.set_scroll_y_min(0.);
            ak.set_scroll_y_max(max);
            if max > 0. {
                ak.add_action(Action::ScrollUp);
                ak.add_action(Action::ScrollDown);
                ak.add_action(Action::SetScrollOffset);
            }
        }

        // Supported AccessKit actions.
        ak.add_action(Action::SetSequentialFocusNavigationStartingPoint);
        if self.is_node_focusable(id) {
            ak.add_action(Action::Focus);
            ak.add_action(Action::Blur);
            ak.add_action(Action::ScrollIntoView);
        }
        match node {
            Node::Box {
                press,
                disabled,
                contextmenu,
                ..
            } => {
                if !disabled && *press != 0 {
                    ak.add_action(Action::Click);
                }
                if *contextmenu != 0 {
                    ak.add_action(Action::ShowContextMenu);
                }
            }
            Node::Button { callback } if *callback != 0 => {
                ak.add_action(Action::Click);
            }
            Node::Input { .. } => {
                ak.add_action(Action::Click);
                ak.add_action(Action::SetValue);
                ak.add_action(Action::ReplaceSelectedText);
                ak.add_action(Action::SetTextSelection);
            }
            _ => {}
        }

        out.insert(node_id(id), ak);
        true
    }

    fn collect_child_label_ids(
        &self,
        id: u16,
        emitted: &HashMap<NodeId, AkNode>,
        out: &mut Vec<NodeId>,
    ) {
        for &c in self.tree.children(id) {
            let cid = node_id(c);
            let Some(ak) = emitted.get(&cid) else {
                continue;
            };
            match self.tree.nodes.get(&c) {
                Some(Node::Text { .. } | Node::Label { .. }) if ak.role() == Role::Label => {
                    out.push(cid);
                }
                Some(Node::Box { press: 0, .. } | Node::Column { .. })
                    if !self.is_node_focusable(c) =>
                {
                    self.collect_child_label_ids(c, emitted, out);
                }
                _ => {}
            }
        }
    }

    /// Generates a full AccessKit `TreeUpdate` (for `ActivationHandler::request_initial_tree` or
    /// `WindowEvent::InitialTreeRequested`), resetting the incremental diff cache.
    pub fn a11y_initial_tree(&mut self, title: &str, size: [f32; 2], scale: f32) -> TreeUpdate {
        let mut current = HashMap::new();
        let focus = self.snapshot_a11y(title, size, scale, &mut current);
        let nodes: Vec<(NodeId, AkNode)> = current
            .iter()
            .map(|(&id, node)| (id, node.clone()))
            .collect();
        self.a11y_cache.initialized = true;
        self.a11y_cache.nodes = current;
        self.a11y_cache.focus = Some(focus);
        let mut info = TreeInfo::new(WINDOW_NODE_ID);
        info.toolkit_name = Some("JSNIR".into());
        info.toolkit_version = Some(env!("CARGO_PKG_VERSION").into());
        TreeUpdate {
            nodes,
            tree: Some(info),
            tree_id: TreeId::ROOT,
            focus,
        }
    }

    /// Computes a minimal incremental AccessKit `TreeUpdate` against the previously published snapshot.
    /// Returns `None` when nothing in the tree or focus changed.
    /// If the cache has not been initialized yet, returns a full initial tree.
    pub fn a11y_update(&mut self, title: &str, size: [f32; 2], scale: f32) -> Option<TreeUpdate> {
        if !self.a11y_cache.initialized {
            return Some(self.a11y_initial_tree(title, size, scale));
        }
        let mut current = HashMap::new();
        let focus = self.snapshot_a11y(title, size, scale, &mut current);
        let mut diff = Vec::new();
        for (&id, node) in &current {
            if self.a11y_cache.nodes.get(&id) != Some(node) {
                diff.push((id, node.clone()));
            }
        }
        if diff.is_empty() && self.a11y_cache.focus == Some(focus) {
            return None;
        }
        self.a11y_cache.nodes = current;
        self.a11y_cache.focus = Some(focus);
        Some(TreeUpdate {
            nodes: diff,
            tree: None,
            tree_id: TreeId::ROOT,
            focus,
        })
    }

    /// Drops the cached AccessKit snapshot when assistive technologies disconnect.
    pub fn a11y_deactivate(&mut self) {
        self.a11y_cache.reset();
    }

    /// Executes an AccessKit `ActionRequest` against this host. Returns `true` if the host state changed.
    pub fn handle_a11y_action(&mut self, request: ActionRequest) -> bool {
        if request.target_tree != TreeId::ROOT {
            return false;
        }
        let Some(target) = decode_node_id(request.target_node) else {
            return false;
        };
        let target_node = match target {
            Target::Window => {
                return match request.action {
                    Action::Focus => {
                        self.focus(None);
                        true
                    }
                    _ => false,
                };
            }
            Target::Node(id) => id,
            Target::InputTextRun { input_id, .. } => input_id,
        };
        if self.tree.nodes.get(&target_node).is_none() || !self.row_can_receive(target_node) {
            return false;
        }

        match request.action {
            Action::Focus => {
                if !self.is_node_focusable(target_node) {
                    return false;
                }
                self.focus(Some(target_node));
                self.scroll_node_into_view(target_node);
                true
            }
            Action::Blur => {
                if self.focused == Some(target_node) {
                    self.focus(None);
                    true
                } else {
                    false
                }
            }
            Action::Click => self.activate_node(target_node),
            Action::ShowContextMenu => {
                if let Some(Node::Box { contextmenu, .. }) = self.tree.nodes.get(&target_node)
                    && *contextmenu != 0
                {
                    let rect = self
                        .screen_rect(target_node)
                        .unwrap_or_else(|| self.tree.rect(target_node));
                    let x = rect[0] + rect[2] / 2.;
                    let y = rect[1] + rect[3] / 2.;
                    let cb = *contextmenu;
                    self.send_a11y_context_menu(cb, x, y);
                    true
                } else {
                    false
                }
            }
            Action::SetValue => {
                let Some(ActionData::Value(val)) = request.data else {
                    return false;
                };
                if !matches!(self.tree.nodes.get(&target_node), Some(Node::Input { .. })) {
                    return false;
                }
                self.focus(Some(target_node));
                if val.is_empty() {
                    self.edit(&[
                        EditorEvent::SelectAll,
                        EditorEvent::Delete {
                            backward: true,
                            word: false,
                        },
                    ]);
                } else {
                    self.edit(&[EditorEvent::SelectAll, EditorEvent::Insert(val.into())]);
                }
                true
            }
            Action::ReplaceSelectedText => {
                let Some(ActionData::Value(val)) = request.data else {
                    return false;
                };
                if !matches!(self.tree.nodes.get(&target_node), Some(Node::Input { .. })) {
                    return false;
                }
                self.focus(Some(target_node));
                if val.is_empty() {
                    self.edit(&[EditorEvent::Delete {
                        backward: true,
                        word: false,
                    }]);
                } else {
                    self.edit(&[EditorEvent::Insert(val.into())]);
                }
                true
            }
            Action::SetTextSelection => {
                let Some(ActionData::SetTextSelection(sel)) = request.data else {
                    return false;
                };
                if !matches!(self.tree.nodes.get(&target_node), Some(Node::Input { .. })) {
                    return false;
                }
                self.focus(Some(target_node));
                let (raw, _) = self.a11y_focused_editor_text();
                let lines = split_text_runs(&raw);
                let Some(a) = position_to_global_char(target_node, &lines, sel.anchor) else {
                    return false;
                };
                let Some(f) = position_to_global_char(target_node, &lines, sel.focus) else {
                    return false;
                };
                self.set_focused_selection([a as u32, f as u32])
            }
            Action::ScrollUp | Action::ScrollDown => {
                let dir = if request.action == Action::ScrollUp {
                    -1.0_f32
                } else {
                    1.0_f32
                };
                let unit = match request.data {
                    Some(ActionData::ScrollUnit(u)) => u,
                    _ => ScrollUnit::Item,
                };
                self.scroll_node_by(target_node, dir, unit == ScrollUnit::Page)
            }
            Action::SetScrollOffset => {
                let Some(ActionData::SetScrollOffset(pt)) = request.data else {
                    return false;
                };
                self.set_node_scroll_offset(target_node, pt.y as f32)
            }
            Action::ScrollIntoView => self.scroll_node_into_view(target_node),
            Action::SetSequentialFocusNavigationStartingPoint => {
                self.set_sequential_focus_start(target_node);
                true
            }
            _ => false,
        }
    }
}

#[cfg(all(test, feature = "engine-quickjs"))]
mod tests {
    use super::*;
    use crate::{host::Item, text::TextEngine};
    use accesskit_consumer::{NodeRef, Tree as ConsumerTree, TreeChangeHandler};
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct Counter {
        added: usize,
        updated: usize,
        removed: usize,
        focus_moves: usize,
    }
    impl TreeChangeHandler for Counter {
        fn node_added(&mut self, _: &NodeRef) {
            self.added += 1;
        }
        fn node_updated(&mut self, _: &NodeRef, _: &NodeRef) {
            self.updated += 1;
        }
        fn focus_moved(&mut self, _: Option<&NodeRef>, _: Option<&NodeRef>) {
            self.focus_moves += 1;
        }
        fn node_removed(&mut self, _: &NodeRef) {
            self.removed += 1;
        }
    }

    fn drain(host: &mut Host) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while host.busy() {
            assert!(Instant::now() < deadline, "host did not settle");
            host.poll();
            std::thread::yield_now();
        }
    }

    #[test]
    fn a11y_tree_validates_in_accesskit_consumer_with_incremental_diffs_and_id_recycling() {
        let script = r#"
            function jsnirMount() {
                jsnir.batch(JSON.stringify({ ops: [
                    { op: 'create', id: 1, node: { kind: 'box', style: { width: 400, height: 300 } } },
                    { op: 'create', id: 2, parent: 1, node: {
                        kind: 'text',
                        text: 'Preferences',
                        style: { width: 200, height: 24 },
                        paint: { a11y: { role: 'heading', level: 1 } }
                    } },
                    { op: 'create', id: 3, parent: 1, node: {
                        kind: 'box',
                        press: 1,
                        style: { width: 100, height: 32 }
                    } },
                    { op: 'create', id: 4, parent: 3, node: {
                        kind: 'text',
                        text: 'Save',
                        style: { width: 60, height: 20 }
                    } },
                    { op: 'create', id: 5, parent: 1, node: {
                        kind: 'box',
                        press: 2,
                        style: { width: 32, height: 32 },
                        paint: { a11y: { role: 'switch', label: 'Dark mode', checked: true } }
                    } },
                    { op: 'create', id: 6, parent: 1, node: {
                        kind: 'box',
                        press: 3,
                        disabled: true,
                        style: { width: 80, height: 32 },
                        paint: { a11y: { label: 'Disabled' } }
                    } },
                    { op: 'create', id: 7, parent: 1, node: {
                        kind: 'box',
                        style: { width: 80, height: 32 },
                        paint: { a11y: { hidden: true } }
                    } },
                    { op: 'create', id: 8, parent: 7, node: {
                        kind: 'text',
                        text: 'Secret decorative text'
                    } },
                    { op: 'create', id: 9, parent: 1, node: {
                        kind: 'input',
                        text: 'secret',
                        purpose: 'password',
                        placeholder: 'Password',
                        style: { width: 200, height: 32 },
                        paint: { a11y: { label: 'Account password' } }
                    } },
                    { op: 'create', id: 10, parent: 1, node: {
                        kind: 'input',
                        text: 'hola 茶\nmundo',
                        multiline: true,
                        style: { width: 200, height: 64 },
                        paint: { a11y: { label: 'Notes' } }
                    } },
                    { op: 'create', id: 11, parent: 1, node: {
                        kind: 'text',
                        text: 'Ready',
                        paint: { a11y: { role: 'status', live: 'polite' } }
                    } }
                ] }));
            }
            function jsnirEvent(_cb, text) {
                if (text === 'status') {
                    jsnir.batch(JSON.stringify({ ops: [
                        { op: 'settext', id: 11, text: 'Saved!' }
                    ] }));
                } else if (text === 'recycle') {
                    // Remove node 5 and immediately recycle ID 5 under root.
                    jsnir.batch(JSON.stringify({ ops: [
                        { op: 'remove', id: 5 },
                        { op: 'create', id: 5, parent: 1, node: {
                            kind: 'box',
                            press: 2,
                            style: { width: 40, height: 32 },
                            paint: { a11y: { role: 'checkBox', label: 'Auto-save', checked: false } }
                        } }
                    ] }));
                }
            }
        "#;
        let mut host = Host::new(script.into(), "{}".into(), TextEngine::default());
        drain(&mut host);
        let _ = host.frame(400., 300., 2., 4096);

        let initial = host.a11y_initial_tree("Settings", [400., 300.], 2.);
        let mut consumer = ConsumerTree::new(initial, true);

        let state = consumer.state();
        let root = state.root();
        assert_eq!(root.role(), Role::Window);
        assert_eq!(root.label().as_deref(), Some("Settings"));

        let heading = state
            .node_by_tree_local_id(node_id(2), TreeId::ROOT)
            .expect("heading node");
        assert_eq!(heading.role(), Role::Heading);
        assert_eq!(heading.value().as_deref(), Some("Preferences"));
        assert_eq!(heading.level(), Some(0)); // 0-based = ARIA H1

        let save_btn = state
            .node_by_tree_local_id(node_id(3), TreeId::ROOT)
            .expect("save button");
        assert_eq!(save_btn.role(), Role::Button);
        assert_eq!(
            save_btn.label().as_deref(),
            Some("Save"),
            "Button without explicit a11y.label must compute label from child Text via labelled_by"
        );
        let include_all = |_: &NodeRef| accesskit_consumer::FilterResult::Include;
        assert!(save_btn.supports_action(Action::Click, &include_all));
        assert!(save_btn.supports_action(Action::Focus, &include_all));
        // Bounding box must be scaled to physical pixels by window transform (scale=2.0).
        let bbox = save_btn.bounding_box().expect("save button bbox");
        assert!((bbox.width() - 200.).abs() < 1., "expected 2x physical width, got {bbox:?}");

        let switch = state
            .node_by_tree_local_id(node_id(5), TreeId::ROOT)
            .expect("switch");
        assert_eq!(switch.role(), Role::Switch);
        assert_eq!(switch.label().as_deref(), Some("Dark mode"));
        assert_eq!(switch.toggled(), Some(Toggled::True));

        let disabled = state
            .node_by_tree_local_id(node_id(6), TreeId::ROOT)
            .expect("disabled button");
        assert!(disabled.is_disabled());
        assert!(!disabled.supports_action(Action::Click, &include_all));
        assert!(!disabled.supports_action(Action::Focus, &include_all));

        // Hidden subtree (7 and descendant 8) must be excluded from the AccessKit tree.
        assert!(state.node_by_tree_local_id(node_id(7), TreeId::ROOT).is_none());
        assert!(state.node_by_tree_local_id(node_id(8), TreeId::ROOT).is_none());

        // Password input must expose masked glyphs only, never plaintext.
        let pwd = state
            .node_by_tree_local_id(node_id(9), TreeId::ROOT)
            .expect("password input");
        assert_eq!(pwd.role(), Role::PasswordInput);
        assert_eq!(pwd.label().as_deref(), Some("Account password"));
        assert_eq!(pwd.value().as_deref(), Some("••••••"));

        // Multiline UTF-8 input exposes 2 TextRuns and supports consumer text ranges.
        let notes = state
            .node_by_tree_local_id(node_id(10), TreeId::ROOT)
            .expect("multiline notes input");
        assert_eq!(notes.role(), Role::MultilineTextInput);
        assert!(notes.supports_text_ranges());
        let doc = notes.document_range();
        assert_eq!(doc.text(), "hola 茶\nmundo");

        // No-op update must return None (zero wasted AT-SPI/UIA updates).
        assert!(host.a11y_update("Settings", [400., 300.], 2.).is_none());

        // Pure text update without layout shift must emit ONLY the single changed node.
        host.push("status".into());
        drain(&mut host);
        let _ = host.frame(400., 300., 2., 4096);
        let text_diff = host
            .a11y_update("Settings", [400., 300.], 2.)
            .expect("expected single-node text diff");
        assert!(text_diff.tree.is_none(), "incremental update omits TreeInfo");
        assert_eq!(
            text_diff.nodes.len(),
            1,
            "single text update must emit only 1 changed node, got {:?}",
            text_diff.nodes.iter().map(|(id, _)| id).collect::<Vec<_>>()
        );
        let mut counts = Counter::default();
        consumer.update_and_process_changes(text_diff, &mut counts);
        assert_eq!(counts.updated, 1);

        let status = consumer
            .state()
            .node_by_tree_local_id(node_id(11), TreeId::ROOT)
            .unwrap();
        assert_eq!(status.value().as_deref(), Some("Saved!"));

        // Remove + immediate ID recycling must validate cleanly in accesskit_consumer and skip unchanged siblings.
        host.push("recycle".into());
        drain(&mut host);
        let _ = host.frame(400., 300., 2., 4096);
        let diff = host
            .a11y_update("Settings", [400., 300.], 2.)
            .expect("expected recycle diff");
        assert!(diff.tree.is_none());
        assert!(
            !diff.nodes.iter().any(|(id, _)| *id == WINDOW_NODE_ID || *id == node_id(2) || *id == node_id(3)),
            "unchanged window/heading/button above removed node should not be re-emitted"
        );
        consumer.update_and_process_changes(diff, &mut counts);

        let recycled = consumer
            .state()
            .node_by_tree_local_id(node_id(5), TreeId::ROOT)
            .unwrap();
        assert_eq!(recycled.role(), Role::CheckBox);
        assert_eq!(recycled.label().as_deref(), Some("Auto-save"));
        assert_eq!(recycled.toggled(), Some(Toggled::False));
    }

    #[test]
    fn a11y_actions_drive_focus_click_edit_selection_scroll_and_context_menu() {
        let script = r#"
            var log = [];
            function record(s) {
                log.push(s);
                jsnir.batch(JSON.stringify({ ops: [
                    { op: 'settext', id: 2, text: log.join('|') }
                ] }));
            }
            function jsnirMount() {
                jsnir.batch(JSON.stringify({ ops: [
                    { op: 'create', id: 1, node: { kind: 'box', style: { width: 320, height: 240 } } },
                    { op: 'create', id: 2, parent: 1, node: { kind: 'text', text: '' } },
                    { op: 'create', id: 3, parent: 1, node: {
                        kind: 'box',
                        press: 1,
                        contextmenu: 2,
                        style: { width: 100, height: 32 },
                        paint: { a11y: { label: 'Run' } }
                    } },
                    { op: 'create', id: 4, parent: 1, node: {
                        kind: 'input',
                        text: 'initial',
                        multiline: true,
                        change: 3,
                        style: { width: 240, height: 60 },
                        paint: { a11y: { label: 'Editor' } }
                    } },
                    { op: 'create', id: 5, parent: 1, node: {
                        kind: 'box',
                        style: { width: 240, height: 80, scroll: true }
                    } },
                    { op: 'create', id: 6, parent: 5, node: {
                        kind: 'box',
                        press: 4,
                        style: { width: 200, height: 300 },
                        paint: { a11y: { label: 'Tall item' } }
                    } }
                ] }));
            }
            function jsnirEvent(cb, text) {
                if (cb === 1) record('click');
                else if (cb === 3) record('change:' + text);
                else if (cb === 4) record('tall');
            }
            function jsnirContextMenu(cb, x, y) {
                if (cb === 2) record('ctx:' + Math.round(x) + ',' + Math.round(y));
            }
        "#;
        let mut host = Host::new(script.into(), "{}".into(), TextEngine::default());
        drain(&mut host);
        let _ = host.frame(320., 240., 1., 4096);
        let mut consumer = ConsumerTree::new(host.a11y_initial_tree("Actions", [320., 240.], 1.), true);
        let mut counts = Counter::default();

        // 1. Focus a button: focused_node is button 3, IME remains disabled.
        assert!(host.handle_a11y_action(ActionRequest {
            action: Action::Focus,
            target_tree: TreeId::ROOT,
            target_node: node_id(3),
            data: None,
        }));
        assert_eq!(host.focused_node(), Some(3));
        assert!(!host.focused(), "button focus must NOT enable text-field IME");
        assert!(!host.ime_state().allowed);

        // 2. Click and ShowContextMenu via AccessKit.
        assert!(host.handle_a11y_action(ActionRequest {
            action: Action::Click,
            target_tree: TreeId::ROOT,
            target_node: node_id(3),
            data: None,
        }));
        assert!(host.handle_a11y_action(ActionRequest {
            action: Action::ShowContextMenu,
            target_tree: TreeId::ROOT,
            target_node: node_id(3),
            data: None,
        }));
        drain(&mut host);

        // 3. SetValue on multiline UTF-8 input, then SetTextSelection across lines, then ReplaceSelectedText.
        assert!(host.handle_a11y_action(ActionRequest {
            action: Action::SetValue,
            target_tree: TreeId::ROOT,
            target_node: node_id(4),
            data: Some(ActionData::Value("café\nline2".into())),
        }));
        assert_eq!(host.focused_input(), Some(4));
        assert!(host.ime_state().allowed);
        let _ = host.frame(320., 240., 1., 4096);
        if let Some(u) = host.a11y_update("Actions", [320., 240.], 1.) {
            consumer.update_and_process_changes(u, &mut counts);
        }

        // Select "fé\nli" (line 0 index 2 -> line 1 index 2).
        assert!(host.handle_a11y_action(ActionRequest {
            action: Action::SetTextSelection,
            target_tree: TreeId::ROOT,
            target_node: node_id(4),
            data: Some(ActionData::SetTextSelection(TextSelection {
                anchor: TextPosition {
                    node: text_run_id(4, 0),
                    character_index: 2,
                },
                focus: TextPosition {
                    node: text_run_id(4, 1),
                    character_index: 2,
                },
            })),
        }));
        let _ = host.frame(320., 240., 1., 4096);
        if let Some(u) = host.a11y_update("Actions", [320., 240.], 1.) {
            consumer.update_and_process_changes(u, &mut counts);
        }
        let ed_node = consumer
            .state()
            .node_by_tree_local_id(node_id(4), TreeId::ROOT)
            .unwrap();
        let sel_range = ed_node.text_selection().expect("selection range");
        assert_eq!(sel_range.text(), "fé\nli");

        assert!(host.handle_a11y_action(ActionRequest {
            action: Action::ReplaceSelectedText,
            target_tree: TreeId::ROOT,
            target_node: node_id(4),
            data: Some(ActionData::Value("XX".into())),
        }));
        drain(&mut host);
        assert_eq!(host.a11y_focused_editor_text().0, "caXXne2");

        // 4. Scroll actions on scrollable container 5.
        assert!(host.handle_a11y_action(ActionRequest {
            action: Action::ScrollDown,
            target_tree: TreeId::ROOT,
            target_node: node_id(5),
            data: Some(ActionData::ScrollUnit(ScrollUnit::Page)),
        }));
        let (off1, max) = host.a11y_scroll_range(5).unwrap();
        assert!(off1 > 0. && max > 0., "scroll_y should advance: {off1}/{max}");
        assert!(host.handle_a11y_action(ActionRequest {
            action: Action::SetScrollOffset,
            target_tree: TreeId::ROOT,
            target_node: node_id(5),
            data: Some(ActionData::SetScrollOffset(accesskit::Point::new(0., 120.))),
        }));
        assert!((host.a11y_scroll_range(5).unwrap().0 - 120.).abs() < 0.5);

        // 5. Sequential focus starting point + Tab advances to next focusable control.
        assert!(host.handle_a11y_action(ActionRequest {
            action: Action::Blur,
            target_tree: TreeId::ROOT,
            target_node: node_id(4),
            data: None,
        }));
        assert_eq!(host.focused_node(), None);
        assert!(host.handle_a11y_action(ActionRequest {
            action: Action::SetSequentialFocusNavigationStartingPoint,
            target_tree: TreeId::ROOT,
            target_node: node_id(3),
            data: None,
        }));
        assert_eq!(host.focus_next(), Some(4));

        // 6. Deactivate resets cache so next a11y_update sends a full TreeUpdate with TreeInfo.
        host.a11y_deactivate();
        let rebuilt = host
            .a11y_update("Actions", [320., 240.], 1.)
            .expect("full tree after deactivate");
        assert!(rebuilt.tree.is_some());

        // Verify JS callbacks fired for click, context menu, and edits.
        let log_text = match host.tree.nodes.get(&2) {
            Some(Node::Text { text, .. }) => text.clone(),
            other => panic!("expected log text, got {other:?}"),
        };
        assert!(log_text.contains("click"), "{log_text}");
        assert!(log_text.contains("ctx:"), "{log_text}");
        assert!(log_text.contains("change:caXXne2"), "{log_text}");
    }

    #[test]
    fn tab_navigation_traps_modal_skips_disabled_and_activates_focused_button() {
        let script = r#"
            var clicks = 0;
            function jsnirMount() {
                jsnir.batch(JSON.stringify({ ops: [
                    { op: 'create', id: 1, node: { kind: 'box', style: { width: 300, height: 200 } } },
                    { op: 'create', id: 2, parent: 1, node: {
                        kind: 'input',
                        text: 'a',
                        style: { width: 120, height: 28 }
                    } },
                    { op: 'create', id: 3, parent: 1, node: {
                        kind: 'box',
                        press: 1,
                        style: { width: 100, height: 28 },
                        paint: {
                            background: 0x222222ff,
                            focus: { background: 0x00ff00ff }
                        }
                    } },
                    { op: 'create', id: 4, parent: 1, node: {
                        kind: 'box',
                        press: 2,
                        disabled: true,
                        style: { width: 100, height: 28 }
                    } },
                    { op: 'create', id: 5, parent: 1, node: {
                        kind: 'box',
                        press: 3,
                        style: { width: 100, height: 28 },
                        paint: { a11y: { focusable: false } }
                    } },
                    { op: 'create', id: 6, parent: 1, node: {
                        kind: 'text',
                        text: '0'
                    } }
                ] }));
            }
            function jsnirEvent(cb, text) {
                if (cb === 1) {
                    clicks++;
                    jsnir.batch(JSON.stringify({ ops: [
                        { op: 'settext', id: 6, text: String(clicks) }
                    ] }));
                } else if (text === 'open-modal') {
                    jsnir.batch(JSON.stringify({ ops: [
                        { op: 'create', id: 10, parent: 1, node: {
                            kind: 'box',
                            style: { position: 'fixed', left: 20, top: 20, width: 200, height: 120, layer: 10 },
                            paint: { a11y: { role: 'dialog', label: 'Confirm', modal: true } }
                        } },
                        { op: 'create', id: 11, parent: 10, node: {
                            kind: 'box',
                            press: 1,
                            style: { width: 80, height: 28 },
                            paint: { a11y: { label: 'OK' } }
                        } },
                        { op: 'create', id: 12, parent: 10, node: {
                            kind: 'box',
                            press: 1,
                            style: { width: 80, height: 28 },
                            paint: { a11y: { label: 'Cancel' } }
                        } }
                    ] }));
                } else if (text === 'close-modal') {
                    jsnir.batch(JSON.stringify({ ops: [
                        { op: 'remove', id: 10 }
                    ] }));
                }
            }
        "#;
        let mut host = Host::new(script.into(), "{}".into(), TextEngine::default());
        drain(&mut host);
        let _ = host.frame(300., 200., 1., 4096);

        // Tab order skips disabled (4) and focusable:false (5).
        assert_eq!(host.focus_order(), vec![2, 3]);
        assert_eq!(host.focus_next(), Some(2));
        assert!(host.focused());
        assert_eq!(host.focus_next(), Some(3));
        assert_eq!(host.focused_node(), Some(3));
        assert!(!host.focused(), "focused Box must NOT activate text-input IME");

        // `paint.focus` variant must render on focused Box button (green background 0x00ff00ff).
        let frame = host.frame(300., 200., 1., 4096);
        assert!(
            frame
                .items
                .iter()
                .any(|item| matches!(item, Item::Fill { top: [0, 255, 0, 255], .. })),
            "focused Box must apply paint.focus variant"
        );

        // Enter/Space activation fires press callback on focused button 3.
        assert!(host.activate_focused());
        drain(&mut host);
        assert_eq!(
            match host.tree.nodes.get(&6) {
                Some(Node::Text { text, .. }) => text.as_str(),
                _ => "",
            },
            "1"
        );

        // Wrapping around: next -> 2, prev -> 3.
        assert_eq!(host.focus_next(), Some(2));
        assert_eq!(host.focus_prev(), Some(3));

        // Open modal dialog: Tab & Shift+Tab trap strictly inside modal [11, 12].
        host.push("open-modal".into());
        drain(&mut host);
        let _ = host.frame(300., 200., 1., 4096);
        assert_eq!(host.focus_order(), vec![11, 12]);
        assert_eq!(host.focus_next(), Some(11));
        assert_eq!(host.focus_next(), Some(12));
        assert_eq!(host.focus_next(), Some(11));
        assert_eq!(host.focus_prev(), Some(12));

        // Close modal: focus drops cleanly and main document Tab order returns [2, 3].
        host.push("close-modal".into());
        drain(&mut host);
        let _ = host.frame(300., 200., 1., 4096);
        assert_eq!(host.focused_node(), None);
        assert_eq!(host.focus_order(), vec![2, 3]);
    }
}
