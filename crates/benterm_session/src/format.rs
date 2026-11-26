//! What the session file holds, and how it is written and read.

use std::collections::HashSet;
use std::fmt;
use std::io::{self, Read};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The version this build writes, and the newest one it reads.
///
/// Version 2 added the key of a pane's scrollback to its leaf, and
/// version 3 the conversation of the pane's agent. A file of an earlier
/// version is read as it is: its panes have neither.
///
/// Version 4 added the task a workspace shows. A session without a task
/// is still written as version 3, so that a build from before tasks goes
/// on reading it.
///
/// Version 5 records terminals that must restore without control. Older
/// builds must leave these sessions alone to preserve that restriction.
pub const VERSION: u32 = 5;

const VERSION_WITH_TASKS: u32 = 4;

/// The version of a session in which no workspace shows a task.
const VERSION_WITHOUT_TASKS: u32 = 3;

/// The most panes a session file holds. A file with more is refused
/// before any shell is started for it. macOS has 511 terminals for all
/// apps together (`kern.tty.ptmx_max`), and this leaves half of them.
pub const MAX_PANES: usize = 256;

/// How deep splits are nested at most. The reader stops at a nesting of
/// 128 JSON values, so the writer stays well below it.
const MAX_DEPTH: usize = 100;

/// A session file larger than this is neither written nor read.
pub const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;

/// What keeps a session from being written or read.
enum Fault {
    /// It is not what a session is, in any build.
    Shape(&'static str),
    /// It is past a limit of this build, which a later build may not have.
    Limit(String),
}

/// The workspaces of the window, from top to bottom.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    /// The place of the selected workspace, counted from 0.
    #[serde(default)]
    pub selected_workspace: usize,
    /// How many workspaces the window ever had, which numbers the next
    /// one made without a name.
    #[serde(default)]
    pub workspaces_made: u64,
    pub workspaces: Vec<Workspace>,
    /// The repositories whose tasks are folded in the sidebar, as the
    /// tasks file has their paths.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folded_repositories: Vec<String>,
}

/// A workspace and its tabs, from left to right.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub name: String,
    /// The place of the workspace's color in the theme.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<usize>,
    /// The place of the selected tab, counted from 0.
    #[serde(default)]
    pub selected_tab: usize,
    pub tabs: Vec<Tab>,
    /// The number of the task the workspace shows, as the tasks file
    /// beside the session file has it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<u64>,
}

/// A tab and its panes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tab {
    /// The name the user gave the tab, one line without spaces around
    /// it. Without one the tab shows the title of its program.
    ///
    /// A build from before tab names passes it over, so it does not raise
    /// the version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The place of the focused pane, counted from 0 with the first side
    /// of every split before its second.
    #[serde(default)]
    pub focused_pane: usize,
    pub root: Node,
}

/// A pane, or two sides with a border between them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Node {
    Terminal(Terminal),
    Browser(Browser),
    Split(Split),
}

/// The kinds of [`Node`] this build knows. A file with another kind is
/// from a newer build.
///
/// A build from before a kind was added reads a file with a pane of that
/// kind as one of a newer build and leaves it as it is, whatever version
/// the file has. So a new kind of pane does not raise the version.
const KINDS: [&str; 3] = ["terminal", "browser", "split"];

/// A pane that shows a web page.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Browser {
    /// The address of the page, which [`is_page_url`] holds for. Without
    /// one the pane shows no page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    url: Option<String>,
}

impl Browser {
    /// A pane that shows the page at `url`, or no page if `url` is not
    /// the address of a web page. The user and the password of an
    /// address are not kept: the session is written to a file.
    pub fn at(url: &str) -> Self {
        Self {
            url: is_page_url(url).then(|| without_userinfo(url)),
        }
    }

    /// The address of the page the pane shows.
    pub fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }
}

/// `url`, the address of a web page, without the user and the password
/// before its host, as in `https://user:password@host/`. An address
/// that names neither is returned as it is written.
fn without_userinfo(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut parsed) if !parsed.username().is_empty() || parsed.password().is_some() => {
            // Neither fails for an address with a host, which a page has.
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
            parsed.into()
        }
        _ => url.to_owned(),
    }
}

/// The longest address a browser pane keeps.
const MAX_URL: usize = 8192;

/// Whether `url` is an address a browser pane opens: one of a web page,
/// with `http://` or `https://`, a host, and a port that is one if there
/// is any, written without spaces or control characters. Files and
/// scripts are not opened.
pub fn is_page_url(url: &str) -> bool {
    // Written out in full: a parser of addresses is lenient, and reads
    // `http:example.com` and `http:///example.com` as addresses too.
    let rest = ["http://", "https://"].into_iter().find_map(|scheme| {
        let start = url.get(..scheme.len())?;
        start
            .eq_ignore_ascii_case(scheme)
            .then(|| &url[scheme.len()..])
    });
    let written = |rest: &str| !rest.starts_with(['/', '?', '#', '\\']) && !rest.is_empty();
    url.len() <= MAX_URL
        && rest.is_some_and(written)
        && !url.chars().any(|c| c.is_control() || c.is_whitespace())
        && url::Url::parse(url).is_ok_and(|url| url.host_str().is_some_and(|host| !host.is_empty()))
}

/// A pane with a shell.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Terminal {
    /// The directory of the shell, as an absolute path. Without one the
    /// shell starts where a new shell does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The key of the file that holds what the pane showed; see
    /// [`new_scrollback_key`]. The pane keeps its key for as long as it
    /// lives, also across restarts, wherever it is among the panes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scrollback: Option<String>,
    /// The conversation of the agent in front in the pane, to take up
    /// again when the pane is made again. A conversation that this build
    /// cannot take up is read as none.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "conversation_or_none"
    )]
    pub conversation: Option<Conversation>,
    /// A pane that must be restored with credentials for its own pane only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restricted: Option<RestrictedPane>,
}

/// The launch information of a pane restored without control.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestrictedPane {
    /// The role name, resolved against the current settings on restore.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// A coding agent whose conversations can be taken up again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    /// The id of the agent's definition in `benterm_agent`.
    pub fn id(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    /// The agent whose definition in `benterm_agent` has the id `id`.
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }
}

/// A conversation of an agent, named by the id the agent's hooks report
/// as the session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conversation {
    pub agent: Agent,
    /// Letters, digits, `-` and `_`, starting with a letter or a digit.
    /// Typed into a shell, such an id is one word and never an option.
    id: String,
}

/// The longest id a conversation has. Both agents use UUIDs, of 36.
const MAX_ID: usize = 128;

impl Conversation {
    /// The conversation `id` of `agent`, if `id` is one that can be
    /// typed into a shell as it is.
    pub fn new(agent: Agent, id: &str) -> Option<Self> {
        is_conversation_id(id).then(|| Self {
            agent,
            id: id.to_owned(),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// The command line that takes the conversation up again, to be
    /// typed into a shell in the pane's directory.
    pub fn resume_command(&self) -> String {
        let [resume, id] = self.resume_args();
        format!("{} {resume} {id}", self.agent.id())
    }

    /// The arguments that select this conversation.
    pub fn resume_args(&self) -> [&str; 2] {
        let resume = match self.agent {
            Agent::Claude => "--resume",
            Agent::Codex => "resume",
        };
        [resume, &self.id]
    }
}

fn is_conversation_id(id: &str) -> bool {
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'-' || b == b'_';
    id.len() <= MAX_ID
        && id.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        && id.bytes().all(word)
}

/// Reads a conversation, and a conversation this build cannot take up,
/// as of an agent it does not know, as none.
fn conversation_or_none<'de, D>(reader: D) -> Result<Option<Conversation>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(reader)?;
    let read = serde_json::from_value::<Conversation>(value).ok();
    Ok(read.and_then(|read| Conversation::new(read.agent, &read.id)))
}

/// A new key for the scrollback of a pane: 32 hexadecimal digits of 128
/// random bits, so that no two panes ever get the same one.
pub fn new_scrollback_key() -> io::Result<String> {
    let mut random = [0; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
    Ok(format!("{:032x}", u128::from_be_bytes(random)))
}

/// Whether `key` is such as [`new_scrollback_key`] makes. Nothing else
/// is used as the name of a file.
pub(crate) fn is_scrollback_key(key: &str) -> bool {
    key.len() == 32 && key.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl Terminal {
    /// A terminal whose shell is in `directory`. A path that is not
    /// absolute, or not UTF-8 and so not text the file can hold, is not
    /// kept: written in another way it would name another directory.
    pub fn in_directory(directory: Option<&Path>) -> Self {
        Self {
            cwd: directory
                .and_then(Path::to_str)
                .filter(|path| is_directory(path))
                .map(str::to_owned),
            ..Self::default()
        }
    }

    /// The directory to start the shell in.
    pub fn directory(&self) -> Option<&Path> {
        self.cwd.as_deref().map(Path::new)
    }
}

/// Whether the text can be the path of a shell's directory.
fn is_directory(path: &str) -> bool {
    Path::new(path).is_absolute() && !path.contains('\0')
}

/// Two sides and how the space is shared between them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Split {
    pub axis: Axis,
    /// The share of the space `first` gets before the minimum pane sizes
    /// are applied, from 0 to 1.
    pub ratio: f64,
    pub first: Box<Node>,
    pub second: Box<Node>,
}

/// The direction along which a split divides its space.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    /// `first` is on the left and `second` on the right.
    Horizontal,
    /// `first` is above and `second` below.
    Vertical,
}

/// Why a file is not used as a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unusable {
    /// The file is not a session file, in any build that reads this
    /// version. The text says why, starting with "it", and shows nothing
    /// of what the file holds.
    Corrupt(String),
    /// The file is from a newer build, which can still read it, or may
    /// be: it is past a limit of this build. The text says what shows
    /// that, starting with "it", and shows nothing of what the file holds.
    Newer(String),
}

/// Why a session is not written: the reader would refuse the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused(String);

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the session is not written, because {}", self.0)
    }
}

impl std::error::Error for Refused {}

/// A session as the file has it: with the version first.
#[derive(Serialize)]
struct Versioned<'a> {
    version: u32,
    #[serde(flatten)]
    session: &'a Session,
}

impl Session {
    /// The text of the session file. A session that [`from_json`]
    /// would refuse is not written.
    ///
    /// [`from_json`]: Self::from_json
    pub fn to_json(&self) -> Result<String, Refused> {
        self.check().map_err(|fault| match fault {
            Fault::Shape(why) => Refused(why.into()),
            Fault::Limit(why) => Refused(why),
        })?;
        let tasks = self.workspaces.iter().any(|w| w.task.is_some());
        let restricted = self
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.tabs)
            .any(|tab| tab.root.has_restricted_pane());
        let file = Versioned {
            version: if restricted {
                VERSION
            } else if tasks {
                VERSION_WITH_TASKS
            } else {
                VERSION_WITHOUT_TASKS
            },
            session: self,
        };
        let mut text = serde_json::to_string_pretty(&file)
            .map_err(|_| Refused("it cannot be written as JSON".into()))?;
        text.push('\n');
        if text.len() > MAX_FILE_BYTES {
            return Err(Refused(format!("it is larger than {MAX_FILE_BYTES} bytes")));
        }
        Ok(text)
    }

    /// The keys of the scrollback of the panes, in the order of the
    /// panes.
    pub fn scrollback_keys(&self) -> Vec<&str> {
        fn of<'a>(node: &'a Node, keys: &mut Vec<&'a str>) {
            match node {
                Node::Terminal(terminal) => keys.extend(terminal.scrollback.as_deref()),
                Node::Browser(_) => {}
                Node::Split(split) => {
                    of(&split.first, keys);
                    of(&split.second, keys);
                }
            }
        }
        let mut keys = Vec::new();
        let tabs = self.workspaces.iter().flat_map(|workspace| &workspace.tabs);
        tabs.for_each(|tab| of(&tab.root, &mut keys));
        keys
    }

    /// Reads the text of a session file. Places past the end of a list
    /// become the last place, ratios outside 0 to 1 the nearer of the
    /// two, a directory that is no absolute path no directory, and an
    /// address that is not one of a web page no address. A
    /// scrollback key that is not one, or that an earlier pane has
    /// already, is dropped.
    pub fn from_json(text: &[u8]) -> Result<Self, Unusable> {
        // The reason never quotes the file: only where the reader stopped.
        let corrupt = |error: serde_json::Error| {
            // The reader gave up before it could tell what the file is.
            // No file this build writes is that deep, so it is not one
            // to replace.
            if error.to_string().starts_with("recursion limit exceeded") {
                return Unusable::Newer("it is nested deeper than this Benterm reads".into());
            }
            Unusable::Corrupt(format!(
                "it is not a session file (line {}, column {})",
                error.line(),
                error.column()
            ))
        };
        let value: Value = serde_json::from_slice(text).map_err(corrupt)?;
        let version = value
            .get("version")
            .and_then(Value::as_u64)
            .ok_or_else(|| Unusable::Corrupt("it has no version number".into()))?;
        if version == 0 {
            return Err(Unusable::Corrupt(
                "it has version 0, which no Benterm wrote".into(),
            ));
        }
        if version > u64::from(VERSION) {
            return Err(Unusable::Newer(format!(
                "it has version {version} and this Benterm reads up to version {VERSION}"
            )));
        }
        if has_unknown_kind(&value) {
            return Err(Unusable::Newer(
                "it has a pane of a kind this Benterm does not know".into(),
            ));
        }
        let mut session: Session = serde_json::from_value(value).map_err(|_| {
            Unusable::Corrupt("it does not have the fields of a session file".into())
        })?;
        session.check().map_err(|fault| match fault {
            Fault::Shape(why) => Unusable::Corrupt(why.into()),
            Fault::Limit(why) => Unusable::Newer(why),
        })?;
        session.bring_into_range();
        Ok(session)
    }

    /// What makes the session one that is neither written nor read, as
    /// text starting with "it".
    fn check(&self) -> Result<(), Fault> {
        if self.workspaces.is_empty() {
            return Err(Fault::Shape("it has no workspace"));
        }
        let mut panes = 0;
        for workspace in &self.workspaces {
            if workspace.tabs.is_empty() {
                return Err(Fault::Shape("it has a workspace without a tab"));
            }
            for tab in &workspace.tabs {
                panes += tab.root.check(0)?;
                if panes > MAX_PANES {
                    return Err(Fault::Limit(format!("it has more than {MAX_PANES} panes")));
                }
            }
        }
        Ok(())
    }

    fn bring_into_range(&mut self) {
        self.selected_workspace = self.selected_workspace.min(self.workspaces.len() - 1);
        let mut keys = HashSet::new();
        for workspace in &mut self.workspaces {
            workspace.selected_tab = workspace.selected_tab.min(workspace.tabs.len() - 1);
            for tab in &mut workspace.tabs {
                tab.name = tab.name.take().and_then(tab_name);
                let panes = tab.root.bring_into_range(&mut keys);
                tab.focused_pane = tab.focused_pane.min(panes - 1);
            }
        }
    }
}

/// `name` as a tab's name: without the spaces around it, and none if it
/// is empty or has a control character or a line or paragraph separator,
/// as the tab list refuses.
fn tab_name(name: String) -> Option<String> {
    let name = name.trim();
    let shown = |c: char| !c.is_control() && !matches!(c, '\u{2028}' | '\u{2029}');
    (!name.is_empty() && name.chars().all(shown)).then(|| name.to_owned())
}

impl Node {
    fn has_restricted_pane(&self) -> bool {
        match self {
            Self::Terminal(terminal) => terminal.restricted.is_some(),
            Self::Browser(_) => false,
            Self::Split(split) => {
                split.first.has_restricted_pane() || split.second.has_restricted_pane()
            }
        }
    }

    /// How many panes are below the node, which is `depth` splits down.
    fn check(&self, depth: usize) -> Result<usize, Fault> {
        match self {
            Node::Terminal(_) | Node::Browser(_) => Ok(1),
            Node::Split(_) if depth >= MAX_DEPTH => Err(Fault::Limit(format!(
                "it has splits nested deeper than {MAX_DEPTH}"
            ))),
            Node::Split(split) => {
                if !split.ratio.is_finite() {
                    return Err(Fault::Shape("it has a split whose ratio is not a number"));
                }
                Ok(split.first.check(depth + 1)? + split.second.check(depth + 1)?)
            }
        }
    }

    /// Returns how many panes are below the node. `keys` are the
    /// scrollback keys of the panes before it.
    fn bring_into_range(&mut self, keys: &mut HashSet<String>) -> usize {
        match self {
            Node::Terminal(terminal) => {
                terminal.cwd.take_if(|path| !is_directory(path));
                terminal
                    .scrollback
                    .take_if(|key| !is_scrollback_key(key) || !keys.insert(key.clone()));
                1
            }
            Node::Browser(browser) => {
                browser.url.take_if(|url| !is_page_url(url));
                // A file of an older build may hold a user and a password.
                browser.url = browser.url.take().map(|url| without_userinfo(&url));
                1
            }
            Node::Split(split) => {
                split.ratio = split.ratio.clamp(0.0, 1.0);
                split.first.bring_into_range(keys) + split.second.bring_into_range(keys)
            }
        }
    }
}

/// Whether the file has a node of a kind this build does not know.
fn has_unknown_kind(file: &Value) -> bool {
    fn in_node(node: &Value) -> bool {
        match node.get("kind").and_then(Value::as_str) {
            Some("split") => ["first", "second"]
                .into_iter()
                .any(|side| node.get(side).is_some_and(in_node)),
            Some(kind) => !KINDS.contains(&kind),
            None => false,
        }
    }
    let workspaces = file.get("workspaces").and_then(Value::as_array);
    workspaces
        .into_iter()
        .flatten()
        .filter_map(|workspace| workspace.get("tabs")?.as_array())
        .flatten()
        .any(|tab| tab.get("root").is_some_and(in_node))
}
