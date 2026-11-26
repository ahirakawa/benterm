//! The workspaces of a window: their order, names and colors, and which of
//! them is selected.
//!
//! The list knows nothing about what a workspace shows. The app keeps a
//! group of tabs in each workspace; tests keep plain numbers.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// Identifies a workspace. Ids come from one counter for the whole process,
/// so no two workspaces share an id and an id is never reused after its
/// workspace closes. This lets an id name a workspace from outside the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkspaceId(u64);

impl WorkspaceId {
    /// An id no workspace in the process has had yet.
    fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        WorkspaceId(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    /// The number behind the id.
    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for WorkspaceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Which of the theme's workspace colors a workspace is drawn with. The
/// list hands out the place in the theme, not the color itself, so a
/// workspace keeps its place when the theme changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkspaceColor(u8);

impl WorkspaceColor {
    /// How many colors a theme has for workspaces.
    pub const COUNT: usize = 6;

    /// Every color, in the order of [`index`](Self::index).
    pub fn all() -> [Self; Self::COUNT] {
        std::array::from_fn(|index| Self(index as u8))
    }

    /// The color at a place in the theme, or `None` at or past
    /// [`COUNT`](Self::COUNT).
    pub fn from_index(index: usize) -> Option<Self> {
        Self::all().get(index).copied()
    }

    /// The place of the color in the theme, below [`COUNT`](Self::COUNT).
    pub fn index(self) -> usize {
        usize::from(self.0)
    }
}

/// Why an operation on the list was refused. A refused operation leaves the
/// list unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceError {
    UnknownWorkspace(WorkspaceId),
    /// The list always has a workspace, so its last one cannot be closed.
    /// The window closes instead.
    LastWorkspace,
    /// A name is shown on one line, so it needs a character that is not a
    /// space and has no control characters.
    InvalidName,
}

impl fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownWorkspace(id) => write!(f, "no workspace has the id {id}"),
            Self::LastWorkspace => f.write_str("the last workspace cannot be closed"),
            Self::InvalidName => {
                f.write_str("a workspace name needs a character and no control characters")
            }
        }
    }
}

impl std::error::Error for WorkspaceError {}

/// Whether a character cannot be in a name, which is one line: a control
/// character, or one of Unicode's line and paragraph separators.
pub fn ends_a_line(ch: char) -> bool {
    ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}')
}

/// A name as it is kept: without the spaces around it, or `None` if it
/// cannot be shown.
fn valid_name(name: &str) -> Option<&str> {
    let name = name.trim();
    (!name.is_empty() && !name.chars().any(ends_a_line)).then_some(name)
}

/// Whether `name` has the form of the numbered name a workspace gets
/// when it is made without one, "Workspace N". A saved session keeps
/// only the names, so this is how a workspace nobody named is known
/// again.
pub fn is_numbered_name(name: &str) -> bool {
    name.strip_prefix("Workspace ").is_some_and(|number| {
        !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
    })
}

/// A workspace in a list. It cannot be cloned, since a clone would be a
/// second workspace with the same id.
#[derive(Debug)]
pub struct Workspace<T> {
    id: WorkspaceId,
    name: String,
    /// Whether the name was given, and is not the numbered one.
    named: bool,
    color: WorkspaceColor,
    content: T,
}

impl<T> Workspace<T> {
    pub fn id(&self) -> WorkspaceId {
        self.id
    }

    /// The name shown for the workspace. Names need not differ; the id is
    /// what tells workspaces apart.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether the workspace was given its name, when it was made or
    /// later. One that was not has the numbered name "Workspace N", which
    /// a view may show something better in place of.
    pub fn is_named(&self) -> bool {
        self.named
    }

    /// The color given when the workspace was made. It never changes.
    pub fn color(&self) -> WorkspaceColor {
        self.color
    }

    /// What the workspace holds.
    pub fn content(&self) -> &T {
        &self.content
    }
}

/// The workspaces of a window from top to bottom, never empty, with one of
/// them selected.
#[derive(Debug)]
pub struct WorkspaceList<T> {
    workspaces: Vec<Workspace<T>>,
    selected: WorkspaceId,
    /// How many workspaces the list ever had, for the numbered names.
    made: u64,
}

impl<T> WorkspaceList<T> {
    /// A list with a single, selected workspace holding `first`. See
    /// [`add`](Self::add) for `name`.
    pub fn new(name: Option<&str>, first: T) -> Self {
        let mut list = Self {
            workspaces: Vec::new(),
            // Replaced just below, before anyone can read it.
            selected: WorkspaceId(0),
            made: 0,
        };
        list.selected = list.add(name, first);
        list
    }

    /// A list of workspaces made earlier, from top to bottom, or `None`
    /// without workspaces. Each has the name and the color it had, and
    /// what it holds. A workspace without a name that can be shown is
    /// numbered by its place, and one without a color takes the least
    /// used one.
    ///
    /// `selected` counts the workspaces from 0; past the last it means
    /// the last. `made` is how many workspaces the list ever had, which
    /// numbers the ones added later; it is at least their number now.
    pub fn from_saved(
        saved: Vec<(Option<&str>, Option<WorkspaceColor>, T)>,
        selected: usize,
        made: u64,
    ) -> Option<Self> {
        let mut list = Self {
            workspaces: Vec::new(),
            // Replaced below, before anyone can read it.
            selected: WorkspaceId(0),
            made: 0,
        };
        for (name, color, content) in saved {
            let id = list.add(name, content);
            let Some(position) = list.position(id) else {
                continue;
            };
            // What is saved is the names alone: one of the form of the
            // numbered name is kept, and taken for one nobody gave.
            if name.is_some_and(is_numbered_name) {
                list.workspaces[position].named = false;
            }
            if let Some(color) = color {
                list.workspaces[position].color = color;
            }
        }
        let selected = list.workspaces.get(selected).or(list.workspaces.last())?;
        list.selected = selected.id;
        list.made = made.max(list.made);
        Some(list)
    }

    /// Where the selected workspace is, counted from 0 at the top.
    pub fn selected_index(&self) -> usize {
        self.selected_position()
    }

    /// How many workspaces the list ever had, which numbers the next one.
    pub fn made(&self) -> u64 {
        self.made
    }

    /// How many workspaces there are; at least one.
    pub fn count(&self) -> usize {
        self.workspaces.len()
    }

    /// The workspaces from top to bottom.
    pub fn ids(&self) -> Vec<WorkspaceId> {
        self.workspaces.iter().map(Workspace::id).collect()
    }

    /// The workspaces from top to bottom.
    pub fn iter(&self) -> impl Iterator<Item = &Workspace<T>> {
        self.workspaces.iter()
    }

    pub fn selected(&self) -> WorkspaceId {
        self.selected
    }

    pub fn selected_workspace(&self) -> &Workspace<T> {
        &self.workspaces[self.selected_position()]
    }

    /// Where a workspace is, counted from 0 at the top.
    pub fn position(&self, id: WorkspaceId) -> Option<usize> {
        self.workspaces.iter().position(|w| w.id == id)
    }

    /// A workspace and what it holds.
    pub fn get(&self, id: WorkspaceId) -> Option<&Workspace<T>> {
        self.position(id).map(|position| &self.workspaces[position])
    }

    fn selected_position(&self) -> usize {
        self.position(self.selected)
            .expect("the selected workspace is in the list")
    }

    fn find(&self, id: WorkspaceId) -> Result<usize, WorkspaceError> {
        self.position(id)
            .ok_or(WorkspaceError::UnknownWorkspace(id))
    }

    /// The color the fewest workspaces have, and of those the first. While
    /// there are no more workspaces than colors, this keeps them all
    /// different, and a color freed by a closed workspace is taken again
    /// before any is doubled.
    fn least_used_color(&self) -> WorkspaceColor {
        let uses =
            |color: WorkspaceColor| self.workspaces.iter().filter(|w| w.color == color).count();
        WorkspaceColor::all()
            .into_iter()
            .min_by_key(|color| (uses(*color), *color))
            .expect("there is at least one color")
    }

    /// Adds a workspace holding `content` at the bottom. The selection
    /// stays where it is, so that a workspace made from outside the window
    /// does not take the keyboard away.
    ///
    /// Without a `name`, or with one that cannot be shown, the workspace is
    /// called "Workspace N", where N counts the workspaces the list ever
    /// had.
    pub fn add(&mut self, name: Option<&str>, content: T) -> WorkspaceId {
        self.made = self.made.saturating_add(1);
        let id = WorkspaceId::next();
        let given = name.and_then(valid_name);
        let named = given.is_some();
        let name = given.map_or_else(|| format!("Workspace {}", self.made), str::to_owned);
        let color = self.least_used_color();
        self.workspaces.push(Workspace {
            id,
            name,
            named,
            color,
            content,
        });
        id
    }

    pub fn select(&mut self, id: WorkspaceId) -> Result<(), WorkspaceError> {
        self.find(id)?;
        self.selected = id;
        Ok(())
    }

    /// Selects the workspace below the selected one, or the first after the
    /// last.
    pub fn select_next(&mut self) {
        let position = (self.selected_position() + 1) % self.workspaces.len();
        self.selected = self.workspaces[position].id;
    }

    /// Selects the workspace above the selected one, or the last before the
    /// first.
    pub fn select_previous(&mut self) {
        let count = self.workspaces.len();
        let position = (self.selected_position() + count - 1) % count;
        self.selected = self.workspaces[position].id;
    }

    /// Gives a workspace another name, without the spaces around it.
    pub fn rename(&mut self, id: WorkspaceId, name: &str) -> Result<(), WorkspaceError> {
        let position = self.find(id)?;
        let name = valid_name(name).ok_or(WorkspaceError::InvalidName)?;
        self.workspaces[position].name = name.to_owned();
        self.workspaces[position].named = true;
        Ok(())
    }

    /// Closes a workspace and returns what it held. Closing the selected
    /// workspace selects the one that takes its place, which is the one
    /// below it, or the one above if it was the last.
    pub fn close(&mut self, id: WorkspaceId) -> Result<T, WorkspaceError> {
        let position = self.find(id)?;
        if self.workspaces.len() == 1 {
            return Err(WorkspaceError::LastWorkspace);
        }
        let workspace = self.workspaces.remove(position);
        if self.selected == id {
            let position = position.min(self.workspaces.len() - 1);
            self.selected = self.workspaces[position].id;
        }
        Ok(workspace.content)
    }
}
