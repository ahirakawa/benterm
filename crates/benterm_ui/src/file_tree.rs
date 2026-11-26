//! The tree of a folder that the file browser shows, which knows nothing
//! about the screen.
//!
//! A folder is read when it is opened, and not before. Nothing here
//! writes to the file system: folders are listed and nothing else.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Whether an entry of the tree is a folder or something else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Folder,
    File,
}

/// One entry read from a folder, before it is placed into a [`FileTree`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderEntry {
    pub path: PathBuf,
    pub name: String,
    pub kind: EntryKind,
}

/// A row of the tree as it is shown: the entries of the root, and below
/// each open folder its own entries, one level deeper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub path: PathBuf,
    pub name: String,
    /// 0 for the entries of the root.
    pub depth: usize,
    pub kind: EntryKind,
    pub open: bool,
    /// Why an open folder could not be read.
    pub error: Option<String>,
}

/// An entry of a folder, with its own entries once it has been read.
#[derive(Debug)]
struct Node {
    path: PathBuf,
    name: String,
    kind: EntryKind,
    open: bool,
    /// `None` until the folder is read.
    entries: Option<Entries>,
}

type Entries = Result<Vec<Node>, String>;

/// The entries of `folder`, folders first, each group by name. A symbolic
/// link counts as what it points to; one that points nowhere is a file.
pub fn read_folder(folder: &Path) -> Result<Vec<FolderEntry>, String> {
    let listing = std::fs::read_dir(folder).map_err(|error| error.to_string())?;
    let mut entries: Vec<FolderEntry> = listing
        .filter_map(Result::ok)
        .map(|entry| {
            let path = entry.path();
            let kind = if std::fs::metadata(&path).is_ok_and(|meta| meta.is_dir()) {
                EntryKind::Folder
            } else {
                EntryKind::File
            };
            FolderEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                path,
                kind,
            }
        })
        .collect();
    entries.sort_by(|a, b| {
        (a.kind != EntryKind::Folder)
            .cmp(&(b.kind != EntryKind::Folder))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(entries)
}

fn nodes_from_entries(entries: Result<Vec<FolderEntry>, String>) -> Entries {
    merge_entries(None, entries)
}

fn merge_entries(existing: Option<Entries>, entries: Result<Vec<FolderEntry>, String>) -> Entries {
    let mut previous: HashMap<PathBuf, (bool, Option<Entries>)> = match existing {
        Some(Ok(nodes)) => nodes
            .into_iter()
            .filter(|node| node.kind == EntryKind::Folder)
            .map(|node| (node.path, (node.open, node.entries)))
            .collect(),
        _ => HashMap::new(),
    };
    entries.map(|entries| {
        entries
            .into_iter()
            .map(|entry| {
                let (open, children) = if entry.kind == EntryKind::Folder {
                    previous.remove(&entry.path).unwrap_or((false, None))
                } else {
                    (false, None)
                };
                Node {
                    path: entry.path,
                    name: entry.name,
                    kind: entry.kind,
                    open,
                    entries: children,
                }
            })
            .collect()
    })
}

fn find_node<'a>(nodes: &'a [Node], path: &Path) -> Option<&'a Node> {
    let node = nodes.iter().find(|node| path.starts_with(&node.path))?;
    if node.path == path {
        return Some(node);
    }
    match &node.entries {
        Some(Ok(entries)) => find_node(entries, path),
        _ => None,
    }
}

fn collect_open_folders(nodes: &[Node], out: &mut Vec<PathBuf>) {
    for node in nodes {
        if node.kind == EntryKind::Folder && node.open {
            out.push(node.path.clone());
            if let Some(Ok(entries)) = &node.entries {
                collect_open_folders(entries, out);
            }
        }
    }
}

fn find_mut<'a>(nodes: &'a mut [Node], path: &Path) -> Option<&'a mut Node> {
    let node = nodes.iter_mut().find(|node| path.starts_with(&node.path))?;
    if node.path == path {
        return Some(node);
    }
    match &mut node.entries {
        Some(Ok(entries)) => find_mut(entries, path),
        _ => None,
    }
}

fn push_rows(nodes: &[Node], depth: usize, rows: &mut Vec<Row>) {
    for node in nodes {
        let error = match (&node.entries, node.open) {
            (Some(Err(error)), true) => Some(error.clone()),
            _ => None,
        };
        rows.push(Row {
            path: node.path.clone(),
            name: node.name.clone(),
            depth,
            kind: node.kind,
            open: node.open,
            error,
        });
        if let (true, Some(Ok(entries))) = (node.open, &node.entries) {
            push_rows(entries, depth + 1, rows);
        }
    }
}

/// The tree under one folder, its root.
#[derive(Debug)]
pub struct FileTree {
    root: PathBuf,
    entries: Entries,
    selected: Option<PathBuf>,
}

impl FileTree {
    /// The tree of `root`, with the root read and every folder in it
    /// closed.
    pub fn new(root: &Path) -> Self {
        Self::from_entries(root, read_folder(root))
    }

    /// The tree of `root` built from entries already read off the UI thread.
    pub fn from_entries(root: &Path, entries: Result<Vec<FolderEntry>, String>) -> Self {
        Self {
            root: root.to_owned(),
            entries: nodes_from_entries(entries),
            selected: None,
        }
    }

    /// Replaces the entries of `folder` with `entries`, keeping any already
    /// open subfolders and their entries. Returns whether `folder` was present
    /// in the tree.
    pub fn apply_folder(
        &mut self,
        folder: &Path,
        entries: Result<Vec<FolderEntry>, String>,
    ) -> bool {
        if folder == self.root {
            let existing = std::mem::replace(&mut self.entries, Ok(Vec::new()));
            self.entries = merge_entries(Some(existing), entries);
            return true;
        }
        if let Some(node) = self.node_mut(folder)
            && node.kind == EntryKind::Folder
        {
            let existing = node.entries.take();
            node.entries = Some(merge_entries(existing, entries));
            node.open = true;
            return true;
        }
        false
    }

    /// Whether `folder` is currently open (the root is always open).
    pub fn is_open(&self, folder: &Path) -> bool {
        if folder == self.root {
            return true;
        }
        match &self.entries {
            Ok(entries) => find_node(entries, folder).is_some_and(|node| node.open),
            Err(_) => false,
        }
    }

    /// The root and every currently open folder in the tree.
    pub fn open_folders(&self) -> Vec<PathBuf> {
        let mut folders = vec![self.root.clone()];
        if let Ok(entries) = &self.entries {
            collect_open_folders(entries, &mut folders);
        }
        folders
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Why the root could not be read, if it could not.
    pub fn error(&self) -> Option<&str> {
        self.entries.as_ref().err().map(String::as_str)
    }

    /// The rows from top to bottom.
    pub fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        if let Ok(entries) = &self.entries {
            push_rows(entries, 0, &mut rows);
        }
        rows
    }

    fn node_mut(&mut self, path: &Path) -> Option<&mut Node> {
        match &mut self.entries {
            Ok(entries) => find_mut(entries, path),
            Err(_) => None,
        }
    }

    /// Whether the contents of `folder` have been read. The root always
    /// has been.
    pub fn is_read(&self, folder: &Path) -> bool {
        if folder == self.root {
            return true;
        }
        fn find<'a>(nodes: &'a [Node], path: &Path) -> Option<&'a Node> {
            let node = nodes.iter().find(|node| path.starts_with(&node.path))?;
            match &node.entries {
                _ if node.path == path => Some(node),
                Some(Ok(entries)) => find(entries, path),
                _ => None,
            }
        }
        match &self.entries {
            Ok(entries) => find(entries, folder).is_some_and(|node| node.entries.is_some()),
            Err(_) => false,
        }
    }

    /// Opens `folder`, reading its contents now.
    pub fn open(&mut self, folder: &Path) {
        if let Some(node) = self.node_mut(folder)
            && node.kind == EntryKind::Folder
        {
            node.entries = Some(nodes_from_entries(read_folder(&node.path)));
            node.open = true;
        }
    }

    /// Closes `folder`. Its contents are read again when it opens. A
    /// selection inside it moves to the folder.
    pub fn close(&mut self, folder: &Path) {
        if let Some(node) = self.node_mut(folder) {
            node.open = false;
            node.entries = None;
            if self
                .selected
                .as_deref()
                .is_some_and(|selected| selected.starts_with(folder))
            {
                self.selected = Some(folder.to_owned());
            }
        }
    }

    /// The selected row's path, if a row is selected.
    pub fn selected(&self) -> Option<&Path> {
        self.selected.as_deref()
    }

    pub fn selected_row(&self) -> Option<Row> {
        let selected = self.selected.as_deref()?;
        self.rows().into_iter().find(|row| row.path == selected)
    }

    /// Selects the row of `path`, if there is one.
    pub fn select(&mut self, path: &Path) {
        if self.rows().iter().any(|row| row.path == path) {
            self.selected = Some(path.to_owned());
        }
    }

    fn selected_index(&self, rows: &[Row]) -> Option<usize> {
        let selected = self.selected.as_deref()?;
        rows.iter().position(|row| row.path == selected)
    }

    /// Selects the row below, or the first if none is selected.
    pub fn select_next(&mut self) {
        let rows = self.rows();
        let index = match self.selected_index(&rows) {
            Some(index) => (index + 1).min(rows.len() - 1),
            None => 0,
        };
        self.selected = rows.get(index).map(|row| row.path.clone());
    }

    /// Selects the row above, or the first if none is selected.
    pub fn select_previous(&mut self) {
        let rows = self.rows();
        let index = self
            .selected_index(&rows)
            .map_or(0, |index| index.saturating_sub(1));
        self.selected = rows.get(index).map(|row| row.path.clone());
    }

    /// Moves the selection into the first child of the currently selected open
    /// folder, without performing any I/O. Returns whether the selection moved.
    pub fn select_first_child_of_selected(&mut self) -> bool {
        let rows = self.rows();
        let Some(index) = self.selected_index(&rows) else {
            return false;
        };
        let row = &rows[index];
        if row.kind != EntryKind::Folder || !row.open {
            return false;
        }
        if let Some(child) = rows.get(index + 1).filter(|next| next.depth > row.depth) {
            self.selected = Some(child.path.clone());
            return true;
        }
        false
    }

    /// Opens the selected folder, or moves into it if it is open.
    pub fn open_selected(&mut self) {
        let rows = self.rows();
        let Some(index) = self.selected_index(&rows) else {
            return;
        };
        let row = &rows[index];
        if row.kind != EntryKind::Folder {
            return;
        }
        if !row.open {
            self.open(&row.path.clone());
        } else if let Some(child) = rows.get(index + 1).filter(|next| next.depth > row.depth) {
            self.selected = Some(child.path.clone());
        }
    }

    /// Closes the selected folder, or moves to the folder the selected
    /// row is in.
    pub fn close_selected(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        if row.kind == EntryKind::Folder && row.open {
            self.close(&row.path);
        } else if row.depth > 0
            && let Some(parent) = row.path.parent()
        {
            self.selected = Some(parent.to_owned());
        }
    }
}
