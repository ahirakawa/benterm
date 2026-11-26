//! The diff of the selected workspace's task, opened from the tabs above
//! the panes: the files its worktree has changed since the task branched
//! off, and the lines of the chosen one.
//!
//! The diff is read by git on a background thread, never on the thread
//! that draws, and read again whenever a file of the worktree changes
//! while the diff is shown. The lines are drawn only as far as they are on
//! screen, so a diff of ten thousand lines costs a frame what a short one
//! does.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use benterm_task::{Diff, FileDiff, FileStatus, Git, LineKind, NEW_FILE_LIMIT, Task, TaskError};
use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use futures::channel::oneshot;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, FontWeight,
    InteractiveElement, IntoElement, KeyBinding, MouseButton, MouseDownEvent, ParentElement,
    Render, ScrollHandle, ScrollStrategy, SharedString, StatefulInteractiveElement, Styled,
    Subscription, UniformListScrollHandle, Window, actions, div, px, uniform_list,
};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use benterm_core::Rgb;

use crate::name_editor::{FieldTheme, NameEditor, NameEditorEvent};
use crate::review::{Lines, Outgoing, Readiness, Review};
use crate::terminal_view::hsla;
use crate::theme::{DiffTheme, Theme};

actions!(
    diff_view,
    [
        SelectNextChangedFile,
        SelectPreviousChangedFile,
        /// Gives the keyboard back to the panes.
        LeaveDiff,
        /// Selects the next line that can take a comment.
        SelectNextLine,
        /// Selects the line above that can take a comment.
        SelectPreviousLine,
        ExtendSelectionDown,
        ExtendSelectionUp,
        CommentOnSelection,
        /// Sends the comments not sent yet to the task's agent.
        SendReview,
        PickerNext,
        PickerPrevious,
    ]
);

/// The key context of the view, which its keys are limited to.
const KEY_CONTEXT: &str = "DiffView";
/// The key context of the open file picker popover.
const PICKER_CONTEXT: &str = "DiffFilePicker";

/// The width of the list of files, as in the mock.
const HEADING_PADDING: f32 = 14.;
/// The width of each column of line numbers, as in the mock.
const NUMBER_WIDTH: f32 = 44.;
/// The width of the `+` and `-` column.
const MARK_WIDTH: f32 = 16.;
/// A row is this many times the font size, as in the mock.
const ROW_HEIGHT: f32 = 1.6;
/// The least width of the field for a comment, which the label of its
/// lines gives way to.
const FIELD_MIN_WIDTH: f32 = 160.;
/// How long the worktree must go without changes before the diff is read
/// again, so that a build or a checkout is read once and not per file.
const SETTLE: Duration = Duration::from_millis(100);

/// Binds the keys of the view while it has the keyboard.
pub fn bind_keys(cx: &mut App) {
    let context = Some(KEY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("down", SelectNextChangedFile, context),
        KeyBinding::new("up", SelectPreviousChangedFile, context),
        KeyBinding::new("escape", LeaveDiff, context),
        KeyBinding::new("alt-down", SelectNextLine, context),
        KeyBinding::new("alt-up", SelectPreviousLine, context),
        KeyBinding::new("alt-shift-down", ExtendSelectionDown, context),
        KeyBinding::new("alt-shift-up", ExtendSelectionUp, context),
        KeyBinding::new("enter", CommentOnSelection, context),
        KeyBinding::new("cmd-enter", SendReview, context),
        KeyBinding::new("down", PickerNext, Some(PICKER_CONTEXT)),
        KeyBinding::new("up", PickerPrevious, Some(PICKER_CONTEXT)),
    ]);
}

/// What the view tells the window.
pub enum DiffEvent {
    /// The keyboard should go back to the panes.
    Leave,
    /// The comments not sent yet on `task`'s diff should go to its agent.
    /// `confirmed` is for after the user was asked and said yes.
    Send { task: u64, confirmed: bool },
}

/// What a field for a comment is for.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Target {
    /// A new comment on these lines.
    New(Lines),
    /// The comment with this id, changed.
    Comment(u64),
}

/// The field a comment is written in.
struct Editing {
    target: Target,
    editor: Entity<NameEditor>,
    _events: Subscription,
}

/// The floating file picker popover, while open.
struct FilePicker {
    editor: Entity<NameEditor>,
    /// Index into the currently matched files.
    selected: usize,
    /// Last query observed, to reset selection on change.
    query: String,
    scroll: ScrollHandle,
    _subscriptions: [Subscription; 2],
}

fn format_limit(bytes: usize) -> String {
    const MIB: usize = 1 << 20;
    const KIB: usize = 1 << 10;
    if bytes >= MIB && bytes.is_multiple_of(MIB) {
        format!("{} MiB", bytes / MIB)
    } else if bytes >= KIB && bytes.is_multiple_of(KIB) {
        format!("{} KiB", bytes / KIB)
    } else {
        format!("{bytes} bytes")
    }
}

/// What a line of the list of rows shows: a row of the diff, a comment
/// under the last of its lines, or the field of the comment written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    Row(usize),
    Comment(u64),
    Editor,
}

/// What a diff compares: a task's worktree as it is now against the
/// commit it branched off from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffSource {
    /// The task's number, which its comments are kept by.
    pub task: u64,
    /// The main checkout of the repository the worktree is of.
    pub repository: PathBuf,
    pub worktree: PathBuf,
    pub base_commit: String,
    /// For the heading: the branch the task branched off from.
    pub base_branch: String,
    /// For the heading: the task's own branch.
    pub branch: String,
}

impl DiffSource {
    pub fn of(task: &Task) -> Self {
        Self {
            task: task.id,
            repository: task.repository.clone(),
            worktree: task.worktree.clone(),
            base_commit: task.base_commit.clone(),
            base_branch: task.base_branch.clone(),
            branch: task.branch.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// The `@@` line in front of a run of changes.
    Hunk,
    Context,
    Added,
    Removed,
    /// Why a file has no lines to show.
    Note,
    /// The line that names a file, in front of its changes, as `git
    /// diff` prints it.
    File,
}

/// A row of the shown file's diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffRow {
    pub kind: RowKind,
    /// The line's number in the commit the task branched off from.
    pub old: Option<u32>,
    /// The line's number in the worktree's file.
    pub new: Option<u32>,
    /// Without the mark in front, and with tabs as spaces.
    pub text: SharedString,
    /// The file's path from the top of the worktree.
    pub path: SharedString,
}

impl DiffRow {
    /// The file and the line in the worktree this row is, for a comment
    /// on it. A removed line is in no file of the worktree, and the `@@`
    /// line is no line at all.
    pub fn line_in_worktree(&self) -> Option<(&SharedString, u32)> {
        match self.kind {
            RowKind::Context | RowKind::Added => self.new.map(|line| (&self.path, line)),
            RowKind::Removed | RowKind::Hunk | RowKind::Note | RowKind::File => None,
        }
    }
}

/// The background of a row, or none for the background of the view.
pub fn row_background(kind: RowKind, theme: &DiffTheme) -> Option<Rgb> {
    match kind {
        RowKind::Added => Some(theme.added_background),
        RowKind::Removed => Some(theme.removed_background),
        RowKind::Hunk => Some(theme.hunk_background),
        RowKind::Context | RowKind::Note | RowKind::File => None,
    }
}

/// The background of a row of `kind` under the mouse, for the rows that
/// take a comment.
pub fn row_hover_background(kind: RowKind, theme: &DiffTheme) -> Option<Rgb> {
    let resting = match kind {
        RowKind::Added => theme.added_background,
        RowKind::Removed => theme.removed_background,
        RowKind::Context => theme.background,
        RowKind::Hunk | RowKind::Note | RowKind::File => return None,
    };
    Some(crate::theme::lifted(resting, theme.text))
}

/// The rows of every file, one after the other as `git diff` prints
/// them: a line that names the file, then its rows.
pub fn all_rows(files: &[FileDiff]) -> Vec<DiffRow> {
    let mut rows = Vec::new();
    for file in files {
        let path = SharedString::from(file.path.clone());
        rows.push(DiffRow {
            kind: RowKind::File,
            old: None,
            new: None,
            text: format!("diff --git a/{0} b/{0}", file.path).into(),
            path,
        });
        rows.extend(rows_of(file));
    }
    rows
}

/// The rows of `file`: each run of changes after its `@@` line.
pub fn rows_of(file: &FileDiff) -> Vec<DiffRow> {
    let path = SharedString::from(file.path.clone());
    let note = |text: String| {
        vec![DiffRow {
            kind: RowKind::Note,
            old: None,
            new: None,
            text: text.into(),
            path: path.clone(),
        }]
    };
    if file.binary {
        return note("A binary file: there are no lines to show.".into());
    }
    if file.too_large {
        return note(format!(
            "A new file over {} MiB: it is not read.",
            NEW_FILE_LIMIT >> 20
        ));
    }
    if file.hunks.is_empty() {
        return note("No lines changed.".into());
    }
    let lines: usize = file.hunks.iter().map(|hunk| hunk.lines.len() + 1).sum();
    let mut rows = Vec::with_capacity(lines);
    for hunk in &file.hunks {
        rows.push(DiffRow {
            kind: RowKind::Hunk,
            old: None,
            new: None,
            text: hunk.header.clone().into(),
            path: path.clone(),
        });
        rows.extend(hunk.lines.iter().map(|line| DiffRow {
            kind: match line.kind {
                LineKind::Context => RowKind::Context,
                LineKind::Added => RowKind::Added,
                LineKind::Removed => RowKind::Removed,
            },
            old: line.old,
            new: line.new,
            text: if line.text.contains('\t') {
                line.text.replace('\t', "    ").into()
            } else {
                line.text.clone().into()
            },
            path: path.clone(),
        }));
    }
    rows
}

/// Calls back after each change in a worktree, once it has settled.
/// Dropping it stops the watching.
struct WorktreeWatcher {
    _watcher: RecommendedWatcher,
}

impl WorktreeWatcher {
    fn start(worktree: &Path) -> notify::Result<(Self, UnboundedReceiver<()>)> {
        let (raw_sender, raw) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(raw_sender)?;
        watcher.watch(worktree, RecursiveMode::Recursive)?;
        let (sender, changes) = unbounded();
        // Ends when the watcher is dropped, which drops the sender of
        // `raw`.
        std::thread::Builder::new()
            .name("benterm-diff-watch".into())
            .spawn(move || {
                while let Ok(event) = raw.recv() {
                    if !changes_the_diff(&event) {
                        continue;
                    }
                    loop {
                        match raw.recv_timeout(SETTLE) {
                            Ok(_) => continue,
                            Err(RecvTimeoutError::Timeout) => break,
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                    }
                    if sender.unbounded_send(()).is_err() {
                        return;
                    }
                }
            })
            .map_err(notify::Error::io)?;
        Ok((Self { _watcher: watcher }, changes))
    }
}

/// Whether a change can change the diff: one to a file of the worktree,
/// not one inside a `.git` folder or a mere read.
fn changes_the_diff(event: &notify::Result<notify::Event>) -> bool {
    let Ok(event) = event else {
        // An overflow or an error: read again to be sure.
        return true;
    };
    if matches!(event.kind, EventKind::Access(_)) {
        return false;
    }
    let in_git = |path: &PathBuf| {
        path.components()
            .any(|part| part == Component::Normal(".git".as_ref()))
    };
    event.paths.is_empty() || !event.paths.iter().all(in_git)
}

/// The watching of a worktree while the diff is shown.
struct Watching {
    _watcher: WorktreeWatcher,
    _reads: gpui::Task<()>,
}

pub struct DiffView {
    theme: Theme,
    git: Git,
    source: Option<DiffSource>,
    shown: bool,
    /// The last diff read for `source`.
    diff: Option<Diff>,
    /// Why the last read failed.
    error: Option<TaskError>,
    selected: usize,
    /// The rows of the selected file.
    rows: Vec<DiffRow>,
    /// The comments on each task's diff, by the task's number. Kept
    /// while another task is shown.
    reviews: HashMap<u64, Review>,
    /// The rows selected for a comment, by index: where the selection
    /// began, and where it ends now.
    selection: Option<(usize, usize)>,
    editing: Option<Editing>,
    /// The floating file picker popover, while open.
    file_picker: Option<FilePicker>,
    /// What is asked or said before the comments are sent.
    asking: Option<Readiness>,
    /// The lines of the list of rows, rows and comments together.
    items: Vec<Item>,
    /// Counts the reads, so that one that ends after a later one began,
    /// or after the source changed, is dropped.
    reads: u64,
    /// Worktrees currently being read by git. The value is whether a
    /// change arrived while the read was running, so it must be read
    /// once more when the current read finishes.
    in_flight: HashMap<PathBuf, bool>,
    watching: Option<Watching>,
    focus: FocusHandle,
    rows_scroll: UniformListScrollHandle,
    /// The installed font of the theme's terminal.
    mono: SharedString,
}

impl EventEmitter<DiffEvent> for DiffView {}

impl DiffView {
    /// A hidden view with nothing to compare.
    pub fn new(theme: Theme, git: Git, cx: &mut Context<Self>) -> Self {
        let mono = crate::theme::installed_mono_font(&theme.terminal, cx);
        Self {
            mono,
            theme,
            git,
            source: None,
            shown: false,
            diff: None,
            error: None,
            selected: 0,
            rows: Vec::new(),
            reviews: HashMap::new(),
            selection: None,
            editing: None,
            file_picker: None,
            asking: None,
            items: Vec::new(),
            reads: 0,
            in_flight: HashMap::new(),
            watching: None,
            focus: cx.focus_handle(),
            rows_scroll: UniformListScrollHandle::new(),
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// Compares `source` from now on, or nothing.
    pub fn set_source(&mut self, source: Option<DiffSource>, cx: &mut Context<Self>) {
        if source == self.source {
            return;
        }
        self.source = source;
        self.diff = None;
        self.error = None;
        self.selected = 0;
        self.rows.clear();
        self.selection = None;
        self.editing = None;
        self.file_picker = None;
        self.asking = None;
        self.place_items();
        self.restart(cx);
        cx.notify();
    }

    /// While shown, the diff follows the worktree; while hidden, nothing
    /// is watched or read, and it is read again when shown.
    pub fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        if shown != self.shown {
            self.shown = shown;
            self.restart(cx);
        }
    }

    /// Whether the diff is shown, and so follows the worktree.
    pub fn is_shown(&self) -> bool {
        self.shown
    }

    pub fn set_theme(&mut self, theme: &Theme, cx: &mut Context<Self>) {
        if &self.theme != theme {
            if self.theme.terminal.font_families != theme.terminal.font_families {
                self.mono = crate::theme::installed_mono_font(&theme.terminal, cx);
            }
            self.theme = theme.clone();
            cx.notify();
        }
    }

    /// The changed files, by path.
    pub fn files(&self) -> &[FileDiff] {
        self.diff.as_ref().map_or(&[], |diff| &diff.files)
    }

    /// How many lines were added and removed, once the diff is read.
    pub fn counts(&self) -> Option<(usize, usize)> {
        self.diff.as_ref().map(Diff::counts)
    }

    pub fn selected_file(&self) -> Option<&FileDiff> {
        self.files().get(self.selected)
    }

    /// The rows of the selected file.
    pub fn rows(&self) -> &[DiffRow] {
        &self.rows
    }

    /// The comments on the diff of the task shown.
    pub fn review(&self) -> Option<&Review> {
        self.reviews.get(&self.source.as_ref()?.task)
    }

    /// The lines selected for a comment.
    pub fn selected_lines(&self) -> Option<Lines> {
        let (anchor, head) = self.selection?;
        Lines::of_rows(self.rows.get(anchor.min(head)..=anchor.max(head))?)
    }

    /// What is asked or said before the comments are sent.
    pub fn asking(&self) -> Option<&Readiness> {
        self.asking.as_ref()
    }

    /// Shows what is asked or said before the comments are sent, or
    /// takes it away.
    pub fn ask(&mut self, asking: Option<Readiness>, cx: &mut Context<Self>) {
        self.asking = asking;
        cx.notify();
    }

    /// The message of the comments on `task`'s diff that are not sent yet.
    pub fn outgoing(&self, task: u64) -> Option<Outgoing> {
        self.reviews.get(&task)?.outgoing()
    }

    /// Marks comments of `task` as sent, after their message went.
    pub fn mark_sent(&mut self, task: u64, ids: &[u64], cx: &mut Context<Self>) {
        if let Some(review) = self.reviews.get_mut(&task) {
            review.mark_sent(ids);
        }
        self.asking = None;
        self.place_items();
        cx.notify();
    }

    fn review_mut(&mut self) -> Option<&mut Review> {
        let task = self.source.as_ref()?.task;
        Some(self.reviews.entry(task).or_default())
    }

    /// Asks the window to send the comments not sent yet.
    fn send(&mut self, confirmed: bool, cx: &mut Context<Self>) {
        let Some(task) = self.source.as_ref().map(|source| source.task) else {
            return;
        };
        if self.outgoing(task).is_some() {
            cx.emit(DiffEvent::Send { task, confirmed });
        }
    }

    /// Selects the row at `index`, or with `extend` the rows from where
    /// the selection began to it, and opens a field for a comment on
    /// them.
    fn click_row(
        &mut self,
        index: usize,
        extend: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A comment is on lines of one file: a selection that would
        // reach into another begins anew at the row.
        let same_file = |a: usize, b: usize| matches!((self.rows.get(a), self.rows.get(b)), (Some(a), Some(b)) if a.path == b.path);
        let anchor = match self.selection {
            Some((anchor, _)) if extend && same_file(anchor, index) => anchor,
            _ => index,
        };
        if Lines::of_rows(
            self.rows
                .get(anchor.min(index)..=anchor.max(index))
                .unwrap_or(&[]),
        )
        .is_none()
        {
            return;
        }
        self.selection = Some((anchor, index));
        // The file of the row is the one the heading names.
        if let Some(file) = self.rows.get(index).map(|row| row.path.clone())
            && let Some(at) = self.files().iter().position(|f| f.path == file.as_ref())
        {
            self.selected = at;
        }
        // The view would take the keyboard back from the field after this.
        window.prevent_default();
        self.comment_on_selection(window, cx);
    }

    /// Moves the end of the selection to the next row in `step`'s way
    /// that can take a comment, and with `extend` keeps where it began.
    fn select_line(&mut self, step: isize, extend: bool, cx: &mut Context<Self>) {
        let takes = |index: usize| Lines::of_rows(&self.rows[index..=index]).is_some();
        // With nothing selected, from the top of the file last gone to.
        let top = self
            .selected_file()
            .and_then(|file| {
                self.rows
                    .iter()
                    .position(|row| row.kind == RowKind::File && row.path.as_ref() == file.path)
            })
            .unwrap_or(0);
        let next = match self.selection {
            None => (top..self.rows.len()).find(|&i| takes(i)),
            Some((_, head)) if step > 0 => (head + 1..self.rows.len()).find(|&i| takes(i)),
            Some((_, head)) => (0..head).rev().find(|&i| takes(i)),
        };
        let Some(next) = next else {
            return;
        };
        let anchor = match self.selection {
            Some((anchor, _)) if extend && self.rows[anchor].path == self.rows[next].path => anchor,
            _ => next,
        };
        self.selection = Some((anchor, next));
        // The heading names the file the selection is in.
        let path = self.rows[next].path.clone();
        if let Some(at) = self.files().iter().position(|f| f.path == path.as_ref()) {
            self.selected = at;
        }
        let lines = self.selected_lines();
        // A comment being written goes with the selection inside its
        // file, and stays where it is when the selection leaves it.
        if let (Some(editing), Some(lines)) = (&mut self.editing, lines)
            && let Target::New(target) = &mut editing.target
            && target.path == lines.path
        {
            *target = lines;
        }
        self.place_items();
        if let Some(item) = self.items.iter().position(|item| *item == Item::Row(next)) {
            self.rows_scroll
                .scroll_to_item(item, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    /// Opens a field for a comment on the selected lines. A field that is
    /// open for a new comment moves to them, with what is typed in it.
    fn comment_on_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(lines) = self.selected_lines() else {
            return;
        };
        match &mut self.editing {
            Some(Editing {
                target: Target::New(target),
                editor,
                ..
            }) => {
                // What was written about one file is not moved to
                // another by a press on a row of that one.
                if target.path != lines.path && !editor.read(cx).text().is_empty() {
                    self.selection = None;
                    self.place_items();
                    cx.notify();
                    return;
                }
                *target = lines;
                let focus = editor.read(cx).focus_handle().clone();
                window.focus(&focus, cx);
            }
            _ => self.open_editor(Target::New(lines), "", window, cx),
        }
        self.place_items();
        self.reveal_editor();
        cx.notify();
    }

    /// Opens the field for `target`, holding `text`, and gives it the
    /// keyboard.
    fn open_editor(
        &mut self,
        target: Target,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let theme = self.field_theme();
        let editor = cx.new(|cx| NameEditor::new(text, theme, cx).with_placeholder("Comment"));
        let events = cx.subscribe_in(&editor, window, |this, _, event, window, cx| match event {
            NameEditorEvent::Confirmed(text) => this.keep_comment(text, window, cx),
            NameEditorEvent::Cancelled => this.close_editor(window, cx),
        });
        let focus = editor.read(cx).focus_handle().clone();
        window.focus(&focus, cx);
        self.editing = Some(Editing {
            target,
            editor,
            _events: events,
        });
        self.place_items();
        self.reveal_editor();
        cx.notify();
    }

    fn field_theme(&self) -> FieldTheme {
        let theme = &self.theme.diff;
        FieldTheme {
            text: theme.text,
            accent: theme.accent,
            selected_text: theme.background,
            placeholder: theme.secondary_text,
            font_size: self.theme.terminal.font_size,
        }
    }

    /// Keeps what was typed in the field, as a new comment or as the new
    /// text of one.
    fn keep_comment(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.take() else {
            return;
        };
        if let Some(review) = self.review_mut() {
            match editing.target {
                Target::New(lines) => {
                    review.add(lines, text);
                }
                Target::Comment(id) => {
                    review.edit(id, text);
                }
            }
        }
        self.close_editor(window, cx);
    }

    /// Closes the field, and gives the keyboard back to the view.
    fn close_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.editing = None;
        self.selection = None;
        window.focus(&self.focus, cx);
        self.place_items();
        cx.notify();
    }

    fn edit_comment(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let text = self
            .review()
            .and_then(|review| review.comments().iter().find(|c| c.id == id && !c.sent))
            .map(|comment| comment.text.clone());
        if let Some(text) = text {
            window.prevent_default();
            self.selection = None;
            self.open_editor(Target::Comment(id), &text, window, cx);
        }
    }

    fn delete_comment(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(review) = self.review_mut() {
            review.remove(id);
        }
        let edited = self
            .editing
            .as_ref()
            .is_some_and(|editing| editing.target == Target::Comment(id));
        if edited {
            self.close_editor(window, cx);
        }
        self.place_items();
        cx.notify();
    }

    /// Escape takes away, in this order, the file picker, what is asked
    /// before sending, the selection, and the view.
    fn escape(&mut self, cx: &mut Context<Self>) {
        if self.file_picker.take().is_some()
            || self.asking.take().is_some()
            || self.selection.take().is_some()
        {
            self.place_items();
            cx.notify();
        } else {
            cx.emit(DiffEvent::Leave);
        }
    }

    /// Opens the floating file picker popover, or closes it if already open.
    fn toggle_file_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.file_picker.is_some() {
            self.close_file_picker(window, cx);
            return;
        }
        if self.files().is_empty() {
            return;
        }
        let theme = self.field_theme();
        let editor = cx.new(|cx| {
            NameEditor::new("", theme, cx)
                .with_placeholder("Filter files…")
                .without_underline()
        });
        let typed = cx.observe(&editor, |this, editor, cx| {
            let query = editor.read(cx).text().to_owned();
            if let Some(picker) = &mut this.file_picker
                && picker.query != query
            {
                picker.query = query;
                picker.selected = 0;
                picker.scroll.scroll_to_item(0);
            }
            cx.notify();
        });
        let events = cx.subscribe_in(&editor, window, |this, _, event, window, cx| match event {
            NameEditorEvent::Confirmed(_) => this.confirm_file_picker(window, cx),
            NameEditorEvent::Cancelled => this.close_file_picker(window, cx),
        });
        let focus = editor.read(cx).focus_handle().clone();
        let scroll = ScrollHandle::new();
        scroll.scroll_to_item(self.selected);
        self.file_picker = Some(FilePicker {
            editor,
            selected: self.selected,
            query: String::new(),
            scroll,
            _subscriptions: [typed, events],
        });
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Closes the file picker and returns keyboard focus to the diff view.
    fn close_file_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.file_picker.take().is_some() {
            window.focus(&self.focus, cx);
            cx.notify();
        }
    }

    /// File indexes in `self.files()` matching the file picker's query.
    fn picker_matches(&self, cx: &App) -> Vec<usize> {
        let query = self
            .file_picker
            .as_ref()
            .map(|picker| picker.editor.read(cx).text())
            .unwrap_or_default();
        if query.trim().is_empty() {
            return (0..self.files().len()).collect();
        }
        let names: Vec<&str> = self.files().iter().map(|file| file.path.as_str()).collect();
        crate::command_match::matches(query, &names)
    }

    /// Moves the highlighted file in the open file picker.
    fn picker_step(&mut self, step: isize, cx: &mut Context<Self>) {
        let count = self.picker_matches(cx).len();
        if count == 0 {
            return;
        }
        if let Some(picker) = &mut self.file_picker {
            let current = picker.selected.min(count - 1);
            let next = current.saturating_add_signed(step).min(count - 1);
            picker.selected = next;
            picker.scroll.scroll_to_item(next);
            cx.notify();
        }
    }

    /// Jumps to the highlighted file in the file picker and closes it.
    fn confirm_file_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self.picker_matches(cx);
        let chosen = self
            .file_picker
            .as_ref()
            .and_then(|picker| shown.get(picker.selected.min(shown.len().saturating_sub(1))))
            .copied();
        if let Some(file_index) = chosen {
            self.select_file(file_index, cx);
        }
        self.close_file_picker(window, cx);
    }

    /// The floating file picker popover anchored below the diff heading.
    fn file_picker_overlay(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let picker = self.file_picker.as_ref()?;
        let theme = &self.theme.diff;
        let mono = self.mono_font();
        let shown = self.picker_matches(cx);
        let selected = picker.selected.min(shown.len().saturating_sub(1));
        let rows: Vec<AnyElement> = if shown.is_empty() {
            vec![
                div()
                    .px(px(10.))
                    .py(px(8.))
                    .text_color(hsla(theme.secondary_text))
                    .child("No matching files")
                    .into_any_element(),
            ]
        } else {
            shown
                .iter()
                .enumerate()
                .filter_map(|(pos, &file_index)| {
                    let file = self.files().get(file_index)?;
                    let (mark, mark_color) = match file.status {
                        FileStatus::Added => ("+", theme.added_marker),
                        FileStatus::Modified => ("M", theme.modified_marker),
                        FileStatus::Deleted => ("-", theme.removed_marker),
                    };
                    let is_selected = pos == selected;
                    let (adds, dels) = file.hunks.iter().flat_map(|h| &h.lines).fold(
                        (0usize, 0usize),
                        |(a, d), line| match line.kind {
                            LineKind::Added => (a + 1, d),
                            LineKind::Removed => (a, d + 1),
                            LineKind::Context => (a, d),
                        },
                    );
                    let total_changes = adds + dels;
                    let stat_bar =
                        (total_changes > 0).then(|| {
                            let add_dots = ((adds * 5 + total_changes / 2) / total_changes).min(5);
                            let del_dots = ((dels * 5 + total_changes / 2) / total_changes)
                                .min(5usize.saturating_sub(add_dots));
                            let neutral_dots = 5usize.saturating_sub(add_dots + del_dots);
                            div()
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap(px(1.5))
                                .children((0..add_dots).map(|_| {
                                    div()
                                        .size(px(4.))
                                        .rounded(px(1.))
                                        .bg(hsla(theme.added_marker))
                                }))
                                .children((0..del_dots).map(|_| {
                                    div()
                                        .size(px(4.))
                                        .rounded(px(1.))
                                        .bg(hsla(theme.removed_marker))
                                }))
                                .children((0..neutral_dots).map(|_| {
                                    div().size(px(4.)).rounded(px(1.)).bg(hsla(theme.border))
                                }))
                        });
                    let unsent_comments = self.review().map_or(0, |review| {
                        review
                            .unsent()
                            .filter(|comment| comment.lines.path == file.path)
                            .count()
                    });
                    let comment_badge = (unsent_comments > 0).then(|| {
                        crate::design::pill(
                            unsent_comments.to_string(),
                            theme.accent,
                            theme.font_size,
                            true,
                        )
                    });
                    Some(
                        div()
                            .id(("diff-file-option", file_index))
                            .debug_selector(move || format!("diff-file-option-{file_index}"))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .mx(px(4.))
                            .px(px(8.))
                            .py(px(5.))
                            .rounded(px(crate::theme::RADIUS_CONTROL))
                            .cursor_pointer()
                            .when(is_selected, |row| {
                                row.bg(hsla(theme.file_active_background))
                            })
                            .hover(|row| row.bg(hsla(theme.file_active_background)))
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(14.))
                                    .font_family(mono.clone())
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(hsla(mark_color))
                                    .child(mark),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(mono.clone())
                                    .text_color(hsla(theme.text))
                                    .child(file.path.clone()),
                            )
                            .children(comment_badge)
                            .children(stat_bar)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.select_file(file_index, cx);
                                    this.close_file_picker(window, cx);
                                }),
                            )
                            .into_any_element(),
                    )
                })
                .collect()
        };
        Some(
            div()
                .id("diff-file-picker")
                .debug_selector(|| "diff-file-picker".into())
                .key_context(PICKER_CONTEXT)
                .on_action(cx.listener(|this, _: &PickerNext, _, cx| this.picker_step(1, cx)))
                .on_action(cx.listener(|this, _: &PickerPrevious, _, cx| this.picker_step(-1, cx)))
                .occlude()
                .absolute()
                .overflow_hidden()
                .top(px(36.))
                .left(px(HEADING_PADDING))
                .w(px(420.))
                .max_w_full()
                .flex()
                .flex_col()
                .bg(hsla(theme.file_list_background))
                .border_1()
                .border_color(hsla(theme.border))
                .rounded(px(crate::theme::RADIUS_CARD))
                .shadow_lg()
                .child(crate::design::top_highlight_bar(theme.file_list_background))
                .child(
                    div()
                        .px(px(10.))
                        .py(px(6.))
                        .border_b_1()
                        .border_color(hsla(theme.border))
                        .flex()
                        .child(picker.editor.clone()),
                )
                .child(
                    div()
                        .id("diff-file-picker-list")
                        .max_h(px(320.))
                        .overflow_y_scroll()
                        .track_scroll(&picker.scroll)
                        .py(px(4.))
                        .children(rows),
                ),
        )
    }

    /// Lays out the list of rows: each row, then the comments of the
    /// shown file that end on it, and the field where it belongs. A
    /// comment whose lines are no longer in the diff goes at the end.
    fn place_items(&mut self) {
        let comments: Vec<(u64, &Lines)> = self
            .review()
            .into_iter()
            .flat_map(|review| review.comments())
            .map(|comment| (comment.id, &comment.lines))
            .collect();
        let target = self.editing.as_ref().map(|editing| &editing.target);
        let comment_item = |id: u64| match target {
            Some(Target::Comment(edited)) if *edited == id => Item::Editor,
            _ => Item::Comment(id),
        };
        let mut items = Vec::with_capacity(self.rows.len() + comments.len() + 1);
        let mut placed = vec![false; comments.len()];
        let mut editor_placed = !matches!(target, Some(Target::New(_)));
        for (index, row) in self.rows.iter().enumerate() {
            items.push(Item::Row(index));
            for (n, (id, lines)) in comments.iter().enumerate() {
                if !placed[n] && lines.ends_at(row) {
                    placed[n] = true;
                    items.push(comment_item(*id));
                }
            }
            if let Some(Target::New(lines)) = target
                && !editor_placed
                && lines.ends_at(row)
            {
                editor_placed = true;
                items.push(Item::Editor);
            }
        }
        for (n, (id, _)) in comments.iter().enumerate() {
            if !placed[n] {
                items.push(comment_item(*id));
            }
        }
        if !editor_placed {
            items.push(Item::Editor);
        }
        self.items = items;
    }

    /// What is said in place of the diff, when there is no diff to show.
    pub fn notice(&self) -> Option<String> {
        if self.source.is_none() {
            return Some("This workspace has no task, so there is no diff.".into());
        }
        if let Some(error) = &self.error {
            if error.is_too_large() {
                return Some(format!(
                    "The diff is too large to show (over {}).",
                    format_limit(self.git.diff_output_limit)
                ));
            }
            return Some(format!("The diff could not be read: {error}"));
        }
        match &self.diff {
            None => Some("Reading the diff…".into()),
            Some(diff) if diff.files.is_empty() && diff.untracked_truncated => {
                Some("The diff is too large to show: too many new files.".into())
            }
            Some(diff) if diff.files.is_empty() => {
                Some("No changes since the task branched off.".into())
            }
            Some(_) => None,
        }
    }

    /// Brings the field for a comment into view: it is under the last of
    /// its lines, which may be the last row on screen.
    fn reveal_editor(&mut self) {
        if let Some(item) = self.items.iter().position(|item| *item == Item::Editor) {
            self.rows_scroll
                .scroll_to_item(item, ScrollStrategy::Nearest);
        }
    }

    /// Brings the row at `index` into view, if it is not.
    pub fn reveal_row(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(item) = self.items.iter().position(|item| *item == Item::Row(index)) {
            self.rows_scroll
                .scroll_to_item(item, ScrollStrategy::Nearest);
            cx.notify();
        }
    }

    /// Goes to the file at `index` of the changed files: its first row
    /// is brought to the top of the column.
    pub fn select_file(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(path) = self.files().get(index).map(|file| file.path.clone()) else {
            return;
        };
        self.selected = index;
        // The rows are gone through from the top of the file gone to.
        self.selection = None;
        let header = self.items.iter().position(|item| match item {
            Item::Row(row) => self
                .rows
                .get(*row)
                .is_some_and(|row| row.kind == RowKind::File && row.path.as_ref() == path),
            _ => false,
        });
        if let Some(header) = header {
            self.rows_scroll.scroll_to_item(header, ScrollStrategy::Top);
        }
        cx.notify();
    }

    /// Stops what was watched and read, and watches and reads again if
    /// there is something to show.
    fn restart(&mut self, cx: &mut Context<Self>) {
        self.watching = None;
        self.reads += 1;
        let Some(source) = self.source.as_ref().filter(|_| self.shown) else {
            return;
        };
        // Watched before it is read, so no change in between is missed.
        // A worktree that cannot be watched, as one that is gone, says
        // why when it is read.
        if let Ok((watcher, mut changes)) = WorktreeWatcher::start(&source.worktree) {
            let reads = cx.spawn(async move |this, cx| {
                while changes.next().await.is_some() {
                    if this.update(cx, |this, cx| this.read(cx)).is_err() {
                        return;
                    }
                }
            });
            self.watching = Some(Watching {
                _watcher: watcher,
                _reads: reads,
            });
        }
        self.read(cx);
    }

    /// Reads the diff on a background thread. At most one read runs at a
    /// time for each worktree; notifications that arrive while a read is
    /// running are coalesced into a single follow-up read when it ends.
    fn read(&mut self, cx: &mut Context<Self>) {
        if !self.shown {
            return;
        }
        let Some(source) = self.source.clone() else {
            return;
        };
        if let Some(reread) = self.in_flight.get_mut(&source.worktree) {
            *reread = true;
            return;
        }
        self.reads += 1;
        let read = self.reads;
        let git = self.git.clone();
        let worktree = source.worktree.clone();
        let (sender, receiver) = oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("benterm-diff-read".into())
            .spawn(move || {
                // An agent works in the worktree and can put another
                // `.git` there, so it is looked at before every read.
                let diff = git
                    .check_worktree(&source.repository, &source.worktree)
                    .and_then(|()| git.diff(&source.worktree, &source.base_commit));
                let _ = sender.send(diff);
            });
        if let Err(error) = spawned {
            self.take(
                Err(TaskError::new(format!(
                    "diff reader thread could not be started ({})",
                    error.kind()
                ))),
                cx,
            );
            return;
        }
        self.in_flight.insert(worktree.clone(), false);
        cx.spawn(async move |this, cx| {
            let diff = receiver.await;
            let _ = this.update(cx, |this, cx| {
                let reread = this.in_flight.remove(&worktree).unwrap_or(false);
                if let Ok(diff) = diff
                    && this.reads == read
                {
                    this.take(diff, cx);
                }
                if reread
                    && this.shown
                    && this
                        .source
                        .as_ref()
                        .is_some_and(|current| current.worktree == worktree)
                {
                    this.read(cx);
                }
            });
        })
        .detach();
    }

    /// Shows a diff that was read, on the same file as before if it is
    /// still changed.
    fn take(&mut self, diff: Result<Diff, TaskError>, cx: &mut Context<Self>) {
        let diff = match diff {
            Ok(diff) => diff,
            Err(error) => {
                if error.is_too_large() {
                    self.diff = None;
                    self.rows.clear();
                    self.selection = None;
                    self.place_items();
                }
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        self.error = None;
        let truncated = diff.untracked_truncated;
        let shown = self.selected_file().map(|file| file.path.clone());
        let selected = shown
            .and_then(|path| diff.files.iter().position(|file| file.path == path))
            .unwrap_or(self.selected.min(diff.files.len().saturating_sub(1)));
        self.diff = Some(diff);
        self.selected = selected;
        let mut rows = all_rows(self.files());
        if truncated {
            rows.push(DiffRow {
                kind: RowKind::Note,
                old: None,
                new: None,
                text: "Some new files were not read: the limit was reached.".into(),
                path: SharedString::default(),
            });
        }
        if rows != self.rows {
            // The rows the selection was of may have moved.
            self.selection = None;
            self.rows = rows;
        }
        self.place_items();
        cx.notify();
    }

    fn select_by(&mut self, step: isize, cx: &mut Context<Self>) {
        let count = self.files().len();
        if count == 0 {
            return;
        }
        let index = self.selected.saturating_add_signed(step).min(count - 1);
        if index != self.selected {
            self.select_file(index, cx);
        }
    }

    /// The font of the rows. Looked up when the theme changes, as the
    /// lookup is too slow for a redraw.
    fn mono_font(&self) -> SharedString {
        self.mono.clone()
    }

    fn diff_rows(
        &self,
        range: Range<usize>,
        row_height: f32,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let theme = &self.theme.diff;
        let number = |value: Option<u32>| {
            div()
                .flex_none()
                .w(px(NUMBER_WIDTH))
                .pr(px(8.))
                .flex()
                .justify_end()
                .text_color(hsla(theme.line_number_text))
                .children(value.map(|n| n.to_string()))
        };
        let selected = self
            .selection
            .map(|(anchor, head)| anchor.min(head)..=anchor.max(head));
        range
            .filter_map(|index| self.items.get(index).copied())
            .filter_map(|item| match item {
                Item::Row(index) => {
                    let row = self.rows.get(index)?;
                    let mark = match row.kind {
                        RowKind::Added => "+",
                        RowKind::Removed => "-",
                        _ => "",
                    };
                    // Added lines are green and removed ones red, as git
                    // colors them, on the faint background of the mock.
                    let text_color = match row.kind {
                        RowKind::Hunk => theme.hunk_text,
                        RowKind::Note => theme.secondary_text,
                        RowKind::Added => theme.added_marker,
                        RowKind::Removed => theme.removed_marker,
                        RowKind::Context | RowKind::File => theme.text,
                    };
                    if row.kind == RowKind::File {
                        // The line that names a file, from the left edge.
                        let element = div()
                            .id(("diff-row", index))
                            .debug_selector(move || format!("diff-row-{index}"))
                            .w_full()
                            .flex()
                            .h(px(row_height))
                            .items_center()
                            .px(px(HEADING_PADDING))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .border_t_1()
                            .border_color(hsla(theme.border))
                            .font_weight(FontWeight::BOLD)
                            .text_color(hsla(theme.text))
                            .child(row.text.clone());
                        return Some(element.into_any_element());
                    }
                    let background = match &selected {
                        Some(rows) if rows.contains(&index) => Some(theme.selected_background),
                        _ => row_background(row.kind, theme),
                    };
                    let word_highlight = self.inline_diff_range(index, row.kind);
                    let content = match word_highlight {
                        Some(range) if !range.is_empty() && range.end <= row.text.len() => {
                            let text = row.text.as_ref();
                            let hl = crate::design::with_alpha(text_color, 0.28);
                            let mut line = div().flex().items_center().text_color(hsla(text_color));
                            if range.start > 0 {
                                line = line.child(text[..range.start].to_owned());
                            }
                            line = line.child(
                                div()
                                    .rounded(px(2.))
                                    .bg(hl)
                                    .child(text[range.start..range.end].to_owned()),
                            );
                            if range.end < text.len() {
                                line = line.child(text[range.end..].to_owned());
                            }
                            line
                        }
                        _ => div().text_color(hsla(text_color)).child(row.text.clone()),
                    };
                    let element = div()
                        .id(("diff-row", index))
                        .debug_selector(move || format!("diff-row-{index}"))
                        .w_full()
                        .flex()
                        .h(px(row_height))
                        .items_center()
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .when_some(background, |div, color| div.bg(hsla(color)))
                        // A selected row keeps the color of the selection.
                        .when_some(
                            row_hover_background(row.kind, theme).filter(|_| {
                                !selected.as_ref().is_some_and(|rows| rows.contains(&index))
                            }),
                            |div, color| div.cursor_pointer().hover(move |div| div.bg(hsla(color))),
                        )
                        .child(number(row.old))
                        .child(number(row.new))
                        .child(div().flex_none().w(px(MARK_WIDTH)).child(mark))
                        .child(content)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                this.click_row(index, event.modifiers.shift, window, cx)
                            }),
                        );
                    Some(element.into_any_element())
                }
                Item::Comment(id) => {
                    let comment = self.review()?.comments().iter().find(|c| c.id == id)?;
                    Some(self.comment_row(comment, row_height, cx))
                }
                Item::Editor => {
                    let editing = self.editing.as_ref()?;
                    let label = match &editing.target {
                        Target::New(lines) => lines.label(),
                        Target::Comment(id) => self
                            .review()
                            .and_then(|review| review.comments().iter().find(|c| c.id == *id))
                            .map(|comment| comment.lines.label())
                            .unwrap_or_default(),
                    };
                    // A long label is cut short before the field is.
                    let label = div()
                        .min_w_0()
                        .truncate()
                        .text_color(hsla(theme.secondary_text))
                        .child(label)
                        .into_any_element();
                    let field = div()
                        .flex()
                        .flex_1()
                        .min_w(px(FIELD_MIN_WIDTH))
                        .child(editing.editor.clone())
                        .into_any_element();
                    let element = self
                        .comment_box(row_height, vec![label, field])
                        .debug_selector(|| "diff-comment-editor".into());
                    Some(element.into_any_element())
                }
            })
            .collect()
    }

    /// Computes an inline changed byte range when `index` is part of an adjacent
    /// Removed + Added pair (e.g. a single modified line).
    fn inline_diff_range(&self, index: usize, kind: RowKind) -> Option<Range<usize>> {
        match kind {
            RowKind::Removed => {
                let next = self.rows.get(index + 1)?;
                if next.kind != RowKind::Added {
                    return None;
                }
                let prev_is_removed = index > 0
                    && self
                        .rows
                        .get(index - 1)
                        .is_some_and(|r| r.kind == RowKind::Removed);
                let next_next_is_added = self
                    .rows
                    .get(index + 2)
                    .is_some_and(|r| r.kind == RowKind::Added);
                if prev_is_removed || next_next_is_added {
                    return None;
                }
                Some(word_diff_ranges(&self.rows[index].text, &next.text).0)
            }
            RowKind::Added => {
                let prev = index.checked_sub(1).and_then(|i| self.rows.get(i))?;
                if prev.kind != RowKind::Removed {
                    return None;
                }
                let prev_prev_is_removed = index >= 2
                    && self
                        .rows
                        .get(index - 2)
                        .is_some_and(|r| r.kind == RowKind::Removed);
                let next_is_added = self
                    .rows
                    .get(index + 1)
                    .is_some_and(|r| r.kind == RowKind::Added);
                if prev_prev_is_removed || next_is_added {
                    return None;
                }
                Some(word_diff_ranges(&prev.text, &self.rows[index].text).1)
            }
            _ => None,
        }
    }

    /// A line of the list of rows for a comment or its field, indented to
    /// the text of the rows, with `children` in its box.
    fn comment_box(&self, row_height: f32, children: Vec<AnyElement>) -> gpui::Div {
        let theme = &self.theme.diff;
        div()
            .w_full()
            .h(px(row_height))
            .flex()
            .pl(px(NUMBER_WIDTH * 2. + MARK_WIDTH - 3.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(8.))
                    .bg(hsla(theme.comment_background))
                    .border_l(px(3.))
                    .border_color(hsla(theme.accent))
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .children(children),
            )
    }

    fn comment_row(
        &self,
        comment: &crate::review::Comment,
        row_height: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme.diff;
        let id = comment.id;
        let button = |name: &'static str, label: &'static str| {
            div()
                .id(SharedString::from(format!("{name}-{id}")))
                .debug_selector(move || format!("{name}-{id}"))
                .flex_none()
                .px(px(6.))
                .cursor_pointer()
                .text_color(hsla(theme.secondary_text))
                .child(label)
        };
        let actions = if comment.sent {
            vec![
                div()
                    .flex_none()
                    .text_color(hsla(theme.added_marker))
                    .child("✓ Sent")
                    .into_any_element(),
                button("diff-comment-delete", "Delete")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| this.delete_comment(id, window, cx)),
                    )
                    .into_any_element(),
            ]
        } else {
            vec![
                button("diff-comment-edit", "Edit")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| this.edit_comment(id, window, cx)),
                    )
                    .into_any_element(),
                button("diff-comment-delete", "Delete")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| this.delete_comment(id, window, cx)),
                    )
                    .into_any_element(),
            ]
        };
        let text = div()
            .flex_1()
            .min_w_0()
            .truncate()
            .when(comment.sent, |text| {
                text.text_color(hsla(theme.secondary_text))
            })
            .child(comment.text.clone())
            .into_any_element();
        let mut children = vec![text];
        children.extend(actions);
        self.comment_box(row_height, children)
            .id(("diff-comment", id as usize))
            .debug_selector(move || format!("diff-comment-{id}"))
            .into_any_element()
    }

    /// The button that sends the comments not sent yet, and how many.
    fn send_button(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let theme = &self.theme.diff;
        let count = self.review()?.unsent().count();
        (count > 0).then(|| {
            let label = match count {
                1 => "Send 1 comment ⌘↩".to_owned(),
                n => format!("Send {n} comments ⌘↩"),
            };
            div()
                .id("diff-send")
                .debug_selector(|| "diff-send".into())
                .flex_none()
                .ml_auto()
                .px(px(10.))
                .py(px(2.))
                .rounded(px(crate::theme::RADIUS_CONTROL))
                .cursor_pointer()
                .bg(hsla(theme.accent))
                .text_color(hsla(theme.background))
                .child(label)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.send(false, cx)),
                )
        })
    }

    /// What is asked or said before the comments are sent, under the
    /// heading.
    fn ask_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let theme = &self.theme.diff;
        let (text, confirm) = match self.asking.as_ref()? {
            Readiness::Ready => return None,
            Readiness::Confirm(text) => (text.clone(), true),
            Readiness::Refuse(text) => (text.clone(), false),
        };
        let button = |name: &'static str, label: &'static str| {
            div()
                .id(name)
                .debug_selector(move || name.into())
                .flex_none()
                .px(px(10.))
                .py(px(2.))
                .rounded(px(crate::theme::RADIUS_CONTROL))
                .cursor_pointer()
                .border_1()
                .border_color(hsla(theme.border))
                .child(label)
        };
        Some(
            div()
                .debug_selector(|| "diff-ask".into())
                .flex()
                .flex_none()
                .items_center()
                .gap(px(10.))
                .px(px(14.))
                .py(px(6.))
                .border_b_1()
                .border_color(hsla(theme.border))
                .bg(hsla(theme.comment_background))
                .child(div().flex_1().min_w_0().child(text))
                .when(confirm, |bar| {
                    bar.child(button("diff-ask-send", "Send anyway").on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.send(true, cx)),
                    ))
                })
                .child(
                    button("diff-ask-cancel", if confirm { "Cancel" } else { "Close" })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| this.ask(None, cx)),
                        ),
                ),
        )
    }

    fn main(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = &self.theme.diff;
        let terminal = &self.theme.terminal;
        let mono = self.mono_font();
        let heading = self.source.as_ref().map(|source| {
            let name = self
                .selected_file()
                .map(|file| file.path.clone())
                .unwrap_or_default();
            let count = self.files().len();
            let step = |name: &'static str, label: &'static str, step: isize| {
                div()
                    .id(name)
                    .debug_selector(move || name.into())
                    .flex_none()
                    .px(px(6.))
                    .rounded(px(crate::theme::RADIUS_CONTROL))
                    .cursor_pointer()
                    .hover(|button| {
                        button
                            .bg(hsla(theme.file_active_background))
                            .text_color(hsla(theme.text))
                    })
                    .child(label)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| this.select_by(step, cx)),
                    )
            };
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(10.))
                .px(px(HEADING_PADDING))
                .py(px(8.))
                .border_b_1()
                .border_color(hsla(theme.border))
                .text_color(hsla(theme.secondary_text))
                .child(
                    div()
                        .id("diff-file-picker-toggle")
                        .debug_selector(|| "diff-file-picker-toggle".into())
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .min_w_0()
                        .px(px(6.))
                        .py(px(2.))
                        .rounded(px(crate::theme::RADIUS_CONTROL))
                        .when(count > 0, |button| {
                            button
                                .cursor_pointer()
                                .hover(|button| button.bg(hsla(theme.file_active_background)))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.toggle_file_picker(window, cx);
                                    }),
                                )
                        })
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .font_family(mono.clone())
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(hsla(theme.text))
                                .child(name),
                        )
                        .when(count > 0, |button| {
                            button.child(
                                div()
                                    .flex_none()
                                    .text_color(hsla(theme.secondary_text))
                                    .child("▾"),
                            )
                        }),
                )
                // Which of the changed files that is, and the way to
                // the one before and the next, as the arrows go.
                .when(count > 0, |heading| {
                    heading
                        .child(
                            div()
                                .debug_selector(|| "diff-file-count".into())
                                .flex_none()
                                .child(format!("{} / {count} files", self.selected + 1)),
                        )
                        .child(step("diff-file-previous", "↑", -1))
                        .child(step("diff-file-next", "↓", 1))
                })
                .child(div().flex_1())
                .child(
                    div()
                        .flex_none()
                        .child(format!("{} ← {}", source.branch, source.base_branch)),
                )
                .children(self.send_button(cx))
        });
        let ask = self.ask_bar(cx);
        let body = match self.notice() {
            Some(notice) => div()
                .debug_selector(|| "diff-notice".into())
                .p(px(16.))
                .child(crate::design::empty_state(
                    notice,
                    &[("Esc", "Back to panes"), ("⇧⌘B", "Progress board")],
                    theme.comment_background,
                    theme.border,
                    theme.text,
                    theme.secondary_text,
                    theme.font_size,
                ))
                .into_any_element(),
            None => {
                let row_height = (terminal.font_size * ROW_HEIGHT).ceil();
                uniform_list(
                    "diff-rows",
                    self.items.len(),
                    cx.processor(move |this, range, _, cx| this.diff_rows(range, row_height, cx)),
                )
                .flex_1()
                .font_family(mono)
                .text_size(px(terminal.font_size))
                .track_scroll(&self.rows_scroll)
                .into_any_element()
            }
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .children(heading)
            .children(ask)
            .child(body)
    }
}

/// Returns the changed byte ranges `(old_range, new_range)` between `old` and
/// `new` by trimming matching leading and trailing characters.
fn word_diff_ranges(old: &str, new: &str) -> (Range<usize>, Range<usize>) {
    let old_chars: Vec<(usize, char)> = old.char_indices().collect();
    let new_chars: Vec<(usize, char)> = new.char_indices().collect();
    let mut prefix = 0;
    while prefix < old_chars.len()
        && prefix < new_chars.len()
        && old_chars[prefix].1 == new_chars[prefix].1
    {
        prefix += 1;
    }
    let mut suffix = 0;
    while prefix + suffix < old_chars.len()
        && prefix + suffix < new_chars.len()
        && old_chars[old_chars.len() - 1 - suffix].1 == new_chars[new_chars.len() - 1 - suffix].1
    {
        suffix += 1;
    }
    let byte_range = |s: &str, chars: &[(usize, char)]| {
        let start = chars.get(prefix).map_or(s.len(), |(idx, _)| *idx);
        let end_idx = chars.len().saturating_sub(suffix);
        let end = chars.get(end_idx).map_or(s.len(), |(idx, _)| *idx);
        start..end.max(start)
    };
    (byte_range(old, &old_chars), byte_range(new, &new_chars))
}

impl Render for DiffView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme.diff;
        div()
            .id("diff-view")
            .debug_selector(|| "diff-view".into())
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &SelectNextChangedFile, _, cx| this.select_by(1, cx)))
            .on_action(
                cx.listener(|this, _: &SelectPreviousChangedFile, _, cx| this.select_by(-1, cx)),
            )
            .on_action(cx.listener(|this, _: &LeaveDiff, _, cx| this.escape(cx)))
            .on_action(
                cx.listener(|this, _: &SelectNextLine, _, cx| this.select_line(1, false, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SelectPreviousLine, _, cx| this.select_line(-1, false, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ExtendSelectionDown, _, cx| this.select_line(1, true, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ExtendSelectionUp, _, cx| this.select_line(-1, true, cx)),
            )
            .on_action(cx.listener(|this, _: &CommentOnSelection, window, cx| {
                this.comment_on_selection(window, cx)
            }))
            .on_action(cx.listener(|this, _: &SendReview, _, cx| {
                // Asked already: the key says yes.
                let confirmed = matches!(this.asking, Some(Readiness::Confirm(_)));
                this.send(confirmed, cx)
            }))
            .relative()
            .size_full()
            .flex()
            .bg(hsla(theme.background))
            .text_color(hsla(theme.text))
            .font_family(theme.font_family.clone())
            .text_size(px(theme.font_size))
            .child(self.main(cx))
            .children(self.file_picker_overlay(cx))
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

    use gpui::TestAppContext;

    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(10);

    #[test]
    fn word_diff_ranges_isolates_changed_middle_span() {
        let old = "    let value = foo_old(bar);";
        let new = "    let value = foo_new(bar);";
        let (r_old, r_new) = word_diff_ranges(old, new);
        assert_eq!(&old[r_old], "old");
        assert_eq!(&new[r_new], "new");
    }

    fn pass_time(cx: &mut TestAppContext) {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(10));
        cx.executor().advance_clock(Duration::from_millis(10));
        cx.run_until_parked();
    }

    fn wait_for(
        cx: &mut TestAppContext,
        what: &str,
        mut done: impl FnMut(&mut TestAppContext) -> bool,
    ) {
        let deadline = Instant::now() + TIMEOUT;
        while !done(cx) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            pass_time(cx);
        }
    }

    fn fake_git(dir: &Path, name: &str, script: &str) -> Git {
        let bin = dir.join(name);
        std::fs::create_dir_all(&bin).unwrap();
        let git_bin = bin.join("git");
        std::fs::write(&git_bin, script).unwrap();
        std::fs::set_permissions(&git_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        Git {
            path: Some(format!("{}:/usr/bin:/bin", bin.display()).into()),
            restrict_programs: false,
            timeout: Duration::from_secs(5),
            ..Git::default()
        }
    }

    /// A source whose worktree has the `.git` file of a worktree of a
    /// repository next to it, as git makes it.
    fn source(task: u64, worktree: PathBuf) -> DiffSource {
        let repository = worktree.with_extension("repository");
        let linked = repository.join(".git/worktrees/worktree");
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", linked.display()),
        )
        .unwrap();
        DiffSource {
            task,
            repository,
            worktree,
            base_commit: "0123456789abcdef".to_owned(),
            base_branch: "main".to_owned(),
            branch: format!("task-{task}"),
        }
    }

    fn added_lines(view: &Entity<DiffView>, cx: &mut TestAppContext) -> Vec<String> {
        view.read_with(cx, |view, _| {
            view.rows()
                .iter()
                .filter(|row| row.kind == RowKind::Added)
                .map(|row| row.text.to_string())
                .collect()
        })
    }

    fn line_count(path: &Path) -> usize {
        std::fs::read_to_string(path)
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }

    #[gpui::test]
    fn changes_during_a_read_coalesce_into_one_follow_up_read(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("worktree");
        let state = dir.path().join("state");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(&state).unwrap();

        let calls = state.join("calls.log");
        let first_dir = state.join("first");
        let first_started = state.join("first_started");
        let release_first = state.join("release_first");

        let script = format!(
            "#!/bin/sh\n\
             case \" $* \" in\n\
               *\" ls-files \"*) exit 0 ;;\n\
             esac\n\
             printf '1\\n' >> \"{calls}\"\n\
             if mkdir \"{first_dir}\" 2>/dev/null; then\n\
               : > \"{first_started}\"\n\
               i=0\n\
               while [ ! -f \"{release_first}\" ] && [ \"$i\" -lt 500 ]; do\n\
                 sleep 0.01\n\
                 i=$((i + 1))\n\
               done\n\
               printf 'diff --git a/README.md b/README.md\\n--- a/README.md\\n+++ b/README.md\\n@@ -1,1 +1,2 @@\\n one\\n+first\\n'\n\
             else\n\
               printf 'diff --git a/README.md b/README.md\\n--- a/README.md\\n+++ b/README.md\\n@@ -1,1 +1,2 @@\\n one\\n+second\\n'\n\
             fi\n",
            calls = calls.display(),
            first_dir = first_dir.display(),
            first_started = first_started.display(),
            release_first = release_first.display(),
        );
        let git = fake_git(dir.path(), "bin", &script);
        let src = source(1, worktree);

        let view = cx.new(|cx| DiffView::new(Theme::default(), git, cx));
        view.update(cx, |view, cx| {
            view.set_shown(true, cx);
            view.set_source(Some(src), cx);
        });

        // Wait until the first git diff is actually running.
        wait_for(cx, "first read to start", |_| first_started.exists());

        // Send 3 change notifications while the first read is still in flight.
        view.update(cx, |view, cx| {
            view.read(cx);
            view.read(cx);
            view.read(cx);
        });
        pass_time(cx);

        // No second read should have started yet while the first is still running.
        assert_eq!(line_count(&calls), 1);

        // Let the first read finish; a single follow-up read should run and win.
        std::fs::write(&release_first, "").unwrap();
        wait_for(cx, "second read result", |cx| {
            added_lines(&view, cx) == ["second"]
        });
        // Give any stray extra read a chance to run before checking the count.
        pass_time(cx);

        assert_eq!(line_count(&calls), 2);
        assert_eq!(added_lines(&view, cx), ["second"]);
    }

    #[gpui::test]
    fn reads_for_different_tasks_run_concurrently(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let worktree_a = dir.path().join("worktree-a");
        let worktree_b = dir.path().join("worktree-b");
        let state = dir.path().join("state");
        std::fs::create_dir_all(&worktree_a).unwrap();
        std::fs::create_dir_all(&worktree_b).unwrap();
        std::fs::create_dir_all(&state).unwrap();

        let entered_a = state.join("entered_a");
        let entered_b = state.join("entered_b");

        // Each worktree's diff waits for the other worktree's diff to start.
        // This rendezvous can only complete if the two tasks read concurrently.
        let script = format!(
            "#!/bin/sh\n\
             case \" $* \" in\n\
               *\" ls-files \"*) exit 0 ;;\n\
             esac\n\
             case \" $* \" in\n\
               *\"worktree-a\"*)\n\
                 : > \"{entered_a}\"\n\
                 i=0\n\
                 while [ ! -f \"{entered_b}\" ] && [ \"$i\" -lt 500 ]; do\n\
                   sleep 0.01\n\
                   i=$((i + 1))\n\
                 done\n\
                 [ -f \"{entered_b}\" ] || exit 1\n\
                 printf 'diff --git a/a.txt b/a.txt\\n--- a/a.txt\\n+++ b/a.txt\\n@@ -1,1 +1,2 @@\\n base\\n+from-a\\n'\n\
                 ;;\n\
               *\"worktree-b\"*)\n\
                 : > \"{entered_b}\"\n\
                 i=0\n\
                 while [ ! -f \"{entered_a}\" ] && [ \"$i\" -lt 500 ]; do\n\
                   sleep 0.01\n\
                   i=$((i + 1))\n\
                 done\n\
                 [ -f \"{entered_a}\" ] || exit 1\n\
                 printf 'diff --git a/b.txt b/b.txt\\n--- a/b.txt\\n+++ b/b.txt\\n@@ -1,1 +1,2 @@\\n base\\n+from-b\\n'\n\
                 ;;\n\
             esac\n",
            entered_a = entered_a.display(),
            entered_b = entered_b.display(),
        );
        let git = fake_git(dir.path(), "bin", &script);

        let view_a = cx.new(|cx| DiffView::new(Theme::default(), git.clone(), cx));
        let view_b = cx.new(|cx| DiffView::new(Theme::default(), git, cx));
        let src_a = source(1, worktree_a);
        let src_b = source(2, worktree_b);

        view_a.update(cx, |view, cx| {
            view.set_shown(true, cx);
            view.set_source(Some(src_a), cx);
        });
        view_b.update(cx, |view, cx| {
            view.set_shown(true, cx);
            view.set_source(Some(src_b), cx);
        });

        wait_for(cx, "both concurrent reads to finish", |cx| {
            added_lines(&view_a, cx) == ["from-a"] && added_lines(&view_b, cx) == ["from-b"]
        });
    }

    #[gpui::test]
    fn switching_tasks_runs_new_task_immediately_and_drops_stale_result(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let worktree_a = dir.path().join("worktree-a");
        let worktree_b = dir.path().join("worktree-b");
        let state = dir.path().join("state");
        std::fs::create_dir_all(&worktree_a).unwrap();
        std::fs::create_dir_all(&worktree_b).unwrap();
        std::fs::create_dir_all(&state).unwrap();

        let started_a = state.join("started_a");
        let release_a = state.join("release_a");
        let finished_a = state.join("finished_a");

        let script = format!(
            "#!/bin/sh\n\
             case \" $* \" in\n\
               *\" ls-files \"*) exit 0 ;;\n\
             esac\n\
             case \" $* \" in\n\
               *\"worktree-a\"*)\n\
                 : > \"{started_a}\"\n\
                 i=0\n\
                 while [ ! -f \"{release_a}\" ] && [ \"$i\" -lt 500 ]; do\n\
                   sleep 0.01\n\
                   i=$((i + 1))\n\
                 done\n\
                 printf 'diff --git a/a.txt b/a.txt\\n--- a/a.txt\\n+++ b/a.txt\\n@@ -1,1 +1,2 @@\\n base\\n+from-a\\n'\n\
                 : > \"{finished_a}\"\n\
                 ;;\n\
               *\"worktree-b\"*)\n\
                 printf 'diff --git a/b.txt b/b.txt\\n--- a/b.txt\\n+++ b/b.txt\\n@@ -1,1 +1,2 @@\\n base\\n+from-b\\n'\n\
                 ;;\n\
             esac\n",
            started_a = started_a.display(),
            release_a = release_a.display(),
            finished_a = finished_a.display(),
        );
        let git = fake_git(dir.path(), "bin", &script);
        let src_a = source(1, worktree_a);
        let src_b = source(2, worktree_b);

        let view = cx.new(|cx| DiffView::new(Theme::default(), git, cx));
        view.update(cx, |view, cx| {
            view.set_shown(true, cx);
            view.set_source(Some(src_a), cx);
        });
        wait_for(cx, "task A read to start", |_| started_a.exists());

        // Switching to task B must not wait for task A's in-flight read.
        view.update(cx, |view, cx| view.set_source(Some(src_b), cx));
        wait_for(cx, "task B result", |cx| {
            added_lines(&view, cx) == ["from-b"]
        });

        // Now let task A finish; its stale result must be dropped.
        std::fs::write(&release_a, "").unwrap();
        wait_for(cx, "task A read to finish", |_| finished_a.exists());
        pass_time(cx);

        assert_eq!(added_lines(&view, cx), ["from-b"]);
    }

    #[gpui::test]
    fn switching_away_and_back_still_serializes_same_worktree(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        let dir = tempfile::tempdir().unwrap();
        let worktree_a = dir.path().join("worktree-a");
        let worktree_b = dir.path().join("worktree-b");
        let state = dir.path().join("state");
        std::fs::create_dir_all(&worktree_a).unwrap();
        std::fs::create_dir_all(&worktree_b).unwrap();
        std::fs::create_dir_all(&state).unwrap();

        let calls_a = state.join("calls_a.log");
        let first_a = state.join("first_a");
        let started_a = state.join("started_a");
        let release_a = state.join("release_a");

        let script = format!(
            "#!/bin/sh\n\
             case \" $* \" in\n\
               *\" ls-files \"*) exit 0 ;;\n\
             esac\n\
             case \" $* \" in\n\
               *\"worktree-a\"*)\n\
                 printf '1\\n' >> \"{calls_a}\"\n\
                 if mkdir \"{first_a}\" 2>/dev/null; then\n\
                   : > \"{started_a}\"\n\
                   i=0\n\
                   while [ ! -f \"{release_a}\" ] && [ \"$i\" -lt 500 ]; do\n\
                     sleep 0.01\n\
                     i=$((i + 1))\n\
                   done\n\
                   printf 'diff --git a/a.txt b/a.txt\\n--- a/a.txt\\n+++ b/a.txt\\n@@ -1,1 +1,2 @@\\n base\\n+first-a\\n'\n\
                 else\n\
                   printf 'diff --git a/a.txt b/a.txt\\n--- a/a.txt\\n+++ b/a.txt\\n@@ -1,1 +1,2 @@\\n base\\n+second-a\\n'\n\
                 fi\n\
                 ;;\n\
               *\"worktree-b\"*)\n\
                 printf 'diff --git a/b.txt b/b.txt\\n--- a/b.txt\\n+++ b/b.txt\\n@@ -1,1 +1,2 @@\\n base\\n+from-b\\n'\n\
                 ;;\n\
             esac\n",
            calls_a = calls_a.display(),
            first_a = first_a.display(),
            started_a = started_a.display(),
            release_a = release_a.display(),
        );
        let git = fake_git(dir.path(), "bin", &script);
        let src_a = source(1, worktree_a);
        let src_b = source(2, worktree_b);

        let view = cx.new(|cx| DiffView::new(Theme::default(), git, cx));
        view.update(cx, |view, cx| {
            view.set_shown(true, cx);
            view.set_source(Some(src_a.clone()), cx);
        });
        wait_for(cx, "first task A read to start", |_| started_a.exists());

        // Switch to B and back to A while A's first read is still running.
        view.update(cx, |view, cx| view.set_source(Some(src_b), cx));
        view.update(cx, |view, cx| view.set_source(Some(src_a), cx));
        pass_time(cx);

        assert_eq!(line_count(&calls_a), 1);

        std::fs::write(&release_a, "").unwrap();
        wait_for(cx, "second task A read result", |cx| {
            added_lines(&view, cx) == ["second-a"]
        });
        pass_time(cx);

        assert_eq!(line_count(&calls_a), 2);
        assert_eq!(added_lines(&view, cx), ["second-a"]);
    }
}
