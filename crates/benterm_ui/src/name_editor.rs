//! A field for typing text: one line, such as a workspace's name, or
//! several, such as what a task's agent is to do.
//!
//! GPUI has no text field of its own. This one takes text from the input
//! method, so that Japanese can be typed, and has a caret that the arrows
//! and mouse clicks move, a selection made with Shift, and the keys of the
//! Edit menu: select all, copy, cut, paste, undo and redo. A single-line
//! field cuts a long line at its ends, never at the caret; a multiline
//! field wraps long lines to the field's width and moves between lines
//! with the Up and Down arrows.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use gpui::prelude::FluentBuilder;
use gpui::{
    App, Bounds, Context, ElementInputHandler, EntityInputHandler, EventEmitter, FocusHandle, Font,
    InteractiveElement, IntoElement, KeyBinding, MouseButton, MouseDownEvent, ParentElement,
    Pixels, Point, Render, ShapedLine, Styled, TextRun, UTF16Selection, Window, actions, canvas,
    div, px,
};

use benterm_core::Rgb;
use benterm_layout::ends_a_line;

use crate::menu::{Cut, Redo, SelectAll, Undo};
use crate::terminal_view::{Copy, hsla};
use crate::theme::{CommandPaletteTheme, SearchTheme, SidebarTheme, TabTheme};

actions!(
    name_editor,
    [
        ConfirmName,
        CancelName,
        /// Takes the last character away, or the whole name while it is
        /// still selected.
        DeleteBackward,
        PasteName,
        /// Takes the text as typed, also in a field of several lines,
        /// where Enter starts a new line.
        ConfirmLines,
        MoveLeft,
        MoveRight,
        /// Moves the caret one line in a multiline field.
        MoveUp,
        MoveDown,
        SelectLeft,
        SelectRight,
        /// Moves the caret one line and extends the selection in a multiline field.
        SelectUp,
        SelectDown,
        MoveToStart,
        MoveToEnd,
        SelectToStart,
        SelectToEnd,
        /// Takes the character after the caret away, or the selection.
        DeleteForward,
    ]
);

/// The key context of the field, which its bindings are limited to.
const KEY_CONTEXT: &str = "NameEditor";

/// Binds the keys of the field. While the input method is converting, it
/// gets these keys first.
pub fn bind_keys(cx: &mut App) {
    let context = Some(KEY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("enter", ConfirmName, context),
        KeyBinding::new("escape", CancelName, context),
        KeyBinding::new("backspace", DeleteBackward, context),
        KeyBinding::new("cmd-v", PasteName, context),
        KeyBinding::new("cmd-enter", ConfirmLines, context),
        KeyBinding::new("left", MoveLeft, context),
        KeyBinding::new("right", MoveRight, context),
        KeyBinding::new("up", MoveUp, context),
        KeyBinding::new("down", MoveDown, context),
        KeyBinding::new("shift-left", SelectLeft, context),
        KeyBinding::new("shift-right", SelectRight, context),
        KeyBinding::new("shift-up", SelectUp, context),
        KeyBinding::new("shift-down", SelectDown, context),
        KeyBinding::new("cmd-left", MoveToStart, context),
        KeyBinding::new("cmd-right", MoveToEnd, context),
        KeyBinding::new("home", MoveToStart, context),
        KeyBinding::new("end", MoveToEnd, context),
        KeyBinding::new("cmd-shift-left", SelectToStart, context),
        KeyBinding::new("cmd-shift-right", SelectToEnd, context),
        KeyBinding::new("delete", DeleteForward, context),
        // The keys of the Edit menu, which the menu sends too.
        KeyBinding::new("cmd-a", SelectAll, context),
        KeyBinding::new("cmd-c", Copy, context),
        KeyBinding::new("cmd-x", Cut, context),
        KeyBinding::new("cmd-z", Undo, context),
        KeyBinding::new("cmd-shift-z", Redo, context),
    ]);
}

/// What the field tells the view it is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NameEditorEvent {
    /// Enter was pressed with this text in the field.
    Confirmed(String),
    /// Escape was pressed.
    Cancelled,
}

/// The text of the field, where the caret and the selection are in it,
/// and what the input method is doing with it. Offsets are in bytes; the
/// input method counts in UTF-16.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NameText {
    text: String,
    /// Where the selection began, and where the caret is: the same place
    /// when nothing is selected.
    anchor: usize,
    head: usize,
    /// The part the input method is still converting.
    marked: Option<Range<usize>>,
    /// The selection the input method named inside that part.
    marked_selection: Option<Range<usize>>,
    /// Whether line breaks are kept. A name is one line.
    multiline: bool,
    /// What the field held before each edit, the latest last, and what
    /// was undone, for doing it again.
    undone: Vec<Snapshot>,
    done: Vec<Snapshot>,
}

/// The text and the selection at one time.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Snapshot {
    text: String,
    anchor: usize,
    head: usize,
}

/// How many edits are kept for undoing.
const HISTORY: usize = 100;

impl NameText {
    /// `text` with all of it selected, so that typing replaces it.
    pub fn new(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            anchor: 0,
            head: text.len(),
            ..Self::default()
        }
    }

    pub fn keeps_lines(&self) -> bool {
        self.multiline
    }

    /// A field of several lines, which keeps its line breaks.
    pub fn lines(text: &str) -> Self {
        Self {
            multiline: true,
            ..Self::new(text)
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// The part the input method is converting, in bytes.
    pub fn marked(&self) -> Option<Range<usize>> {
        self.marked.clone()
    }

    /// Whether the whole text is selected, as it is when the editing
    /// starts.
    pub fn all_selected(&self) -> bool {
        !self.text.is_empty() && self.selection() == (0..self.text.len())
    }

    /// The selected part, in bytes: empty at the caret when nothing is
    /// selected.
    pub fn selection(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    /// Where the caret is, in bytes.
    pub fn caret(&self) -> usize {
        self.head
    }

    pub fn selected_text(&self) -> &str {
        &self.text[self.selection()]
    }

    fn utf16_len(&self) -> usize {
        self.text.encode_utf16().count()
    }

    fn byte_offset(&self, utf16: usize) -> usize {
        byte_offset(&self.text, utf16)
    }

    fn utf16_offset(&self, byte: usize) -> usize {
        self.text[..byte].encode_utf16().count()
    }

    fn bytes(&self, utf16: Range<usize>) -> Range<usize> {
        let start = self.byte_offset(utf16.start);
        start..self.byte_offset(utf16.end).max(start)
    }

    fn utf16(&self, bytes: Range<usize>) -> Range<usize> {
        self.utf16_offset(bytes.start)..self.utf16_offset(bytes.end)
    }

    /// The text in a UTF-16 range, and the range it was really taken from.
    pub fn text_in_range_utf16(&self, range: Range<usize>) -> (Range<usize>, String) {
        let bytes = self.bytes(range);
        (self.utf16(bytes.clone()), self.text[bytes].to_owned())
    }

    /// The selection: the one the input method named while it converts,
    /// else the selected part, which is empty at the caret.
    pub fn selected_range_utf16(&self) -> Range<usize> {
        if let Some(selection) = self.marked_selection.clone() {
            return self.utf16(selection);
        }
        debug_assert!(self.head <= self.utf16_len().max(self.text.len()));
        self.utf16(self.selection())
    }

    pub fn marked_range_utf16(&self) -> Option<Range<usize>> {
        self.marked.clone().map(|marked| self.utf16(marked))
    }

    /// What new text takes the place of: the range the input method names,
    /// else what it is converting, else the selection.
    fn target(&self, range_utf16: Option<Range<usize>>) -> Range<usize> {
        range_utf16
            .map(|range| self.bytes(range))
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selection())
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text.clone(),
            anchor: self.anchor,
            head: self.head,
        }
    }

    /// Keeps the field as it is for undoing, before an edit. What the
    /// input method does while it converts is one edit with what it
    /// commits.
    fn remember(&mut self) {
        if self.marked.is_some() {
            return;
        }
        self.undone.push(self.snapshot());
        if self.undone.len() > HISTORY {
            self.undone.remove(0);
        }
        self.done.clear();
    }

    /// Puts `new` in place of the target and returns where it went, with
    /// the caret after it. In a name, which is one line, characters that
    /// end a line are left out.
    fn put(&mut self, range_utf16: Option<Range<usize>>, new: &str) -> Range<usize> {
        let target = self.target(range_utf16);
        let multiline = self.multiline;
        let new: String = new.chars().filter(|ch| kept(*ch, multiline)).collect();
        // Putting nothing in place of nothing is no edit to undo.
        if !(target.is_empty() && new.is_empty()) {
            self.remember();
        }
        let placed = target.start..target.start + new.len();
        self.text.replace_range(target, &new);
        self.anchor = placed.end;
        self.head = placed.end;
        self.marked_selection = None;
        placed
    }

    /// Text is typed, pasted, or committed by the input method.
    pub fn replace(&mut self, range_utf16: Option<Range<usize>>, new: &str) {
        self.put(range_utf16, new);
        self.marked = None;
    }

    /// The input method shows `new` as what it is converting, with
    /// `selected_utf16` selected in it, counted from the start of `new`.
    /// Without a selection the caret is at the end of `new`.
    pub fn replace_and_mark(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new: &str,
        selected_utf16: Option<Range<usize>>,
    ) {
        let placed = self.put(range_utf16, new);
        if placed.is_empty() {
            self.marked = None;
            return;
        }
        // The input method counts in `new` as it gave it, which may hold
        // characters that were left out.
        let selection = match selected_utf16 {
            Some(selected) => {
                let multiline = self.multiline;
                let start = placed.start + kept_bytes_before(new, selected.start, multiline);
                let end = placed.start + kept_bytes_before(new, selected.end, multiline);
                start..end.max(start)
            }
            None => placed.end..placed.end,
        };
        self.marked_selection = Some(selection);
        self.marked = Some(placed);
    }

    /// The input method is done converting; the text stays.
    pub fn unmark(&mut self) {
        self.marked = None;
        self.marked_selection = None;
    }

    /// The start of the character before `at`, and of the one after it.
    fn before(&self, at: usize) -> usize {
        self.text[..at]
            .char_indices()
            .next_back()
            .map_or(0, |(start, _)| start)
    }

    fn after(&self, at: usize) -> usize {
        self.text[at..]
            .chars()
            .next()
            .map_or(self.text.len(), |ch| at + ch.len_utf8())
    }

    /// Puts the caret at `to`, and the start of the selection too unless
    /// the selection is being made longer or shorter.
    fn move_to(&mut self, to: usize, extend: bool) {
        self.head = to;
        if !extend {
            self.anchor = to;
        }
        self.marked = None;
        self.marked_selection = None;
    }

    /// Left: one character, or to the start of the selection.
    pub fn move_left(&mut self, extend: bool) {
        let selection = self.selection();
        let to = if !extend && !selection.is_empty() {
            selection.start
        } else {
            self.before(self.head)
        };
        self.move_to(to, extend);
    }

    /// Right: one character, or to the end of the selection.
    pub fn move_right(&mut self, extend: bool) {
        let selection = self.selection();
        let to = if !extend && !selection.is_empty() {
            selection.end
        } else {
            self.after(self.head)
        };
        self.move_to(to, extend);
    }

    /// Puts the caret at `to`, clamped to a character boundary inside the text.
    pub fn set_caret(&mut self, to: usize, extend: bool) {
        let mut clamped = to.min(self.text.len());
        while clamped > 0 && !self.text.is_char_boundary(clamped) {
            clamped -= 1;
        }
        self.move_to(clamped, extend);
    }

    /// Moves the caret to the same character column of the previous line,
    /// or to the start of the text if already on the first line.
    pub fn move_up(&mut self, extend: bool) {
        let line_start = self.text[..self.head].rfind('\n').map_or(0, |i| i + 1);
        if line_start == 0 {
            self.move_to(0, extend);
            return;
        }
        let col = self.text[line_start..self.head].chars().count();
        let prev_end = line_start - 1;
        let prev_start = self.text[..prev_end].rfind('\n').map_or(0, |i| i + 1);
        let to = prev_start + nth_char_byte(&self.text[prev_start..prev_end], col);
        self.move_to(to, extend);
    }

    /// Moves the caret to the same character column of the next line,
    /// or to the end of the text if already on the last line.
    pub fn move_down(&mut self, extend: bool) {
        let line_start = self.text[..self.head].rfind('\n').map_or(0, |i| i + 1);
        let col = self.text[line_start..self.head].chars().count();
        let line_end = self.text[self.head..]
            .find('\n')
            .map_or(self.text.len(), |i| self.head + i);
        if line_end == self.text.len() {
            self.move_to(self.text.len(), extend);
            return;
        }
        let next_start = line_end + 1;
        let next_end = self.text[next_start..]
            .find('\n')
            .map_or(self.text.len(), |i| next_start + i);
        let to = next_start + nth_char_byte(&self.text[next_start..next_end], col);
        self.move_to(to, extend);
    }

    pub fn move_to_start(&mut self, extend: bool) {
        self.move_to(0, extend);
    }

    pub fn move_to_end(&mut self, extend: bool) {
        self.move_to(self.text.len(), extend);
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.move_to(self.text.len(), true);
    }

    /// Takes `range` out, with the caret where it was.
    fn delete(&mut self, range: Range<usize>) {
        if range.is_empty() {
            return;
        }
        self.remember();
        self.text.replace_range(range.clone(), "");
        self.move_to(range.start, false);
    }

    /// Backspace: the selection goes, else the character before the
    /// caret.
    pub fn delete_backward(&mut self) {
        let selection = self.selection();
        if selection.is_empty() {
            self.delete(self.before(self.head)..self.head);
        } else {
            self.delete(selection);
        }
        self.marked = None;
        self.marked_selection = None;
    }

    /// Delete: the selection goes, else the character after the caret.
    pub fn delete_forward(&mut self) {
        let selection = self.selection();
        if selection.is_empty() {
            self.delete(self.head..self.after(self.head));
        } else {
            self.delete(selection);
        }
    }

    /// Takes the selection out and returns it, for the clipboard.
    pub fn cut(&mut self) -> String {
        let selection = self.selection();
        let cut = self.text[selection.clone()].to_owned();
        self.delete(selection);
        cut
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.text = snapshot.text;
        self.anchor = snapshot.anchor;
        self.head = snapshot.head;
        self.marked = None;
        self.marked_selection = None;
    }

    /// Takes the last edit back.
    pub fn undo(&mut self) {
        if let Some(before) = self.undone.pop() {
            self.done.push(self.snapshot());
            self.restore(before);
        }
    }

    /// Makes the edit that was last taken back again.
    pub fn redo(&mut self) {
        if let Some(after) = self.done.pop() {
            self.undone.push(self.snapshot());
            self.restore(after);
        }
    }
}

/// The byte offset of the `nth` character in `line`, or `line.len()` if
/// `line` has fewer than `nth` characters.
fn nth_char_byte(line: &str, nth: usize) -> usize {
    line.char_indices()
        .nth(nth)
        .map_or(line.len(), |(offset, _)| offset)
}

/// Splits a single hard line (without `\n`) into visual line byte ranges
/// that fit within `max_width` pixels, using `x_at(byte_offset)` as the
/// cumulative x-advance from the start of `line`.
///
/// Prefers breaking after whitespace when a space falls within the line;
/// otherwise breaks at the last character that fits, always advancing by at
/// least one character.
pub fn wrap_line(line: &str, max_width: f32, x_at: impl Fn(usize) -> f32) -> Vec<Range<usize>> {
    if line.is_empty() || max_width <= 0. || x_at(line.len()) <= max_width {
        return std::iter::once(0..line.len()).collect();
    }
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < line.len() {
        let origin = x_at(start);
        if x_at(line.len()) - origin <= max_width {
            ranges.push(start..line.len());
            break;
        }
        let mut last_fit = start;
        let mut last_space_after = None;
        for (rel, ch) in line[start..].char_indices() {
            let end = start + rel + ch.len_utf8();
            if x_at(end) - origin > max_width {
                break;
            }
            last_fit = end;
            if ch.is_whitespace() && end < line.len() {
                last_space_after = Some(end);
            }
        }
        let chosen = if last_fit == start {
            // Even a single character is wider than `max_width`; advance one char.
            let ch = line[start..].chars().next().unwrap();
            start + ch.len_utf8()
        } else {
            last_space_after.unwrap_or(last_fit)
        };
        ranges.push(start..chosen);
        start = chosen;
    }
    ranges
}

/// The index of the visual line that owns `caret`. At a soft-wrap boundary
/// where `lines[i].end == lines[i + 1].start`, prefers the start of the next
/// visual line.
fn caret_visual_line(lines: &[Range<usize>], caret: usize) -> usize {
    for (index, range) in lines.iter().enumerate().rev() {
        if range.start <= caret && caret <= range.end {
            return index;
        }
    }
    0
}

/// Whether a character stays in a field: in one of a single line none
/// that ends a line does, and one of several lines keeps the line feed.
fn kept(ch: char, multiline: bool) -> bool {
    !ends_a_line(ch) || (multiline && ch == '\n')
}

/// How many bytes of `text` before a UTF-16 offset stay in a field. An
/// offset inside a character counts as its start.
fn kept_bytes_before(text: &str, utf16: usize, multiline: bool) -> usize {
    let before = &text[..byte_offset(text, utf16)];
    before
        .chars()
        .filter(|ch| kept(*ch, multiline))
        .map(char::len_utf8)
        .sum()
}

/// The byte offset in `text` of a UTF-16 offset, moved back to the start
/// of a character if it is inside one, and to the end if it is past it.
fn byte_offset(text: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (offset, ch) in text.char_indices() {
        if units + ch.len_utf16() > utf16 {
            return offset;
        }
        units += ch.len_utf16();
    }
    text.len()
}

/// How the field looks, taken from the theme of what it is in.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldTheme {
    pub text: Rgb,
    /// The line under the field, and the background of selected text.
    pub accent: Rgb,
    /// Selected text, over the accent.
    pub selected_text: Rgb,
    /// What an empty field says it is for.
    pub placeholder: Rgb,
    /// In pixels.
    pub font_size: f32,
}

impl From<SidebarTheme> for FieldTheme {
    fn from(theme: SidebarTheme) -> Self {
        Self {
            text: theme.item_text,
            accent: theme.item_active_indicator,
            selected_text: theme.background,
            placeholder: theme.item_secondary_text,
            font_size: theme.font_size,
        }
    }
}

impl From<TabTheme> for FieldTheme {
    fn from(theme: TabTheme) -> Self {
        Self {
            text: theme.active_text,
            accent: theme.active_indicator,
            selected_text: theme.active_background,
            placeholder: theme.secondary_text,
            font_size: theme.font_size,
        }
    }
}

impl From<CommandPaletteTheme> for FieldTheme {
    fn from(theme: CommandPaletteTheme) -> Self {
        Self {
            text: theme.text,
            accent: theme.accent,
            selected_text: theme.input_background,
            placeholder: theme.secondary_text,
            font_size: theme.font_size,
        }
    }
}

impl From<SearchTheme> for FieldTheme {
    fn from(theme: SearchTheme) -> Self {
        Self {
            text: theme.text,
            accent: theme.accent,
            selected_text: theme.input_background,
            placeholder: theme.secondary_text,
            font_size: theme.font_size,
        }
    }
}

/// One laid-out visual line of a [`NameEditor`], kept for mouse hit-testing
/// and visual Up/Down navigation.
struct VisualLine {
    range: Range<usize>,
    shaped: ShapedLine,
    scroll_x: Pixels,
}

/// Cached layout from the most recent prepaint of a [`NameEditor`].
#[derive(Default)]
struct EditorLayout {
    bounds: Option<Bounds<Pixels>>,
    font: Option<Font>,
    line_height: Pixels,
    lines: Vec<VisualLine>,
}

/// The field as a view.
pub struct NameEditor {
    name: NameText,
    focus: FocusHandle,
    theme: FieldTheme,
    /// Shown while the field is empty.
    placeholder: Option<String>,
    /// Whether a line is drawn under the field. A field in a box of its
    /// own has none.
    underline: bool,
    /// Layout from the last prepaint, used for click hit-testing, wrapping,
    /// and visual Up/Down movement.
    layout: Rc<RefCell<EditorLayout>>,
}

impl NameEditor {
    /// A field holding `text`, all of it selected.
    pub fn new(text: &str, theme: impl Into<FieldTheme>, cx: &mut Context<Self>) -> Self {
        Self {
            name: NameText::new(text),
            focus: cx.focus_handle(),
            theme: theme.into(),
            placeholder: None,
            underline: true,
            layout: Rc::new(RefCell::new(EditorLayout::default())),
        }
    }

    /// A field of several lines holding `text`: Enter starts a new line
    /// there, and Cmd-Enter takes the text.
    pub fn lines(text: &str, theme: impl Into<FieldTheme>, cx: &mut Context<Self>) -> Self {
        Self {
            name: NameText::lines(text),
            ..Self::new(text, theme, cx)
        }
    }

    /// The field without the line under it, for one drawn in a box.
    pub fn without_underline(mut self) -> Self {
        self.underline = false;
        self
    }

    /// The field with `text` shown in it while it is empty.
    pub fn with_placeholder(mut self, text: &str) -> Self {
        self.placeholder = Some(text.to_owned());
        self
    }

    /// Puts `text` in the field, all of it selected.
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.name = NameText::new(text);
        cx.notify();
    }

    /// Empties the field.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.name = NameText::new("");
        cx.notify();
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// What is in the field now.
    pub fn text(&self) -> &str {
        self.name.text()
    }

    pub fn set_theme(&mut self, theme: impl Into<FieldTheme>, cx: &mut Context<Self>) {
        self.theme = theme.into();
        cx.notify();
    }

    /// Whether the input method is converting. The keys of the field then
    /// belong to it, even if one of them gets here.
    pub(crate) fn is_converting(&self) -> bool {
        self.converting()
    }

    fn converting(&self) -> bool {
        self.name.marked().is_some()
    }

    fn confirm(&mut self, _: &ConfirmName, _: &mut Window, cx: &mut Context<Self>) {
        if self.converting() {
            return;
        }
        if self.name.keeps_lines() {
            self.name.replace(None, "\n");
            cx.notify();
        } else {
            cx.emit(NameEditorEvent::Confirmed(self.name.text().to_owned()));
        }
    }

    fn confirm_lines(&mut self, _: &ConfirmLines, _: &mut Window, cx: &mut Context<Self>) {
        // In a field of one line the key is not the field's: what the
        // field is in may have a use for it, as the diff has.
        if !self.name.keeps_lines() {
            cx.propagate();
        } else if !self.converting() {
            cx.emit(NameEditorEvent::Confirmed(self.name.text().to_owned()));
        }
    }

    /// Changes the text or the selection with `change`, unless the input
    /// method is converting, whose keys these then are.
    fn edit(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut NameText)) {
        if !self.converting() {
            change(&mut self.name);
            cx.notify();
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let selected = self.name.selected_text();
        if !selected.is_empty() {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(selected.to_owned()));
        }
    }

    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if self.converting() {
            return;
        }
        let cut = self.name.cut();
        if !cut.is_empty() {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(cut));
            cx.notify();
        }
    }

    fn cancel(&mut self, _: &CancelName, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(NameEditorEvent::Cancelled);
    }

    fn delete_backward(&mut self, _: &DeleteBackward, _: &mut Window, cx: &mut Context<Self>) {
        if !self.converting() {
            self.name.delete_backward();
            cx.notify();
        }
    }

    fn paste(&mut self, _: &PasteName, _: &mut Window, cx: &mut Context<Self>) {
        if self.converting() {
            return;
        }
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.name.replace(None, &text);
            cx.notify();
        }
    }
}

impl EventEmitter<NameEditorEvent> for NameEditor {}

impl EntityInputHandler for NameEditor {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let (range, text) = self.name.text_in_range_utf16(range);
        *adjusted_range = Some(range);
        Some(text)
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.name.selected_range_utf16(),
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.name.marked_range_utf16()
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.name.unmark();
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.name.replace(range, text);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.name.replace_and_mark(range, text, selected);
        cx.notify();
    }

    /// Where the candidate window goes: under the field.
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        field: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(field)
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl NameEditor {
    /// Places the caret at the character boundary closest to the mouse click.
    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        if self.converting() {
            return;
        }
        let offset = {
            let layout = self.layout.borrow();
            let Some(bounds) = layout.bounds else {
                return;
            };
            if layout.lines.is_empty() {
                return;
            }
            let row = if layout.line_height > px(0.) {
                let rel_y = f32::from(event.position.y - bounds.origin.y);
                let step = f32::from(layout.line_height);
                (rel_y / step).floor().max(0.) as usize
            } else {
                0
            }
            .min(layout.lines.len() - 1);
            let line = &layout.lines[row];
            let local_x = (event.position.x - bounds.origin.x + line.scroll_x).max(px(0.));
            let in_line = line.shaped.closest_index_for_x(local_x);
            line.range.start + in_line
        };
        self.name.set_caret(offset, event.modifiers.shift);
        cx.notify();
    }

    /// Moves the caret up or down one visual line in a multiline field.
    /// Single-line fields propagate the key so parent views (such as the
    /// command palette) keep Up/Down navigation.
    fn move_vertical(&mut self, down: bool, extend: bool, cx: &mut Context<Self>) {
        if !self.name.keeps_lines() {
            cx.propagate();
            return;
        }
        if self.converting() {
            return;
        }
        let target = {
            let layout = self.layout.borrow();
            if layout.lines.len() > 1 {
                let ranges: Vec<Range<usize>> =
                    layout.lines.iter().map(|line| line.range.clone()).collect();
                let current = caret_visual_line(&ranges, self.name.caret());
                let caret_in_line = self
                    .name
                    .caret()
                    .saturating_sub(layout.lines[current].range.start);
                let caret_x = layout.lines[current].shaped.x_for_index(caret_in_line);
                if down {
                    if current + 1 < layout.lines.len() {
                        let next = &layout.lines[current + 1];
                        Some(next.range.start + next.shaped.closest_index_for_x(caret_x))
                    } else {
                        Some(self.name.text().len())
                    }
                } else if current > 0 {
                    let prev = &layout.lines[current - 1];
                    Some(prev.range.start + prev.shaped.closest_index_for_x(caret_x))
                } else {
                    Some(0)
                }
            } else {
                None
            }
        };
        match target {
            Some(offset) => self.name.set_caret(offset, extend),
            None if down => self.name.move_down(extend),
            None => self.name.move_up(extend),
        }
        cx.notify();
    }

    /// Computes visual line byte ranges in `self.name.text()`.
    /// In multiline mode, wraps hard lines to the field's laid-out width.
    fn visual_ranges(&self, window: &Window) -> Vec<Range<usize>> {
        let text = self.name.text();
        let (wrap_width, font) = {
            let layout = self.layout.borrow();
            (
                layout.bounds.map(|b| f32::from(b.size.width) - 1.),
                layout.font.clone(),
            )
        };
        let font_size = px(self.theme.font_size);
        let color = hsla(self.theme.text);
        let mut ranges = Vec::new();
        let mut start = 0;
        for hard_line in text.split('\n') {
            if self.name.keeps_lines()
                && !hard_line.is_empty()
                && let (Some(max_w), Some(font)) = (wrap_width, font.as_ref())
                && max_w > 0.
            {
                let run = TextRun {
                    len: hard_line.len(),
                    font: font.clone(),
                    color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let shaped = window.text_system().shape_line(
                    hard_line.to_owned().into(),
                    font_size,
                    &[run],
                    None,
                );
                for sub in wrap_line(hard_line, max_w, |i| f32::from(shaped.x_for_index(i))) {
                    ranges.push(start + sub.start..start + sub.end);
                }
            } else {
                ranges.push(start..start + hard_line.len());
            }
            start += hard_line.len() + 1;
        }
        ranges
    }

    /// One line of the field, from `line.start` to `line.end` of the
    /// text: what is selected on the accent, what the input method
    /// converts underlined, and the caret where it is, if `here`.
    fn line(&self, index: usize, line: Range<usize>, here: bool) -> gpui::Div {
        let theme = &self.theme;
        let text = self.name.text();
        let selection = self.name.selection();
        let marked = self.name.marked().unwrap_or(0..0);
        let caret = self.name.caret();
        let mut cuts = vec![
            line.start,
            line.end,
            selection.start,
            selection.end,
            marked.start,
            marked.end,
            caret,
        ];
        cuts.retain(|cut| line.contains(cut) || *cut == line.end);
        cuts.sort_unstable();
        cuts.dedup();
        let piece = |part: Range<usize>| {
            let selected = selection.start <= part.start && part.end <= selection.end;
            let converting = marked.start <= part.start && part.end <= marked.end;
            div()
                .flex_none()
                .when(selected, |piece| {
                    piece
                        .bg(hsla(theme.accent))
                        .text_color(hsla(theme.selected_text))
                })
                // What is being converted is underlined.
                .when(converting, |piece| {
                    piece.border_b_1().border_color(hsla(theme.text))
                })
                .child(text[part].to_owned())
        };
        let pieces = |within: Range<usize>| {
            cuts.windows(2)
                .map(|pair| pair[0]..pair[1])
                .filter(|part| within.start <= part.start && part.end <= within.end)
                .map(piece)
                .collect::<Vec<_>>()
        };
        let split = if here { caret } else { line.end };
        let multiline = self.name.keeps_lines();
        let row = div()
            .debug_selector(move || format!("name-editor-line-{index}"))
            .flex()
            .items_center()
            .min_h(px((theme.font_size * 1.4).ceil()))
            .child(
                div()
                    .flex()
                    .when(!multiline, |left| left.justify_end())
                    .min_w_0()
                    .overflow_hidden()
                    .children(pieces(line.start..split)),
            );
        if !here {
            return row;
        }
        row.child(
            div()
                .debug_selector(|| "name-editor-caret".into())
                .flex_none()
                .w(px(1.))
                .h(px(theme.font_size + 2.))
                .bg(hsla(theme.text)),
        )
        .child(
            div()
                .flex()
                .min_w_0()
                .overflow_hidden()
                .children(pieces(split..line.end)),
        )
    }
}

impl Render for NameEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let text = self.name.text().to_owned();
        // The caret shows where typing goes: only in the field that has
        // the keyboard.
        let focused = self.focus.is_focused(window);
        let visual_ranges = self.visual_ranges(window);
        let caret_line = caret_visual_line(&visual_ranges, self.name.caret());
        let mut lines: Vec<gpui::Div> = visual_ranges
            .iter()
            .enumerate()
            .map(|(index, range)| self.line(index, range.clone(), focused && index == caret_line))
            .collect();
        // An empty field says what it is for, after the caret.
        if let Some(placeholder) = self.placeholder.as_ref().filter(|_| text.is_empty())
            && let Some(first) = lines.pop()
        {
            lines.push(
                first.child(
                    div()
                        .flex_none()
                        .text_color(hsla(theme.placeholder))
                        .child(placeholder.clone()),
                ),
            );
        }
        let focus = self.focus.clone();
        let view = cx.entity();
        let view_for_wrap = view.clone();
        let layout = self.layout.clone();
        let font_size = px(theme.font_size);
        let line_height = px((theme.font_size * 1.4).ceil());
        let color = hsla(theme.text);
        let multiline = self.name.keeps_lines();
        let caret = self.name.caret();
        div()
            .id("name-editor")
            .debug_selector(|| "name-editor".into())
            .track_focus(&self.focus)
            .key_context(KEY_CONTEXT)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::confirm_lines))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::delete_backward))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(
                cx.listener(|this, _: &MoveLeft, _, cx| {
                    this.edit(cx, |name| name.move_left(false))
                }),
            )
            .on_action(cx.listener(|this, _: &MoveRight, _, cx| {
                this.edit(cx, |name| name.move_right(false))
            }))
            .on_action(cx.listener(|this, _: &MoveUp, _, cx| this.move_vertical(false, false, cx)))
            .on_action(cx.listener(|this, _: &MoveDown, _, cx| this.move_vertical(true, false, cx)))
            .on_action(
                cx.listener(|this, _: &SelectLeft, _, cx| {
                    this.edit(cx, |name| name.move_left(true))
                }),
            )
            .on_action(cx.listener(|this, _: &SelectRight, _, cx| {
                this.edit(cx, |name| name.move_right(true))
            }))
            .on_action(cx.listener(|this, _: &SelectUp, _, cx| this.move_vertical(false, true, cx)))
            .on_action(
                cx.listener(|this, _: &SelectDown, _, cx| this.move_vertical(true, true, cx)),
            )
            .on_action(cx.listener(|this, _: &MoveToStart, _, cx| {
                this.edit(cx, |name| name.move_to_start(false))
            }))
            .on_action(cx.listener(|this, _: &MoveToEnd, _, cx| {
                this.edit(cx, |name| name.move_to_end(false))
            }))
            .on_action(cx.listener(|this, _: &SelectToStart, _, cx| {
                this.edit(cx, |name| name.move_to_start(true))
            }))
            .on_action(cx.listener(|this, _: &SelectToEnd, _, cx| {
                this.edit(cx, |name| name.move_to_end(true))
            }))
            .on_action(
                cx.listener(|this, _: &DeleteForward, _, cx| {
                    this.edit(cx, NameText::delete_forward)
                }),
            )
            .on_action(
                cx.listener(|this, _: &SelectAll, _, cx| this.edit(cx, NameText::select_all)),
            )
            .on_action(cx.listener(|this, _: &Undo, _, cx| this.edit(cx, NameText::undo)))
            .on_action(cx.listener(|this, _: &Redo, _, cx| this.edit(cx, NameText::redo)))
            .relative()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_color(hsla(theme.text))
            .when(self.underline, |field| {
                field.border_b_1().border_color(hsla(theme.accent))
            })
            .children(lines)
            .child(
                canvas(
                    move |bounds, window, cx| {
                        let font = window.text_style().font();
                        let shaped_lines: Vec<VisualLine> = visual_ranges
                            .iter()
                            .map(|range| {
                                let slice = &text[range.clone()];
                                let run = TextRun {
                                    len: slice.len(),
                                    font: font.clone(),
                                    color,
                                    background_color: None,
                                    underline: None,
                                    strikethrough: None,
                                };
                                let shaped = window.text_system().shape_line(
                                    slice.to_owned().into(),
                                    font_size,
                                    &[run],
                                    None,
                                );
                                let scroll_x = if !multiline && focused {
                                    let caret_in =
                                        caret.saturating_sub(range.start).min(slice.len());
                                    let caret_x = shaped.x_for_index(caret_in);
                                    let avail = (bounds.size.width - px(1.)).max(px(0.));
                                    (caret_x - avail).max(px(0.))
                                } else {
                                    px(0.)
                                };
                                VisualLine {
                                    range: range.clone(),
                                    shaped,
                                    scroll_x,
                                }
                            })
                            .collect();
                        let needs_rewrap = {
                            let mut state = layout.borrow_mut();
                            let width_changed = state.bounds.is_none_or(|prev| {
                                (f32::from(prev.size.width) - f32::from(bounds.size.width)).abs()
                                    > 0.5
                            });
                            let font_changed = state.font.as_ref() != Some(&font);
                            state.bounds = Some(bounds);
                            state.font = Some(font);
                            state.line_height = line_height;
                            state.lines = shaped_lines;
                            multiline && (width_changed || font_changed)
                        };
                        if needs_rewrap {
                            let view = view_for_wrap.clone();
                            window.defer(cx, move |_, cx| {
                                view.update(cx, |_, cx| cx.notify());
                            });
                        }
                    },
                    move |bounds, (), window, cx| {
                        // Text input goes to the field while it has the
                        // focus.
                        let handler = ElementInputHandler::new(bounds, view);
                        window.handle_input(&focus, handler, cx);
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
    }
}
