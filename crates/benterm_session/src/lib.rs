//! The session file restores workspaces, tabs, and panes. Unknown fields are
//! discarded on write. Bump the version when losing a new field would be
//! irreversible, as with conversation, scrollback, and task associations.
//! Older builds leave unsupported versions intact.
//!
//! Tasks live separately in `benterm_task` and outlive sessions. Pane leaves
//! reference files in the adjacent `scrollback` directory. Each file is
//! replaced atomically. Save order can vary because a stale key still names
//! the same pane's content. A missing file restores an empty scrollback.
//! The adjacent agent-state history is managed by [`history`].
//!
//! [`SessionStore`] atomically replaces the session file.

mod format;
pub mod history;
mod store;

pub use crate::format::{
    Agent, Axis, Browser, Conversation, MAX_FILE_BYTES, MAX_PANES, Node, Refused, RestrictedPane,
    Session, Split, Tab, Terminal, Unusable, VERSION, Workspace, is_page_url, new_scrollback_key,
};
pub use crate::store::{
    MAX_SCROLLBACK_BYTES, Opened, SessionStore, Ticket, open, session_path, session_path_from,
};
