//! The file browser: a panel right of the panes with the tree of the
//! focused pane's folder, and the file opened from it, read only.
//!
//! The panel follows the focused pane by asking for its working directory
//! whenever the window is drawn, which it is after every change of focus
//! and every output of a terminal, such as the prompt after a `cd`. The
//! panel itself is a cached view, drawn again only when it changes, so
//! output in the panes does not lay out the tree and the file again.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use futures::channel::oneshot;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, InteractiveElement,
    IntoElement, KeyBinding, MouseButton, ParentElement, Render, ScrollHandle, ScrollStrategy,
    SharedString, StatefulInteractiveElement, StyleRefinement, Styled, Subscription,
    UniformListScrollHandle, Window, actions, div, px, relative, uniform_list,
};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::BentermWindow;
use crate::file_preview::{Content, PREVIEW_LIMIT, Preview, read_preview};
use crate::file_tree::{EntryKind, FileTree, FolderEntry, Row, read_folder};
use crate::markdown::render_block;
use crate::sidebar;
use crate::terminal_view::hsla;
use crate::theme::Theme;

actions!(
    file_browser,
    [
        /// Shows the files of the focused pane's folder beside the panes,
        /// or hides them.
        ToggleFileBrowser,
        SelectNextFile,
        SelectPreviousFile,
        /// Opens the selected folder, or moves into it.
        OpenFolder,
        /// Closes the selected folder, or moves to the folder it is in.
        CloseFolder,
        /// Shows the selected file, or opens or closes the selected folder.
        OpenFile,
        /// Gives the keyboard back to the focused pane.
        LeaveFileBrowser,
    ]
);

/// The key context of the panel, which its arrows are limited to.
const KEY_CONTEXT: &str = "FileBrowser";

/// The width of the tree, as the list of files in the mock's diff review.
const TREE_WIDTH: f32 = 220.;
/// The part of the window the panel takes while it shows a file.
const OPEN_SHARE: f32 = 0.5;
/// How much deeper each level of the tree is drawn.
const INDENT: f32 = 12.;
/// The space left and right of the panel's contents.
const PADDING: f32 = 12.;
/// How long an open folder must go without changes before it is read again,
/// so a burst of writes coalesces into one reload.
const SETTLE: Duration = Duration::from_millis(100);

/// Binds Cmd-Shift-E, which works anywhere in the window, and the keys
/// that move through the tree while the panel has the keyboard.
pub fn bind_keys(cx: &mut App) {
    let context = Some(KEY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("cmd-shift-e", ToggleFileBrowser, Some(sidebar::KEY_CONTEXT)),
        KeyBinding::new("down", SelectNextFile, context),
        KeyBinding::new("up", SelectPreviousFile, context),
        KeyBinding::new("right", OpenFolder, context),
        KeyBinding::new("left", CloseFolder, context),
        KeyBinding::new("enter", OpenFile, context),
        KeyBinding::new("escape", LeaveFileBrowser, context),
    ]);
}

/// What the panel tells the window.
pub enum FileBrowserEvent {
    /// The keyboard should go back to the focused pane.
    Leave,
}

/// Reads folder listings and file previews for [`FileBrowser`].
pub trait FileReader: Send + Sync + 'static {
    fn read_folder(&self, folder: &Path) -> Result<Vec<FolderEntry>, String>;
    fn read_preview(&self, path: &Path, limit: usize) -> Preview;
}

/// The default [`FileReader`], reading directly from the file system.
#[derive(Debug, Default, Clone, Copy)]
pub struct FsReader;

impl FileReader for FsReader {
    fn read_folder(&self, folder: &Path) -> Result<Vec<FolderEntry>, String> {
        read_folder(folder)
    }

    fn read_preview(&self, path: &Path, limit: usize) -> Preview {
        read_preview(path, limit)
    }
}

/// Currently watched folders shared between the UI thread and the debounce
/// thread. Canonical paths are resolved lazily on the watch thread so the UI
/// thread never touches the file system when updating watches.
#[derive(Default)]
struct WatchedFolders {
    watched: HashSet<PathBuf>,
    by_canonical: HashMap<PathBuf, PathBuf>,
    canonicalized: HashSet<PathBuf>,
}

impl WatchedFolders {
    fn insert(&mut self, folder: PathBuf) {
        self.watched.insert(folder);
    }

    fn remove(&mut self, folder: &Path) {
        self.watched.remove(folder);
        self.canonicalized.remove(folder);
        self.by_canonical.retain(|_, value| value != folder);
    }

    fn match_folder(&mut self, candidate: &Path) -> Option<PathBuf> {
        if let Some(found) = self.watched.get(candidate) {
            return Some(found.clone());
        }
        if let Some(found) = self.by_canonical.get(candidate) {
            return Some(found.clone());
        }
        let pending: Vec<PathBuf> = self
            .watched
            .iter()
            .filter(|folder| !self.canonicalized.contains(*folder))
            .cloned()
            .collect();
        for folder in pending {
            self.canonicalized.insert(folder.clone());
            if let Ok(canonical) = std::fs::canonicalize(&folder) {
                self.by_canonical.insert(canonical, folder);
            }
        }
        self.by_canonical.get(candidate).cloned()
    }

    fn all(&self) -> Vec<PathBuf> {
        self.watched.iter().cloned().collect()
    }
}

/// Watches only the open folders (non-recursively) and emits coalesced lists
/// of changed folder paths once events settle. Dropping it stops watching.
struct FolderWatcher {
    watcher: RecommendedWatcher,
    shared: Arc<Mutex<WatchedFolders>>,
}

impl FolderWatcher {
    fn start() -> notify::Result<(Self, UnboundedReceiver<Vec<PathBuf>>)> {
        let (raw_sender, raw) = mpsc::channel();
        let watcher = notify::recommended_watcher(raw_sender)?;
        let shared = Arc::new(Mutex::new(WatchedFolders::default()));
        let shared_thread = shared.clone();
        let (sender, changes) = unbounded();
        std::thread::Builder::new()
            .name("benterm-file-watch".into())
            .spawn(move || {
                while let Ok(event) = raw.recv() {
                    let mut dirty = HashSet::new();
                    collect_dirty(&event, &shared_thread, &mut dirty);
                    if dirty.is_empty() {
                        continue;
                    }
                    loop {
                        match raw.recv_timeout(SETTLE) {
                            Ok(next) => collect_dirty(&next, &shared_thread, &mut dirty),
                            Err(RecvTimeoutError::Timeout) => break,
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                    }
                    let folders: Vec<PathBuf> = dirty.into_iter().collect();
                    if !folders.is_empty() && sender.unbounded_send(folders).is_err() {
                        return;
                    }
                }
            })
            .map_err(notify::Error::io)?;
        Ok((Self { watcher, shared }, changes))
    }

    fn sync(&mut self, folders: &[PathBuf]) {
        let desired: HashSet<PathBuf> = folders.iter().cloned().collect();
        let current: Vec<PathBuf> = self
            .shared
            .lock()
            .map(|guard| guard.all())
            .unwrap_or_default();
        for folder in &current {
            if !desired.contains(folder) {
                let _ = self.watcher.unwatch(folder);
                if let Ok(mut guard) = self.shared.lock() {
                    guard.remove(folder);
                }
            }
        }
        let existing: HashSet<PathBuf> = current.into_iter().collect();
        for folder in desired {
            if !existing.contains(&folder)
                && self
                    .watcher
                    .watch(&folder, RecursiveMode::NonRecursive)
                    .is_ok()
                && let Ok(mut guard) = self.shared.lock()
            {
                guard.insert(folder);
            }
        }
    }

    fn watch_one(&mut self, folder: &Path) {
        let already = self
            .shared
            .lock()
            .map(|guard| guard.watched.contains(folder))
            .unwrap_or(false);
        if !already
            && self
                .watcher
                .watch(folder, RecursiveMode::NonRecursive)
                .is_ok()
            && let Ok(mut guard) = self.shared.lock()
        {
            guard.insert(folder.to_owned());
        }
    }
}

fn collect_dirty(
    event: &notify::Result<notify::Event>,
    shared: &Mutex<WatchedFolders>,
    dirty: &mut HashSet<PathBuf>,
) {
    let Ok(mut guard) = shared.lock() else {
        return;
    };
    let Ok(event) = event else {
        dirty.extend(guard.all());
        return;
    };
    if matches!(event.kind, EventKind::Access(_)) {
        return;
    }
    if event.paths.is_empty() {
        dirty.extend(guard.all());
        return;
    }
    for path in &event.paths {
        if let Some(folder) = guard.match_folder(path) {
            dirty.insert(folder);
        }
        if let Some(parent) = path.parent()
            && let Some(folder) = guard.match_folder(parent)
        {
            dirty.insert(folder);
        }
    }
}

/// Active folder watcher and its event consumer task.
struct Watching {
    watcher: FolderWatcher,
    _task: gpui::Task<()>,
}

/// A file being shown.
struct Opened {
    path: PathBuf,
    preview: Preview,
    /// The lines of a text file, with tabs as spaces, made once.
    lines: Vec<SharedString>,
}

/// A path requested to be revealed and opened once the tree has loaded
/// enough of its ancestors.
#[derive(Clone)]
struct PendingReveal {
    path: PathBuf,
    is_dir: bool,
}

/// The panel as a view.
pub struct FileBrowser {
    reader: Arc<dyn FileReader>,
    /// The root folder currently followed (may still be loading while `tree`
    /// keeps showing the previous root).
    target_root: Option<PathBuf>,
    /// Incremented every time `target_root` changes so stale results are
    /// discarded.
    root_generation: u64,
    /// Monotonic counter for folder read requests.
    next_request: u64,
    /// Latest in-flight request for `target_root`.
    root_request: Option<u64>,
    /// Latest in-flight request per subfolder path.
    folder_requests: HashMap<PathBuf, u64>,
    /// File path currently requested for preview.
    preview_target: Option<PathBuf>,
    /// Monotonic counter for preview read requests.
    preview_request: u64,
    /// Monotonic counter for reveal metadata checks.
    reveal_request: u64,
    /// Path waiting for the root or ancestor folders to finish loading.
    pending_reveal: Option<PendingReveal>,
    /// Watches currently open folders and triggers reloads on changes.
    watching: Option<Watching>,
    tree: Option<FileTree>,
    opened: Option<Opened>,
    theme: Theme,
    focus: FocusHandle,
    tree_scroll: UniformListScrollHandle,
    text_scroll: UniformListScrollHandle,
    markdown_scroll: ScrollHandle,
    /// The installed font of the theme's terminal.
    mono: SharedString,
}

impl EventEmitter<FileBrowserEvent> for FileBrowser {}

impl FileBrowser {
    fn new(theme: Theme, cx: &mut Context<Self>) -> Self {
        Self::with_reader(theme, Arc::new(FsReader), cx)
    }

    /// A file browser that reads folders and previews through `reader`.
    pub fn with_reader(theme: Theme, reader: Arc<dyn FileReader>, cx: &mut Context<Self>) -> Self {
        Self {
            mono: crate::theme::installed_mono_font(&theme.terminal, cx),
            reader,
            target_root: None,
            root_generation: 0,
            next_request: 0,
            root_request: None,
            folder_requests: HashMap::new(),
            preview_target: None,
            preview_request: 0,
            reveal_request: 0,
            pending_reveal: None,
            watching: None,
            tree: None,
            opened: None,
            theme,
            focus: cx.focus_handle(),
            tree_scroll: UniformListScrollHandle::new(),
            text_scroll: UniformListScrollHandle::new(),
            markdown_scroll: ScrollHandle::new(),
        }
    }

    /// The folder whose tree is shown.
    pub fn root(&self) -> Option<&Path> {
        self.tree.as_ref().map(FileTree::root)
    }

    pub fn rows(&self) -> Vec<Row> {
        self.tree.as_ref().map(FileTree::rows).unwrap_or_default()
    }

    pub fn selected(&self) -> Option<&Path> {
        self.tree.as_ref().and_then(FileTree::selected)
    }

    /// The file whose contents are shown.
    pub fn opened(&self) -> Option<&Path> {
        self.opened.as_ref().map(|opened| opened.path.as_path())
    }

    pub fn preview(&self) -> Option<&Preview> {
        self.opened.as_ref().map(|opened| &opened.preview)
    }

    /// Follows `root`, reading its entries off the UI thread if it differs
    /// from the currently followed root. Keeps the previous display until the
    /// new root finishes loading.
    pub fn follow(&mut self, root: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.target_root == root {
            return;
        }
        self.target_root = root.clone();
        self.root_generation += 1;
        self.root_request = None;
        self.folder_requests.clear();
        self.preview_target = None;
        self.preview_request += 1;
        self.reveal_request += 1;
        self.pending_reveal = None;
        self.watching = None;
        let Some(root) = root else {
            self.tree = None;
            self.opened = None;
            cx.notify();
            return;
        };
        // Start watching before reading so changes during the initial read are
        // not missed.
        self.ensure_watching(cx);
        if let Some(watching) = &mut self.watching {
            watching.watcher.sync(std::slice::from_ref(&root));
        }
        self.request_root(root, cx);
    }

    fn ensure_watching(&mut self, cx: &mut Context<Self>) {
        if self.watching.is_some() {
            return;
        }
        if let Ok((watcher, mut changes)) = FolderWatcher::start() {
            let task = cx.spawn(async move |this, cx| {
                while let Some(folders) = changes.next().await {
                    if this
                        .update(cx, |this, cx| this.on_folders_changed(folders, cx))
                        .is_err()
                    {
                        return;
                    }
                }
            });
            self.watching = Some(Watching {
                watcher,
                _task: task,
            });
        }
    }

    fn sync_watches(&mut self) {
        let folders = match &self.tree {
            Some(tree) => tree.open_folders(),
            None => self.target_root.iter().cloned().collect(),
        };
        if let Some(watching) = &mut self.watching {
            watching.watcher.sync(&folders);
        }
    }

    fn on_folders_changed(&mut self, folders: Vec<PathBuf>, cx: &mut Context<Self>) {
        let Some(root) = self.tree.as_ref().map(|tree| tree.root().to_owned()) else {
            return;
        };
        for folder in folders {
            if folder == root {
                self.request_root(root.clone(), cx);
            } else if self.tree.as_ref().is_some_and(|tree| tree.is_open(&folder)) {
                self.request_folder(folder, false, cx);
            }
        }
    }

    fn request_root(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.next_request += 1;
        let req_id = self.next_request;
        self.root_request = Some(req_id);
        let generation = self.root_generation;
        let reader = self.reader.clone();
        let read_path = root.clone();
        let (sender, receiver) = oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("benterm-file-read".into())
            .spawn(move || {
                let entries = reader.read_folder(&read_path);
                let _ = sender.send(entries);
            });
        if let Err(error) = spawned {
            let entries = Err(format!("folder reader thread failed ({})", error.kind()));
            self.apply_root_result(root, generation, req_id, entries, cx);
            return;
        }
        cx.spawn(async move |this, cx| {
            let Ok(entries) = receiver.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.apply_root_result(root, generation, req_id, entries, cx);
            });
        })
        .detach();
    }

    fn apply_root_result(
        &mut self,
        root: PathBuf,
        generation: u64,
        req_id: u64,
        entries: Result<Vec<FolderEntry>, String>,
        cx: &mut Context<Self>,
    ) {
        if self.root_generation != generation
            || self.target_root.as_deref() != Some(root.as_path())
            || self.root_request != Some(req_id)
        {
            return;
        }
        self.root_request = None;
        match &mut self.tree {
            Some(tree) if tree.root() == root => {
                tree.apply_folder(&root, entries);
            }
            _ => {
                self.tree = Some(FileTree::from_entries(&root, entries));
                self.opened = None;
            }
        }
        self.sync_watches();
        self.advance_pending_reveal(cx);
        cx.notify();
    }

    fn request_folder(&mut self, folder: PathBuf, open: bool, cx: &mut Context<Self>) {
        let Some(root) = self.target_root.clone() else {
            return;
        };
        self.next_request += 1;
        let req_id = self.next_request;
        self.folder_requests.insert(folder.clone(), req_id);
        let generation = self.root_generation;
        let reader = self.reader.clone();
        if open && let Some(watching) = &mut self.watching {
            watching.watcher.watch_one(&folder);
        }
        let read_path = folder.clone();
        let (sender, receiver) = oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("benterm-file-read".into())
            .spawn(move || {
                let entries = reader.read_folder(&read_path);
                let _ = sender.send(entries);
            });
        if let Err(error) = spawned {
            let entries = Err(format!("folder reader thread failed ({})", error.kind()));
            self.apply_folder_result(root, generation, folder, req_id, open, entries, cx);
            return;
        }
        cx.spawn(async move |this, cx| {
            let Ok(entries) = receiver.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.apply_folder_result(root, generation, folder, req_id, open, entries, cx);
            });
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_folder_result(
        &mut self,
        root: PathBuf,
        generation: u64,
        folder: PathBuf,
        req_id: u64,
        open: bool,
        entries: Result<Vec<FolderEntry>, String>,
        cx: &mut Context<Self>,
    ) {
        if self.root_generation != generation
            || self.target_root.as_deref() != Some(root.as_path())
            || self.folder_requests.get(&folder) != Some(&req_id)
        {
            return;
        }
        self.folder_requests.remove(&folder);
        let Some(tree) = &mut self.tree else {
            return;
        };
        if tree.root() != root {
            return;
        }
        if !open && !tree.is_open(&folder) {
            return;
        }
        if tree.apply_folder(&folder, entries) {
            self.sync_watches();
            self.advance_pending_reveal(cx);
            cx.notify();
        }
    }

    fn set_theme(&mut self, theme: &Theme, cx: &mut Context<Self>) {
        if &self.theme != theme {
            if self.theme.terminal.font_families != theme.terminal.font_families {
                self.mono = crate::theme::installed_mono_font(&theme.terminal, cx);
            }
            self.theme = theme.clone();
            cx.notify();
        }
    }

    /// Reveals and opens `path` without touching the file system on the UI
    /// thread:
    /// - If `path` is a file, expands its ancestor folders under the root,
    ///   selects it in the tree, and opens its preview.
    /// - If `path` is a folder under the root, expands its ancestors and the
    ///   folder itself (leaving it open if already expanded) and selects it.
    /// - If `path` is a folder outside the root, switches the tree root to
    ///   `path`.
    pub fn reveal_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.reveal_request += 1;
        let req_id = self.reveal_request;
        let probe = path.clone();
        cx.spawn(async move |this, cx| {
            let is_dir = cx
                .background_spawn(async move {
                    std::fs::metadata(&probe).map(|meta| meta.is_dir()).ok()
                })
                .await;
            let Some(is_dir) = is_dir else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.reveal_request != req_id {
                    return;
                }
                this.pending_reveal = Some(PendingReveal { path, is_dir });
                this.advance_pending_reveal(cx);
            });
        })
        .detach();
    }

    fn advance_pending_reveal(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_reveal.clone() else {
            return;
        };
        let tree_matches_target = self
            .tree
            .as_ref()
            .zip(self.target_root.as_ref())
            .is_some_and(|(tree, target)| tree.root() == target);
        if !tree_matches_target {
            if self.target_root.is_some() && self.root_request.is_some() {
                // Wait for `apply_root_result` to finish loading the root.
                return;
            }
            if pending.is_dir {
                self.pending_reveal = None;
                self.follow(Some(pending.path), cx);
            } else if let Some(parent) = pending.path.parent().map(Path::to_owned) {
                self.follow(Some(parent), cx);
                self.pending_reveal = Some(pending);
            } else {
                self.pending_reveal = None;
            }
            return;
        }

        let Some(root) = self.tree.as_ref().map(|tree| tree.root().to_owned()) else {
            return;
        };

        if pending.path == root {
            self.pending_reveal = None;
            cx.notify();
            return;
        }

        if let Ok(rel) = pending.path.strip_prefix(&root) {
            let mut folders_to_open = Vec::new();
            let mut current = root.clone();
            let components: Vec<_> = rel.components().collect();
            let folder_count = if pending.is_dir {
                components.len()
            } else {
                components.len().saturating_sub(1)
            };
            for component in &components[..folder_count] {
                current.push(component.as_os_str());
                folders_to_open.push(current.clone());
            }

            for folder in folders_to_open {
                let is_open = self.tree.as_ref().is_some_and(|tree| tree.is_open(&folder));
                if is_open {
                    continue;
                }
                let present = self.tree.as_ref().is_some_and(|tree| {
                    tree.rows()
                        .iter()
                        .any(|row| row.path == folder && row.kind == EntryKind::Folder)
                });
                if present {
                    if !self.folder_requests.contains_key(&folder) {
                        self.request_folder(folder, true, cx);
                    }
                    return;
                }
                break;
            }

            if let Some(tree) = &mut self.tree {
                tree.select(&pending.path);
            }
            self.scroll_to_selected();
            self.pending_reveal = None;
            if !pending.is_dir {
                self.request_preview(pending.path, cx);
            }
            cx.notify();
        } else if pending.is_dir {
            self.pending_reveal = None;
            self.follow(Some(pending.path), cx);
        } else {
            self.pending_reveal = None;
            self.request_preview(pending.path, cx);
            cx.notify();
        }
    }

    /// Selects `path`, and opens it: a folder in the tree, opened or
    /// closed, or a file to show. Reads happen off the UI thread.
    pub fn open_path(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(tree) = &mut self.tree else {
            return;
        };
        tree.select(path);
        let row = tree.rows().into_iter().find(|row| row.path == path);
        match row {
            Some(row) if row.kind == EntryKind::Folder => {
                if row.open {
                    self.close_folder(path, cx);
                } else {
                    self.request_folder(path.to_owned(), true, cx);
                }
            }
            _ => {
                self.request_preview(path.to_owned(), cx);
            }
        }
        self.scroll_to_selected();
        cx.notify();
    }

    fn close_folder(&mut self, folder: &Path, cx: &mut Context<Self>) {
        if let Some(tree) = &mut self.tree {
            tree.close(folder);
        }
        self.folder_requests
            .retain(|path, _| !path.starts_with(folder));
        self.sync_watches();
        cx.notify();
    }

    fn request_preview(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(root) = self.target_root.clone() else {
            return;
        };
        self.preview_request += 1;
        let req_id = self.preview_request;
        self.preview_target = Some(path.clone());
        let generation = self.root_generation;
        let reader = self.reader.clone();
        let read_path = path.clone();
        let (sender, receiver) = oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("benterm-file-preview".into())
            .spawn(move || {
                let preview = reader.read_preview(&read_path, PREVIEW_LIMIT);
                let lines = match &preview.content {
                    Content::Text(text) => text
                        .lines()
                        .map(|line| line.replace('\t', "    ").into())
                        .collect(),
                    _ => Vec::new(),
                };
                let _ = sender.send((preview, lines));
            });
        if let Err(error) = spawned {
            let preview = Preview {
                content: Content::Unreadable(format!(
                    "preview reader thread failed ({})",
                    error.kind()
                )),
                size: 0,
                truncated: false,
            };
            self.apply_preview_result(root, generation, path, req_id, preview, Vec::new(), cx);
            return;
        }
        cx.spawn(async move |this, cx| {
            let Ok((preview, lines)) = receiver.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.apply_preview_result(root, generation, path, req_id, preview, lines, cx);
            });
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_preview_result(
        &mut self,
        root: PathBuf,
        generation: u64,
        path: PathBuf,
        req_id: u64,
        preview: Preview,
        lines: Vec<SharedString>,
        cx: &mut Context<Self>,
    ) {
        if self.root_generation != generation
            || self.target_root.as_deref() != Some(root.as_path())
            || self.preview_target.as_deref() != Some(path.as_path())
            || self.preview_request != req_id
        {
            return;
        }
        self.text_scroll = UniformListScrollHandle::new();
        self.markdown_scroll = ScrollHandle::new();
        self.opened = Some(Opened {
            path,
            preview,
            lines,
        });
        cx.notify();
    }

    fn close_file(&mut self, cx: &mut Context<Self>) {
        self.preview_request += 1;
        self.preview_target = None;
        self.opened = None;
        cx.notify();
    }

    fn open_selected_folder(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self.tree.as_ref().and_then(FileTree::selected_row) else {
            return;
        };
        if row.kind != EntryKind::Folder {
            return;
        }
        if !row.open {
            self.request_folder(row.path, true, cx);
        } else if let Some(tree) = &mut self.tree
            && tree.select_first_child_of_selected()
        {
            self.scroll_to_selected();
            cx.notify();
        }
    }

    fn close_selected_folder(&mut self, cx: &mut Context<Self>) {
        let closed = self
            .tree
            .as_ref()
            .and_then(FileTree::selected_row)
            .filter(|row| row.kind == EntryKind::Folder && row.open)
            .map(|row| row.path);
        if let Some(tree) = &mut self.tree {
            tree.close_selected();
        }
        if let Some(folder) = closed {
            self.folder_requests
                .retain(|path, _| !path.starts_with(&folder));
            self.sync_watches();
        }
        self.scroll_to_selected();
        cx.notify();
    }

    fn scroll_to_selected(&self) {
        if let Some(tree) = &self.tree
            && let Some(selected) = tree.selected()
            && let Some(index) = tree.rows().iter().position(|row| row.path == selected)
        {
            self.tree_scroll
                .scroll_to_item(index, ScrollStrategy::Nearest);
        }
    }

    fn change_tree(&mut self, change: impl FnOnce(&mut FileTree), cx: &mut Context<Self>) {
        if let Some(tree) = &mut self.tree {
            change(tree);
            self.scroll_to_selected();
            cx.notify();
        }
    }

    fn open_selected_file(&mut self, cx: &mut Context<Self>) {
        if let Some(selected) = self.selected().map(Path::to_owned) {
            self.open_path(&selected, cx);
        }
    }

    /// The font of the tree and the preview. Looked up when the theme
    /// changes, as the lookup is too slow for a redraw.
    fn mono_font(&self) -> SharedString {
        self.mono.clone()
    }

    fn tree_rows(&self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = &self.theme.sidebar;
        let selected = self.selected();
        let rows = self.rows();
        range
            .filter_map(|index| rows.get(index).map(|row| (index, row)))
            .map(|(index, row)| {
                let path = row.path.clone();
                let mark = match (row.kind, row.open) {
                    (EntryKind::Folder, true) => "▾",
                    (EntryKind::Folder, false) => "▸",
                    (EntryKind::File, _) => "",
                };
                let active = selected == Some(row.path.as_path());
                let guide_color = if active {
                    hsla(theme.item_active_indicator)
                } else {
                    crate::design::subtle_border(theme.border, theme.background)
                };
                let guides = (0..row.depth).map(|level| {
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(px(PADDING + INDENT * level as f32 + 3.))
                        .w(px(1.))
                        .bg(guide_color)
                });
                div()
                    .id(("file-row", index))
                    .debug_selector(move || format!("file-row-{index}"))
                    .relative()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .w_full()
                    .py(px(2.))
                    .pl(px(PADDING + INDENT * row.depth as f32))
                    .pr(px(PADDING))
                    .rounded(px(crate::theme::RADIUS_CONTROL))
                    .cursor_pointer()
                    .text_color(hsla(theme.item_text))
                    .children(guides)
                    .map(|row| {
                        if active {
                            row.bg(hsla(theme.item_active_background))
                        } else {
                            row.hover(|row| row.bg(hsla(theme.item_hover_background)))
                        }
                    })
                    .child(
                        div()
                            .flex_none()
                            .w(px(8.))
                            .text_color(hsla(theme.item_secondary_text))
                            .child(mark),
                    )
                    .child(match row.kind {
                        EntryKind::Folder => crate::icons::folder(
                            row.open,
                            hsla(if active {
                                theme.item_active_indicator
                            } else {
                                theme.section_header_text
                            }),
                            13.,
                        )
                        .into_any_element(),
                        EntryKind::File => crate::icons::file(hsla(theme.item_secondary_text), 13.)
                            .into_any_element(),
                    })
                    .child(div().flex_1().min_w_0().truncate().child(row.name.clone()))
                    .children(row.error.as_ref().map(|_| {
                        div()
                            .flex_none()
                            .text_color(hsla(theme.item_secondary_text))
                            .child("cannot read")
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            window.focus(&this.focus, cx);
                            this.open_path(&path, cx);
                        }),
                    )
                    .into_any_element()
            })
            .collect()
    }

    /// The tree, as the list of files in the mock's diff review: a heading
    /// with the root's name, and a row for each entry.
    fn tree(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = &self.theme.sidebar;
        let name = self.root().map_or_else(String::new, |root| {
            root.file_name().map_or_else(
                || root.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            )
        });
        let header = div()
            .flex()
            .flex_none()
            .justify_between()
            .gap(px(8.))
            .pt(px(10.))
            .pb(px(6.))
            .px(px(PADDING))
            .text_size(px(crate::theme::small_text(theme.font_size)))
            .text_color(hsla(theme.section_header_text))
            .child("FILES")
            .child(div().min_w_0().truncate().child(name));
        let message = |text: String| {
            div()
                .px(px(PADDING))
                .py(px(6.))
                .text_color(hsla(theme.item_secondary_text))
                .child(text)
        };
        let body = match &self.tree {
            None => message("This pane has no folder.".into()).into_any_element(),
            Some(tree) => match tree.error() {
                Some(error) => {
                    message(format!("Cannot read the folder: {error}")).into_any_element()
                }
                None => uniform_list(
                    "file-tree",
                    tree.rows().len(),
                    cx.processor(|this, range, _, cx| this.tree_rows(range, cx)),
                )
                .flex_1()
                .font_family(self.mono_font())
                .text_size(px(crate::theme::small_text(theme.font_size)))
                .track_scroll(&self.tree_scroll)
                .into_any_element(),
            },
        };
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(TREE_WIDTH))
            .h_full()
            .bg(hsla(theme.background))
            .border_l_1()
            .border_color(hsla(theme.border))
            .child(header)
            .child(body)
    }

    fn text_lines(&self, range: Range<usize>) -> Vec<AnyElement> {
        let Some(opened) = &self.opened else {
            return Vec::new();
        };
        range
            .filter_map(|index| opened.lines.get(index))
            .map(|line| {
                div()
                    .px(px(14.))
                    .whitespace_nowrap()
                    .child(line.clone())
                    .into_any_element()
            })
            .collect()
    }

    /// The file being shown, as the mock's diff review shows a file: its
    /// name in a bar above, its contents below.
    fn file(&self, opened: &Opened, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let sidebar = &self.theme.sidebar;
        let terminal = &self.theme.terminal;
        let mono = self.mono_font();
        let name = self
            .root()
            .and_then(|root| opened.path.strip_prefix(root).ok())
            .unwrap_or(&opened.path)
            .display()
            .to_string();
        let close = div()
            .id("file-close")
            .debug_selector(|| "file-close".into())
            .flex()
            .items_center()
            .justify_center()
            .size(px(20.))
            .rounded(px(crate::theme::RADIUS_CONTROL))
            .cursor_pointer()
            .hover(|close| close.bg(hsla(sidebar.item_hover_background)))
            .child(crate::icons::close(hsla(sidebar.item_secondary_text), 12.))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.close_file(cx)),
            );
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .px(px(14.))
            .py(px(8.))
            .border_b_1()
            .border_color(hsla(sidebar.border))
            .text_color(hsla(sidebar.item_secondary_text))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(mono.clone())
                    .text_color(hsla(sidebar.item_text))
                    .child(name),
            )
            .child(size_label(opened.preview.size))
            .child(close);
        let message = |selector: &'static str, text: String| {
            div()
                .debug_selector(move || selector.into())
                .p(px(14.))
                .text_color(hsla(sidebar.item_secondary_text))
                .child(text)
                .into_any_element()
        };
        let body = match &opened.preview.content {
            Content::Text(_) => uniform_list(
                "file-text",
                opened.lines.len(),
                cx.processor(|this, range, _, _| this.text_lines(range)),
            )
            .flex_1()
            .py(px(8.))
            .font_family(mono)
            .text_size(px(terminal.font_size))
            .text_color(hsla(terminal.foreground))
            .track_scroll(&self.text_scroll)
            .into_any_element(),
            Content::Markdown(blocks) => div()
                .id("file-markdown")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.markdown_scroll)
                .px(px(18.))
                .py(px(12.))
                .flex()
                .flex_col()
                .gap(px(8.))
                .text_color(hsla(sidebar.item_text))
                .children(
                    blocks
                        .iter()
                        .map(|block| render_block(block, sidebar, &mono)),
                )
                .into_any_element(),
            Content::NotText => message(
                "file-preview-not-text",
                "This is not a text file, so its contents are not shown.".into(),
            ),
            Content::NotRegularFile => message(
                "file-preview-not-regular",
                "This is not a regular file, so it is not opened.".into(),
            ),
            Content::Unreadable(error) => message(
                "file-preview-unreadable",
                format!("Cannot read the file: {error}"),
            ),
        };
        let truncated = opened.preview.truncated.then(|| {
            div()
                .debug_selector(|| "file-preview-truncated".into())
                .flex_none()
                .px(px(14.))
                .py(px(6.))
                .bg(hsla(sidebar.background))
                .border_t_1()
                .border_color(hsla(sidebar.border))
                .text_color(hsla(self.theme_warning()))
                .child(format!(
                    "Showing only the first {} of {}.",
                    size_label(PREVIEW_LIMIT as u64),
                    size_label(opened.preview.size)
                ))
        });
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(hsla(terminal.background))
            .border_l_1()
            .border_color(hsla(sidebar.border))
            .child(header)
            .child(body)
            .children(truncated)
    }

    /// The color of the line that says the file is cut short: the yellow
    /// of the terminal's colors, as the mock's warnings are.
    fn theme_warning(&self) -> benterm_core::Rgb {
        self.theme.terminal.ansi[3]
    }
}

/// A size in bytes as people read it.
fn size_label(bytes: u64) -> String {
    const KB: f64 = 1024.;
    let value = bytes as f64;
    if value < KB {
        format!("{bytes} B")
    } else if value < KB * KB {
        format!("{:.0} KB", value / KB)
    } else {
        format!("{:.1} MB", value / KB / KB)
    }
}

impl Render for FileBrowser {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme.sidebar;
        let file = self.opened.as_ref().map(|opened| self.file(opened, cx));
        div()
            .id("file-browser")
            .debug_selector(|| "file-browser".into())
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &SelectNextFile, _, cx| {
                this.change_tree(FileTree::select_next, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectPreviousFile, _, cx| {
                this.change_tree(FileTree::select_previous, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenFolder, _, cx| this.open_selected_folder(cx)))
            .on_action(cx.listener(|this, _: &CloseFolder, _, cx| this.close_selected_folder(cx)))
            .on_action(cx.listener(|this, _: &OpenFile, _, cx| this.open_selected_file(cx)))
            .on_action(
                cx.listener(|_, _: &LeaveFileBrowser, _, cx| cx.emit(FileBrowserEvent::Leave)),
            )
            .size_full()
            .flex()
            .font_family(theme.font_family.clone())
            .text_size(px(theme.font_size))
            .children(file)
            .child(self.tree(cx))
    }
}

/// The file browser while it is shown.
pub(crate) struct OpenFileBrowser {
    view: Entity<FileBrowser>,
    /// The last `followed_folder` passed to `follow`, so opening a folder
    /// outside the pane's root is not immediately overwritten on redraw.
    followed: Option<PathBuf>,
    _events: Subscription,
}

impl BentermWindow {
    /// The file browser, while it is shown.
    pub fn file_browser(&self) -> Option<Entity<FileBrowser>> {
        self.files.as_ref().map(|open| open.view.clone())
    }

    /// The folder the file browser follows: that of the program the
    /// focused pane started, which is the shell's.
    fn followed_folder(&self, cx: &App) -> Option<PathBuf> {
        self.focused_terminal(cx)?.read(cx).working_directory()
    }

    fn ensure_file_browser(
        &mut self,
        initial_root: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<FileBrowser> {
        if let Some(open) = &self.files {
            return open.view.clone();
        }
        let theme = self.settings.theme().clone();
        let followed = self.followed_folder(cx);
        let root = initial_root.or_else(|| followed.clone());
        let view = cx.new(|cx| {
            let mut browser = FileBrowser::new(theme, cx);
            browser.follow(root, cx);
            browser
        });
        let events = cx.subscribe_in(&view, window, |this, _, event, window, cx| match event {
            FileBrowserEvent::Leave => this.focus_selected(window, cx),
        });
        self.files = Some(OpenFileBrowser {
            view: view.clone(),
            followed,
            _events: events,
        });
        view
    }

    /// Opens `path` in the file browser, opening the panel first if it is
    /// closed. Keeps keyboard focus where it is.
    pub(crate) fn open_path_in_file_browser(
        &mut self,
        pane: benterm_layout::PaneId,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let initial_root = self.followed_folder(cx).or_else(|| {
            self.terminal_with_id(pane, cx).and_then(|view| {
                let view = view.read(cx);
                view.foreground_working_directory()
                    .or_else(|| view.working_directory())
            })
        });
        let view = self.ensure_file_browser(initial_root, window, cx);
        view.update(cx, |browser, cx| browser.reveal_path(path, cx));
        cx.notify();
    }

    /// Shows the file browser with the keyboard in it, or hides it and
    /// gives the keyboard back to the focused pane if it was there.
    pub fn toggle_file_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(open) = self.files.take() {
            if open.view.read(cx).focus.contains_focused(window, cx) {
                self.focus_selected(window, cx);
            }
            cx.notify();
            return;
        }
        let view = self.ensure_file_browser(None, window, cx);
        let focus = view.read(cx).focus.clone();
        window.focus(&focus, cx);
        cx.notify();
    }

    /// The panel right of the tabs, while it is shown. It is brought up to
    /// date with the focused pane's folder and the theme first.
    pub(crate) fn file_browser_panel(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let root = self.followed_folder(cx);
        let open = self.files.as_mut()?;
        let root_changed = open.followed != root;
        if root_changed {
            open.followed = root.clone();
        }
        let view = open.view.clone();
        let theme = self.settings.theme();
        view.update(cx, |browser, cx| {
            if root_changed {
                browser.follow(root, cx);
            }
            browser.set_theme(theme, cx);
        });
        let mut style = StyleRefinement::default()
            .flex_none()
            .h_full()
            .min_w(px(TREE_WIDTH));
        style = if view.read(cx).opened.is_some() {
            style.w(relative(OPEN_SHARE))
        } else {
            style.w(px(TREE_WIDTH))
        };
        Some(view.cached(style).into_any_element())
    }
}
