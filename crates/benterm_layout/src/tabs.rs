//! The tabs of a window: their order, which of them is selected, and the
//! names the user gave them.
//!
//! The list knows nothing about what a tab shows. The app keeps a pane tree
//! and its terminals in each tab; tests keep plain numbers.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// Identifies a tab. Ids come from one counter for the whole process, so no
/// two tabs share an id and an id is never reused after its tab closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TabId(u64);

impl TabId {
    /// An id no tab in the process has had yet.
    fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        TabId(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    /// The number behind the id.
    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for TabId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Why an operation on the list was refused. A refused operation leaves the
/// list unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabError {
    UnknownTab(TabId),
    /// Tabs are numbered from 1.
    NoTabZero,
    /// The list always has a tab, so its last one cannot be closed. The
    /// window closes instead.
    LastTab,
    /// A tab's name is one line.
    InvalidName,
}

impl fmt::Display for TabError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownTab(id) => write!(f, "no tab has the id {id}"),
            Self::NoTabZero => f.write_str("tabs are numbered from 1"),
            Self::LastTab => f.write_str("the last tab cannot be closed"),
            Self::InvalidName => f.write_str("a tab's name is one line"),
        }
    }
}

impl std::error::Error for TabError {}

/// The tabs of a window in the order they are shown, never empty, with one
/// of them selected.
#[derive(Clone, Debug)]
pub struct TabList<T> {
    tabs: Vec<(TabId, T)>,
    selected: TabId,
    /// The names the user gave, which tabs without one do not have.
    names: HashMap<TabId, String>,
}

impl<T> TabList<T> {
    /// A list with a single, selected tab holding `first`.
    pub fn new(first: T) -> Self {
        let id = TabId::next();
        Self {
            tabs: vec![(id, first)],
            selected: id,
            names: HashMap::new(),
        }
    }

    /// A list holding `tabs` from left to right, or `None` without tabs.
    /// `selected` counts them from 0; past the last tab it means the last.
    pub fn from_tabs(tabs: Vec<T>, selected: usize) -> Option<Self> {
        let tabs: Vec<(TabId, T)> = tabs.into_iter().map(|tab| (TabId::next(), tab)).collect();
        let selected = tabs.get(selected).or(tabs.last())?.0;
        Some(Self {
            tabs,
            selected,
            names: HashMap::new(),
        })
    }

    /// Where the selected tab is, counted from 0 on the left.
    pub fn selected_index(&self) -> usize {
        self.selected_position()
    }

    /// How many tabs there are; at least one.
    pub fn count(&self) -> usize {
        self.tabs.len()
    }

    /// The tabs from left to right.
    pub fn ids(&self) -> Vec<TabId> {
        self.tabs.iter().map(|(id, _)| *id).collect()
    }

    /// The tabs and what they hold, from left to right.
    pub fn iter(&self) -> impl Iterator<Item = (TabId, &T)> {
        self.tabs.iter().map(|(id, tab)| (*id, tab))
    }

    pub fn selected(&self) -> TabId {
        self.selected
    }

    pub fn selected_tab(&self) -> &T {
        &self.tabs[self.selected_position()].1
    }

    /// Where a tab is, counted from 0 on the left.
    pub fn position(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|(tab, _)| *tab == id)
    }

    /// What a tab holds.
    pub fn get(&self, id: TabId) -> Option<&T> {
        self.position(id).map(|position| &self.tabs[position].1)
    }

    /// The name the user gave a tab, if they gave it one.
    pub fn name(&self, id: TabId) -> Option<&str> {
        self.names.get(&id).map(String::as_str)
    }

    /// Gives a tab the name `name`, without the spaces around it. A name
    /// that is only spaces takes the tab's name away.
    pub fn rename(&mut self, id: TabId, name: &str) -> Result<(), TabError> {
        self.find(id)?;
        let name = name.trim();
        if name.chars().any(crate::ends_a_line) {
            return Err(TabError::InvalidName);
        }
        if name.is_empty() {
            self.names.remove(&id);
        } else {
            self.names.insert(id, name.to_owned());
        }
        Ok(())
    }

    fn selected_position(&self) -> usize {
        self.position(self.selected)
            .expect("the selected tab is in the list")
    }

    fn find(&self, id: TabId) -> Result<usize, TabError> {
        self.position(id).ok_or(TabError::UnknownTab(id))
    }

    fn select_position(&mut self, position: usize) {
        self.selected = self.tabs[position].0;
    }

    /// Adds a tab holding `tab` just right of the selected one and selects
    /// it, as Ghostty does by default.
    pub fn open(&mut self, tab: T) -> TabId {
        let id = TabId::next();
        let position = self.selected_position() + 1;
        self.tabs.insert(position, (id, tab));
        self.selected = id;
        id
    }

    pub fn select(&mut self, id: TabId) -> Result<(), TabError> {
        self.find(id)?;
        self.selected = id;
        Ok(())
    }

    /// Selects the tab numbered `number` from 1 on the left, or the last
    /// tab if there are fewer, like Ghostty's `goto_tab`.
    pub fn goto(&mut self, number: usize) -> Result<(), TabError> {
        if number == 0 {
            return Err(TabError::NoTabZero);
        }
        self.select_position(number.min(self.tabs.len()) - 1);
        Ok(())
    }

    /// Selects the rightmost tab.
    pub fn select_last(&mut self) {
        self.select_position(self.tabs.len() - 1);
    }

    /// Selects the tab right of the selected one, or the first after the
    /// last.
    pub fn select_next(&mut self) {
        let position = (self.selected_position() + 1) % self.tabs.len();
        self.select_position(position);
    }

    /// Selects the tab left of the selected one, or the last before the
    /// first.
    pub fn select_previous(&mut self) {
        let count = self.tabs.len();
        let position = (self.selected_position() + count - 1) % count;
        self.select_position(position);
    }

    /// Closes a tab and returns what it held. Closing the selected tab
    /// selects the tab that takes its place, which is the one on its right,
    /// or the one on its left if it was the rightmost.
    pub fn close(&mut self, id: TabId) -> Result<T, TabError> {
        let position = self.find(id)?;
        if self.tabs.len() == 1 {
            return Err(TabError::LastTab);
        }
        let (_, tab) = self.tabs.remove(position);
        self.names.remove(&id);
        if self.selected == id {
            self.select_position(position.min(self.tabs.len() - 1));
        }
        Ok(tab)
    }

    /// Moves a tab so that it ends up at `position`, counted from 0 on the
    /// left; a position past the end means the rightmost. The selection
    /// stays on the same tab.
    pub fn move_to(&mut self, id: TabId, position: usize) -> Result<(), TabError> {
        let from = self.find(id)?;
        let tab = self.tabs.remove(from);
        let to = position.min(self.tabs.len());
        self.tabs.insert(to, tab);
        Ok(())
    }
}
