//! Whether an agent at work has stalled: its state says it works, and
//! neither its screen nor the files of its directory have changed for
//! longer than a threshold.
//!
//! The state alone does not tell: when a hook does not arrive, a turn
//! that has ended still shows as work. So the mark goes by what the work
//! leaves behind. It changes no state and no wait; it is only shown.
//!
//! Nothing here reads a clock, the screen or a file. The caller hands in
//! what it saw and when, so a test moves the time by itself.

mod files;

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use benterm_agent::AgentState;
use benterm_layout::PaneId;
use gpui::{App, AppContext, Context, Entity, Task};

use crate::selection::fingerprint;
use crate::{BentermWindow, TerminalView};

/// The longest time between two looks at the panes. A look reads a
/// counter and the screen of each agent at work, which takes a few
/// microseconds of the UI thread, and draws nothing unless a mark comes
/// or goes.
const MOST_INTERVAL: Duration = Duration::from_secs(5);

/// The time between two looks at the panes, for `threshold`: a quarter
/// of it, so the mark comes soon after the threshold has passed, and no
/// more than [`MOST_INTERVAL`]. The files of a stalled agent are looked
/// at again as often, so the mark goes within it once one changes.
fn interval(threshold: Duration) -> Duration {
    (threshold / 4).min(MOST_INTERVAL)
}

/// Whether an agent is marked as stalled, and the last change seen of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StallMark {
    pub stalled: bool,
    /// The last change seen, in milliseconds since 1970: of the screen,
    /// of a file, or the start of the work.
    pub last_change_ms: u64,
}

/// What a window keeps to mark its stalled agents.
#[derive(Default)]
pub(crate) struct WindowStalls {
    /// The terminals whose agent was seen at work, by their id.
    panes: HashMap<PaneId, PaneStall>,
    _tick: Option<Task<()>>,
}

struct PaneStall {
    stall: Stall,
    /// Whether the mark was shown at the last look.
    shown: bool,
    /// A look at the files runs.
    looking: bool,
    /// Where the next look at the files starts.
    from: usize,
    /// The newest time of a file seen so far.
    newest_file: Option<SystemTime>,
    /// The last change of `stall` as it was when its time on the wall
    /// clock was taken, and that time. Taken once for each change, so it
    /// is told the same every time.
    wall: Option<(Instant, SystemTime)>,
}

impl PaneStall {
    /// Takes the time on the wall clock of the last change, if it is new
    /// since the last time, as of `now`.
    fn note_wall(&mut self, now: Instant) {
        let last = self.stall.last_change();
        if self.wall.is_some_and(|(noted, _)| noted == last) {
            return;
        }
        let ago = now.saturating_duration_since(last);
        let at = SystemTime::now()
            .checked_sub(ago)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        self.wall = Some((last, at));
    }
}

impl BentermWindow {
    /// Looks at the window's terminals every little while for agents
    /// that have stalled, for as long as the window is open.
    pub(crate) fn watch_stalls(cx: &mut Context<Self>) -> WindowStalls {
        let tick = cx.spawn(async move |this, cx| {
            loop {
                let Ok(every) =
                    this.read_with(cx, |this, _| interval(this.settings.stall_threshold()))
                else {
                    break;
                };
                cx.background_executor().timer(every).await;
                if this
                    .update(cx, |this, cx| this.look_for_stalls(cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        WindowStalls {
            panes: HashMap::new(),
            _tick: Some(tick),
        }
    }

    /// The mark of the agent in the terminal `pane`, while it is at work.
    pub fn stall_mark(&self, pane: PaneId) -> Option<StallMark> {
        let watched = self.stalls.panes.get(&pane)?;
        if !watched.stall.working {
            return None;
        }
        let threshold = self.settings.stall_threshold();
        let (_, at) = watched.wall?;
        Some(StallMark {
            stalled: watched.stall.is_stalled(threshold),
            last_change_ms: at
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |since| since.as_millis() as u64),
        })
    }

    /// Whether the agent in the terminal `pane` is marked as stalled.
    pub(crate) fn is_stalled(&self, pane: PaneId) -> bool {
        let threshold = self.settings.stall_threshold();
        self.stalls
            .panes
            .get(&pane)
            .is_some_and(|watched| watched.stall.is_stalled(threshold))
    }

    /// Takes what each terminal shows now, starts the looks at the files
    /// that are due, and shows the marks that came or went.
    pub(crate) fn look_for_stalls(&mut self, cx: &mut Context<Self>) {
        let now = cx.background_executor().now();
        let threshold = self.settings.stall_threshold();
        let every = interval(threshold);
        let terminals = self.all_terminals(cx);
        self.stalls
            .panes
            .retain(|pane, _| terminals.iter().any(|(id, _)| id == pane));
        let mut looks = Vec::new();
        for (pane, view) in terminals {
            let view = view.read(cx);
            let state = view.agent_status().state;
            if state != AgentState::Working && !self.stalls.panes.contains_key(&pane) {
                continue;
            }
            let watched = self.stalls.panes.entry(pane).or_insert_with(|| PaneStall {
                stall: Stall::new(now),
                shown: false,
                looking: false,
                from: 0,
                newest_file: None,
                wall: None,
            });
            watched.stall.observe_state(state, now);
            if state != AgentState::Working {
                continue;
            }
            watched.stall.observe_screen(screen_print(view), now);
            watched.note_wall(now);
            if !watched.looking && watched.stall.wants_files(now, threshold, every) {
                let directory = view
                    .foreground_working_directory()
                    .or_else(|| view.working_directory());
                if let Some(directory) = directory {
                    watched.looking = true;
                    looks.push((pane, directory, watched.from));
                }
            }
        }
        for (pane, directory, from) in looks {
            self.look_at_files(pane, directory, from, now, cx);
        }
        self.show_stalls(cx);
    }

    /// Looks at the files of `directory` on a background thread, for the
    /// agent in the terminal `pane`, as of `now`.
    fn look_at_files(
        &mut self,
        pane: PaneId,
        directory: PathBuf,
        from: usize,
        now: Instant,
        cx: &mut Context<Self>,
    ) {
        let git_path = self.git(self.shells.as_ref()).path;
        let wall = SystemTime::now();
        let look =
            cx.background_spawn(async move { files::look(&directory, git_path.as_deref(), from) });
        cx.spawn(async move |this, cx| {
            let found = look.await;
            let _ = this.update(cx, |this, cx| {
                let Some(watched) = this.stalls.panes.get_mut(&pane) else {
                    return;
                };
                watched.looking = false;
                watched.from = found.next;
                // Only a time newer than any seen before is a change: the
                // clock of the files is not the one of the looks.
                let newer = found
                    .newest
                    .filter(|newest| watched.newest_file.is_none_or(|seen| *newest > seen));
                if let Some(newest) = newer {
                    watched.newest_file = Some(newest);
                }
                let changed = newer.map(|newest| {
                    let ago = wall.duration_since(newest).unwrap_or_default();
                    now.checked_sub(ago).unwrap_or(now)
                });
                watched.stall.observe_files(changed, now);
                watched.note_wall(now);
                this.show_stalls(cx);
            });
        })
        .detach();
    }

    /// Draws the window again, and hands the board the marks, if a mark
    /// came or went.
    fn show_stalls(&mut self, cx: &mut Context<Self>) {
        let threshold = self.settings.stall_threshold();
        let mut flipped = false;
        for watched in self.stalls.panes.values_mut() {
            let stalled = watched.stall.is_stalled(threshold);
            flipped |= stalled != watched.shown;
            watched.shown = stalled;
        }
        if flipped {
            self.sync_board(cx);
            cx.notify();
        }
    }

    /// Every terminal of the window: those in the panes, and those
    /// behind the window.
    fn all_terminals(&self, cx: &App) -> Vec<(PaneId, Entity<TerminalView>)> {
        let mut terminals = Vec::new();
        for workspace in self.workspaces.iter() {
            let tabs = workspace.content().tabs.read(cx);
            for tab in tabs.tabs() {
                let Some(grid) = tabs.grid(tab) else {
                    continue;
                };
                let grid = grid.read(cx);
                terminals.extend(
                    grid.panes()
                        .into_iter()
                        .filter_map(|pane| Some((pane, grid.terminal(pane)?.clone()))),
                );
            }
        }
        terminals.extend(self.agents.terminals_behind());
        terminals
    }
}

/// The fingerprint of what a terminal shows: its screen, and how many
/// rows went above it.
fn screen_print(view: &TerminalView) -> u64 {
    let screen = view.screen();
    let mut hasher = DefaultHasher::new();
    fingerprint(&screen.rows).hash(&mut hasher);
    screen.scrollback_len.hash(&mut hasher);
    hasher.finish()
}

/// What is known of one agent's changes.
#[derive(Clone, Debug)]
pub(crate) struct Stall {
    working: bool,
    /// The last change seen: of the screen, of a file, or the start of
    /// the work.
    last_change: Instant,
    /// The fingerprint of the screen as last seen.
    screen: Option<u64>,
    /// When the files were last looked at.
    checked: Option<Instant>,
}

impl Stall {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            working: false,
            last_change: now,
            screen: None,
            checked: None,
        }
    }

    /// Takes the agent's state. Work that begins counts as a change.
    pub(crate) fn observe_state(&mut self, state: AgentState, now: Instant) {
        let working = state == AgentState::Working;
        if working && !self.working {
            self.changed(now);
        }
        self.working = working;
    }

    pub(crate) fn observe_screen(&mut self, fingerprint: u64, now: Instant) {
        if self.screen.is_some_and(|screen| screen != fingerprint) {
            self.changed(now);
        }
        self.screen = Some(fingerprint);
    }

    /// Takes the newest change a look at the files found, `None` when it
    /// found none, and when the look was.
    pub(crate) fn observe_files(&mut self, newest: Option<Instant>, at: Instant) {
        if let Some(newest) = newest {
            self.changed(newest.min(at));
        }
        self.checked = Some(self.checked.map_or(at, |checked| checked.max(at)));
    }

    /// Whether the agent is marked: at work, and a look at the files
    /// more than `threshold` after the last change found none since.
    pub(crate) fn is_stalled(&self, threshold: Duration) -> bool {
        self.working
            && self
                .checked
                .and_then(|checked| checked.checked_duration_since(self.last_change))
                .is_some_and(|quiet| quiet > threshold)
    }

    /// Whether the files should be looked at now: the agent is at work
    /// and has shown no change for longer than `threshold`, and the
    /// files were not looked at since, or not for `interval`.
    pub(crate) fn wants_files(
        &self,
        now: Instant,
        threshold: Duration,
        interval: Duration,
    ) -> bool {
        let quiet = now.saturating_duration_since(self.last_change);
        if !self.working || quiet <= threshold {
            return false;
        }
        match self.checked {
            None => true,
            // The last look was too early to tell.
            Some(checked) if !self.is_stalled(threshold) => {
                checked.saturating_duration_since(self.last_change) <= threshold
                    || now.saturating_duration_since(checked) >= interval
            }
            Some(checked) => now.saturating_duration_since(checked) >= interval,
        }
    }

    /// Notes a change at `at`, unless a later one is known.
    fn changed(&mut self, at: Instant) {
        self.last_change = self.last_change.max(at);
    }

    pub(crate) fn last_change(&self) -> Instant {
        self.last_change
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THRESHOLD: Duration = Duration::from_secs(600);
    const INTERVAL: Duration = Duration::from_secs(5);
    const SECOND: Duration = Duration::from_secs(1);

    /// An agent that began to work at `start`, with a screen seen then.
    fn working(start: Instant) -> Stall {
        let mut stall = Stall::new(start);
        stall.observe_state(AgentState::Working, start);
        stall.observe_screen(1, start);
        stall
    }

    #[test]
    fn work_unchanged_a_second_longer_than_the_threshold_is_marked() {
        let start = Instant::now();
        let mut stall = working(start);
        let now = start + THRESHOLD + SECOND;
        stall.observe_screen(1, now);
        assert!(stall.wants_files(now, THRESHOLD, INTERVAL));
        stall.observe_files(None, now);
        assert!(stall.is_stalled(THRESHOLD));
        assert_eq!(stall.last_change(), start);
    }

    #[test]
    fn work_unchanged_a_second_shorter_than_the_threshold_is_not_marked() {
        let start = Instant::now();
        let mut stall = working(start);
        let now = start + THRESHOLD - SECOND;
        stall.observe_screen(1, now);
        assert!(!stall.wants_files(now, THRESHOLD, INTERVAL));
        stall.observe_files(None, now);
        assert!(!stall.is_stalled(THRESHOLD));
    }

    #[test]
    fn a_change_of_the_screen_takes_the_mark_away() {
        let start = Instant::now();
        let mut stall = working(start);
        let then = start + THRESHOLD + SECOND;
        stall.observe_files(None, then);
        assert!(stall.is_stalled(THRESHOLD));

        stall.observe_screen(2, then + SECOND);
        assert!(!stall.is_stalled(THRESHOLD));
        assert_eq!(stall.last_change(), then + SECOND);
    }

    #[test]
    fn the_end_of_the_work_takes_the_mark_away_with_the_screen_unchanged() {
        let start = Instant::now();
        let mut stall = working(start);
        let then = start + THRESHOLD + SECOND;
        stall.observe_files(None, then);
        assert!(stall.is_stalled(THRESHOLD));

        stall.observe_state(AgentState::Done, then + SECOND);
        stall.observe_screen(1, then + SECOND);
        assert!(!stall.is_stalled(THRESHOLD));
        let later = then + THRESHOLD * 2;
        assert!(!stall.wants_files(later, THRESHOLD, INTERVAL));
    }

    #[test]
    fn a_file_changed_within_the_threshold_keeps_the_mark_off() {
        let start = Instant::now();
        let mut stall = working(start);
        let now = start + THRESHOLD + SECOND;
        stall.observe_screen(1, now);
        stall.observe_files(Some(now - THRESHOLD + SECOND), now);
        assert!(!stall.is_stalled(THRESHOLD));
        assert_eq!(stall.last_change(), now - THRESHOLD + SECOND);
        // Looked at again once the threshold has passed since the file
        // changed, and not before.
        assert!(!stall.wants_files(now, THRESHOLD, INTERVAL));
        assert!(stall.wants_files(now + 2 * SECOND, THRESHOLD, INTERVAL));
    }

    #[test]
    fn work_that_begins_again_counts_as_a_change() {
        let start = Instant::now();
        let mut stall = working(start);
        let then = start + THRESHOLD + SECOND;
        stall.observe_state(AgentState::Done, then);
        stall.observe_state(AgentState::Working, then + SECOND);
        stall.observe_files(None, then + SECOND);
        assert!(!stall.is_stalled(THRESHOLD));
        assert_eq!(stall.last_change(), then + SECOND);
    }

    #[test]
    fn a_marked_agent_has_its_files_looked_at_again_after_the_interval() {
        let start = Instant::now();
        let mut stall = working(start);
        let then = start + THRESHOLD + SECOND;
        stall.observe_files(None, then);
        assert!(!stall.wants_files(then + INTERVAL - SECOND, THRESHOLD, INTERVAL));
        assert!(stall.wants_files(then + INTERVAL, THRESHOLD, INTERVAL));

        // A file written meanwhile takes the mark away.
        stall.observe_files(Some(then + SECOND), then + INTERVAL);
        assert!(!stall.is_stalled(THRESHOLD));
    }

    #[test]
    fn a_change_older_than_the_last_one_seen_moves_nothing_back() {
        let start = Instant::now();
        let mut stall = working(start);
        stall.observe_files(Some(start - THRESHOLD), start + SECOND);
        assert_eq!(stall.last_change(), start);
    }
}
