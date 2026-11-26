//! Pure data model, status mapping, and layered layout for the Epic and
//! child-issue dependency graph.
//!
//! Nothing here depends on GPUI, so layout, filtering, cycle handling, and
//! error guidance are tested directly as fast unit tests.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

pub use benterm_github::{
    ClosedReason, Epic, GithubError, Issue, IssueRef, IssueState, PullRequest, PullRequestState,
    Repository, RepositoryIssues,
};

use benterm_core::Rgb;
use benterm_task::LocalIssuesError;

use crate::tasks::IssueAgentState;
use crate::theme::BoardTheme;

/// Visual status of a node in the Epic issue graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IssueNodeStatus {
    Epic,
    Done,
    Waiting,
    Working,
    Assigned,
    Todo,
}

impl IssueNodeStatus {
    /// Label shown on the node and in summaries, matching the mock.
    pub fn label(self) -> &'static str {
        match self {
            Self::Epic => "Epic",
            Self::Done => "完了",
            Self::Waiting => "入力待ち",
            Self::Working => "作業中",
            Self::Assigned => "タスク作成済み",
            Self::Todo => "未着手",
        }
    }

    /// Theme color for this status, matching the mock palette.
    pub fn color(self, theme: &BoardTheme) -> Rgb {
        match self {
            Self::Epic => theme.epic,
            Self::Done => theme.done,
            Self::Waiting => theme.waiting,
            Self::Working => theme.working,
            Self::Assigned => theme.idle,
            Self::Todo => theme.idle,
        }
    }
}

/// A single node in the Epic issue graph (the Epic itself or a direct child).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueNode {
    pub number: u64,
    pub title: String,
    pub status: IssueNodeStatus,
    pub url: String,
    pub is_epic: bool,
    /// Issue numbers inside this Epic that block this issue.
    pub blocked_by_in_epic: Vec<u64>,
    /// Issue numbers inside this Epic that this issue blocks.
    pub blocking_in_epic: Vec<u64>,
    /// Dependencies outside this Epic (not drawn in the graph, kept for detail view).
    pub external_blocked_by: Vec<IssueRef>,
    /// Issues outside this Epic blocked by this issue (not drawn, kept for detail view).
    pub external_blocking: Vec<IssueRef>,
    /// Closing pull requests for this issue (kept for detail view).
    pub closing_prs: Vec<PullRequest>,
}

/// Summary counts of direct child issues of an Epic.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IssueSummary {
    pub total_children: usize,
    pub done: usize,
    pub waiting: usize,
    pub working: usize,
    pub assigned: usize,
    pub todo: usize,
}

/// What set of issues is represented by an [`IssueGraphModel`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IssueGraphScope {
    /// A single Epic and its direct sub-issues.
    Epic { number: u64, title: String },
    /// All issues in the repository.
    Repository,
}

/// Full graph model built from a GitHub [`Epic`] or [`RepositoryIssues`] and local agent states.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueGraphModel {
    pub repository: Repository,
    pub scope: IssueGraphScope,
    /// All nodes (in Epic mode, index 0 is the Epic followed by direct child issues).
    pub nodes: Vec<IssueNode>,
    /// Direct parent-to-child links: `(parent_number, child_number)`.
    pub parent_edges: Vec<(u64, u64)>,
    /// Dependency arrows between issues inside the graph: `(blocker, blocked)`.
    pub dep_edges: Vec<(u64, u64)>,
    pub summary: IssueSummary,
}

impl IssueGraphModel {
    pub fn node(&self, number: u64) -> Option<&IssueNode> {
        self.nodes.iter().find(|n| n.number == number)
    }
}

/// Placement of a node in logical grid coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IssueSpot {
    pub number: u64,
    /// Horizontal position in node-width units (may be fractional for centering).
    pub column: f32,
    /// Vertical layer: 0 for Epic, 1+ for child issues.
    pub row: usize,
}

/// Result of laying out the visible portion of an [`IssueGraphModel`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IssueGraphLayout {
    pub spots: Vec<IssueSpot>,
    /// Visible solid lines from Epic to child issues.
    pub parent_edges: Vec<(u64, u64)>,
    /// Visible dashed dependency arrows `(blocker, blocked)`.
    pub dep_edges: Vec<(u64, u64)>,
}

impl IssueGraphLayout {
    pub fn spot(&self, number: u64) -> Option<IssueSpot> {
        self.spots.iter().find(|s| s.number == number).copied()
    }

    pub fn columns(&self) -> f32 {
        let last = self.spots.iter().map(|s| s.column).fold(-1.0_f32, f32::max);
        (last + 1.0).max(0.0)
    }

    pub fn rows(&self) -> usize {
        self.spots.iter().map(|s| s.row + 1).max().unwrap_or(0)
    }
}

/// Arrow-key navigation direction in the issue graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavDirection {
    Left,
    Right,
    Up,
    Down,
}

/// Computes the node status of a child issue from its GitHub state and local
/// agent state.
///
/// Closed issues are always `Done`. Open issues reflect `Waiting` or `Working`
/// when an associated task's agent is in that state; otherwise `Todo`.
pub fn child_node_status(issue_state: IssueState, agent_state: IssueAgentState) -> IssueNodeStatus {
    match issue_state {
        IssueState::Closed(_) => IssueNodeStatus::Done,
        IssueState::Open => match agent_state {
            IssueAgentState::Waiting => IssueNodeStatus::Waiting,
            IssueAgentState::Working => IssueNodeStatus::Working,
            IssueAgentState::None | IssueAgentState::Done | IssueAgentState::Error => {
                IssueNodeStatus::Todo
            }
        },
    }
}

/// The issues that are nodes of a graph, to tell whether a parent or a
/// dependency is one of them.
///
/// An issue of GitHub is known by its URL, which also tells the issue of
/// another repository apart. An issue of a local file has no URL and is
/// known by its number: one without a URL is never one with a URL.
struct IssueSet<'a> {
    urls: HashSet<&'a str>,
    numbers: HashSet<u64>,
}

impl<'a> IssueSet<'a> {
    fn of(issues: &'a [Issue]) -> Self {
        let mut set = Self {
            urls: HashSet::new(),
            numbers: HashSet::new(),
        };
        for issue in issues {
            if issue.url.is_empty() {
                set.numbers.insert(issue.number);
            } else {
                set.urls.insert(&issue.url);
            }
        }
        set
    }

    fn has(&self, other: &IssueRef) -> bool {
        if other.url.is_empty() {
            self.numbers.contains(&other.number)
        } else {
            self.urls.contains(other.url.as_str())
        }
    }
}

fn is_same_issue(other: &IssueRef, issue: &Issue) -> bool {
    if other.url.is_empty() || issue.url.is_empty() {
        other.url == issue.url && other.number == issue.number
    } else {
        other.url == issue.url
    }
}

/// Builds an [`IssueGraphModel`] from a fetched [`Epic`] and local agent states.
pub fn build_model(epic: &Epic, agent_states: &BTreeMap<u64, IssueAgentState>) -> IssueGraphModel {
    let epic_number = epic.issue.number;
    let children = IssueSet::of(&epic.sub_issues);

    let mut nodes = Vec::with_capacity(1 + epic.sub_issues.len());
    let mut parent_edges = Vec::with_capacity(epic.sub_issues.len());
    let mut dep_edges = Vec::new();
    let mut seen_deps: HashSet<(u64, u64)> = HashSet::new();
    let mut summary = IssueSummary {
        total_children: epic.sub_issues.len(),
        ..IssueSummary::default()
    };

    // Epic root node.
    nodes.push(IssueNode {
        number: epic.issue.number,
        title: epic.issue.title.clone(),
        status: IssueNodeStatus::Epic,
        url: epic.issue.url.clone(),
        is_epic: true,
        blocked_by_in_epic: Vec::new(),
        blocking_in_epic: Vec::new(),
        external_blocked_by: epic.issue.blocked_by.clone(),
        external_blocking: epic.issue.blocking.clone(),
        closing_prs: epic.issue.closing_prs.clone(),
    });

    for child in &epic.sub_issues {
        let agent_state = agent_states
            .get(&child.number)
            .copied()
            .unwrap_or(IssueAgentState::None);
        let status = child_node_status(child.state, agent_state);
        match status {
            IssueNodeStatus::Done => summary.done += 1,
            IssueNodeStatus::Waiting => summary.waiting += 1,
            IssueNodeStatus::Working => summary.working += 1,
            IssueNodeStatus::Todo => summary.todo += 1,
            IssueNodeStatus::Assigned => summary.assigned += 1,
            IssueNodeStatus::Epic => {}
        }

        let mut blocked_by_in_epic = Vec::new();
        let mut external_blocked_by = Vec::new();
        for dep in &child.blocked_by {
            if is_same_issue(dep, child) {
                continue;
            }
            if children.has(dep) {
                if !blocked_by_in_epic.contains(&dep.number) {
                    blocked_by_in_epic.push(dep.number);
                }
                if seen_deps.insert((dep.number, child.number)) {
                    dep_edges.push((dep.number, child.number));
                }
            } else {
                external_blocked_by.push(dep.clone());
            }
        }

        let mut blocking_in_epic = Vec::new();
        let mut external_blocking = Vec::new();
        for dep in &child.blocking {
            if is_same_issue(dep, child) {
                continue;
            }
            if children.has(dep) {
                if !blocking_in_epic.contains(&dep.number) {
                    blocking_in_epic.push(dep.number);
                }
                if seen_deps.insert((child.number, dep.number)) {
                    dep_edges.push((child.number, dep.number));
                }
            } else {
                external_blocking.push(dep.clone());
            }
        }

        parent_edges.push((epic_number, child.number));
        nodes.push(IssueNode {
            number: child.number,
            title: child.title.clone(),
            status,
            url: child.url.clone(),
            is_epic: false,
            blocked_by_in_epic,
            blocking_in_epic,
            external_blocked_by,
            external_blocking,
            closing_prs: child.closing_prs.clone(),
        });
    }

    IssueGraphModel {
        repository: epic.repository.clone(),
        scope: IssueGraphScope::Epic {
            number: epic_number,
            title: epic.issue.title.clone(),
        },
        nodes,
        parent_edges,
        dep_edges,
        summary,
    }
}

/// Builds an [`IssueGraphModel`] for all issues in a repository, preserving
/// parent/sub-issue links and blocking dependencies between issues in the
/// repository while also including standalone issues.
pub fn build_repository_model(
    repo_issues: &RepositoryIssues,
    agent_states: &BTreeMap<u64, IssueAgentState>,
) -> IssueGraphModel {
    let issues = IssueSet::of(&repo_issues.issues);

    let mut nodes = Vec::with_capacity(repo_issues.issues.len());
    let mut parent_edges = Vec::new();
    let mut seen_parents: HashSet<(u64, u64)> = HashSet::new();
    let mut dep_edges = Vec::new();
    let mut seen_deps: HashSet<(u64, u64)> = HashSet::new();
    let mut summary = IssueSummary {
        total_children: repo_issues.issues.len(),
        ..IssueSummary::default()
    };

    for issue in &repo_issues.issues {
        let agent_state = agent_states
            .get(&issue.number)
            .copied()
            .unwrap_or(IssueAgentState::None);
        let status = child_node_status(issue.state, agent_state);
        match status {
            IssueNodeStatus::Done => summary.done += 1,
            IssueNodeStatus::Waiting => summary.waiting += 1,
            IssueNodeStatus::Working => summary.working += 1,
            IssueNodeStatus::Todo => summary.todo += 1,
            IssueNodeStatus::Assigned => summary.assigned += 1,
            IssueNodeStatus::Epic => {}
        }

        if let Some(parent) = &issue.parent
            && !is_same_issue(parent, issue)
            && issues.has(parent)
            && seen_parents.insert((parent.number, issue.number))
        {
            parent_edges.push((parent.number, issue.number));
        }

        let mut blocked_by_in_epic = Vec::new();
        let mut external_blocked_by = Vec::new();
        for dep in &issue.blocked_by {
            if is_same_issue(dep, issue) {
                continue;
            }
            if issues.has(dep) {
                if !blocked_by_in_epic.contains(&dep.number) {
                    blocked_by_in_epic.push(dep.number);
                }
                if seen_deps.insert((dep.number, issue.number)) {
                    dep_edges.push((dep.number, issue.number));
                }
            } else {
                external_blocked_by.push(dep.clone());
            }
        }

        let mut blocking_in_epic = Vec::new();
        let mut external_blocking = Vec::new();
        for dep in &issue.blocking {
            if is_same_issue(dep, issue) {
                continue;
            }
            if issues.has(dep) {
                if !blocking_in_epic.contains(&dep.number) {
                    blocking_in_epic.push(dep.number);
                }
                if seen_deps.insert((issue.number, dep.number)) {
                    dep_edges.push((issue.number, dep.number));
                }
            } else {
                external_blocking.push(dep.clone());
            }
        }

        nodes.push(IssueNode {
            number: issue.number,
            title: issue.title.clone(),
            status,
            url: issue.url.clone(),
            is_epic: false,
            blocked_by_in_epic,
            blocking_in_epic,
            external_blocked_by,
            external_blocking,
            closing_prs: issue.closing_prs.clone(),
        });
    }

    IssueGraphModel {
        repository: repo_issues.repository.clone(),
        scope: IssueGraphScope::Repository,
        nodes,
        parent_edges,
        dep_edges,
        summary,
    }
}

/// Maximum number of child nodes placed on a single visual row before wrapping
/// into subsequent rows, keeping wide Epics compact and readable.
pub const MAX_ROW_COLUMNS: usize = 8;

/// Lays out `model`, optionally hiding closed (`Done`) child issues and any
/// edges connected to them.
///
/// The Epic is always placed at row 0. Visible child issues are assigned to
/// logical dependency layers using longest-path layering with deterministic
/// cycle breaking, and layers wider than [`MAX_ROW_COLUMNS`] wrap across
/// consecutive visual rows.
pub fn layout_graph(model: &IssueGraphModel, hide_done: bool) -> IssueGraphLayout {
    if model.nodes.is_empty() {
        return IssueGraphLayout::default();
    }

    let visible_nodes: Vec<&IssueNode> = model
        .nodes
        .iter()
        .filter(|n| n.is_epic || !hide_done || n.status != IssueNodeStatus::Done)
        .collect();
    let visible_set: HashSet<u64> = visible_nodes.iter().map(|n| n.number).collect();

    let parent_edges: Vec<(u64, u64)> = model
        .parent_edges
        .iter()
        .copied()
        .filter(|(a, b)| visible_set.contains(a) && visible_set.contains(b))
        .collect();

    let dep_edges: Vec<(u64, u64)> = model
        .dep_edges
        .iter()
        .copied()
        .filter(|(a, b)| a != b && visible_set.contains(a) && visible_set.contains(b))
        .collect();

    let has_epic = visible_nodes.iter().any(|n| n.is_epic);
    let base_layer = usize::from(has_epic);

    let child_ids: Vec<u64> = visible_nodes
        .iter()
        .filter(|n| !n.is_epic)
        .map(|n| n.number)
        .collect();
    let order_index: HashMap<u64, usize> = child_ids
        .iter()
        .enumerate()
        .map(|(i, &id)| (id, i))
        .collect();

    let mut adj: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut in_degree: HashMap<u64, usize> = child_ids.iter().map(|&id| (id, 0)).collect();
    let mut seen_layout_edges: HashSet<(u64, u64)> = HashSet::new();
    for &(from, to) in parent_edges.iter().chain(dep_edges.iter()) {
        if from != to
            && order_index.contains_key(&from)
            && order_index.contains_key(&to)
            && seen_layout_edges.insert((from, to))
        {
            adj.entry(from).or_default().push(to);
            *in_degree.entry(to).or_insert(0) += 1;
        }
    }

    let mut layer: HashMap<u64, usize> = child_ids.iter().map(|&id| (id, base_layer)).collect();
    let mut placed: HashSet<u64> = HashSet::with_capacity(child_ids.len());
    let mut ready: VecDeque<u64> = child_ids
        .iter()
        .copied()
        .filter(|id| in_degree[id] == 0)
        .collect();

    while placed.len() < child_ids.len() {
        let next = match ready.pop_front() {
            Some(id) => id,
            None => {
                // Remaining nodes are in or downstream of a cycle; break
                // deterministically by smallest remaining in-degree, then order.
                *child_ids
                    .iter()
                    .filter(|id| !placed.contains(id))
                    .min_by_key(|id| (in_degree[id], order_index[id]))
                    .expect("unplaced child exists")
            }
        };
        if !placed.insert(next) {
            continue;
        }
        let next_layer = layer[&next];
        if let Some(neighbors) = adj.get(&next) {
            for &v in neighbors {
                if placed.contains(&v) {
                    continue;
                }
                let entry = layer.entry(v).or_insert(base_layer);
                if *entry <= next_layer {
                    *entry = next_layer + 1;
                }
                if let Some(deg) = in_degree.get_mut(&v) {
                    *deg = deg.saturating_sub(1);
                    if *deg == 0 {
                        ready.push_back(v);
                    }
                }
            }
        }
    }

    // Group visible nodes by logical layer (layer 0 = Epic or root issues),
    // then wrap layers wider than MAX_ROW_COLUMNS into consecutive visual rows.
    let mut logical_rows: BTreeMap<usize, Vec<u64>> = BTreeMap::new();
    for node in &visible_nodes {
        let row = if node.is_epic {
            0
        } else {
            layer.get(&node.number).copied().unwrap_or(base_layer)
        };
        logical_rows.entry(row).or_default().push(node.number);
    }

    let mut visual_rows: Vec<Vec<u64>> = Vec::new();
    for (_layer_idx, members) in logical_rows {
        for chunk in members.chunks(MAX_ROW_COLUMNS) {
            visual_rows.push(chunk.to_vec());
        }
    }

    let max_width = visual_rows.iter().map(Vec::len).max().unwrap_or(1) as f32;
    let mut spot_map: HashMap<u64, IssueSpot> = HashMap::with_capacity(visible_nodes.len());
    for (row, members) in visual_rows.into_iter().enumerate() {
        let offset = (max_width - members.len() as f32) / 2.0;
        for (i, number) in members.into_iter().enumerate() {
            spot_map.insert(
                number,
                IssueSpot {
                    number,
                    column: offset + i as f32,
                    row,
                },
            );
        }
    }

    let spots = visible_nodes
        .iter()
        .filter_map(|n| spot_map.get(&n.number).copied())
        .collect();

    IssueGraphLayout {
        spots,
        parent_edges,
        dep_edges,
    }
}

/// Moves keyboard selection from `current` in `direction` using `layout`.
pub fn move_selection(
    layout: &IssueGraphLayout,
    current: Option<u64>,
    direction: NavDirection,
) -> Option<u64> {
    if layout.spots.is_empty() {
        return None;
    }
    let Some(cur) = current.and_then(|id| layout.spot(id)) else {
        return Some(layout.spots[0].number);
    };

    match direction {
        NavDirection::Left | NavDirection::Right => {
            let mut ordered = layout.spots.clone();
            ordered.sort_by(|a, b| {
                a.row
                    .cmp(&b.row)
                    .then_with(|| a.column.total_cmp(&b.column))
            });
            let pos = ordered.iter().position(|s| s.number == cur.number)?;
            let next = match direction {
                NavDirection::Right => (pos + 1).min(ordered.len() - 1),
                NavDirection::Left => pos.saturating_sub(1),
                _ => unreachable!(),
            };
            Some(ordered[next].number)
        }
        NavDirection::Up => {
            let target_row = layout
                .spots
                .iter()
                .filter(|s| s.row < cur.row)
                .map(|s| s.row)
                .max()?;
            layout
                .spots
                .iter()
                .filter(|s| s.row == target_row)
                .min_by(|a, b| {
                    (a.column - cur.column)
                        .abs()
                        .total_cmp(&(b.column - cur.column).abs())
                })
                .map(|s| s.number)
        }
        NavDirection::Down => {
            let target_row = layout
                .spots
                .iter()
                .filter(|s| s.row > cur.row)
                .map(|s| s.row)
                .min()?;
            layout
                .spots
                .iter()
                .filter(|s| s.row == target_row)
                .min_by(|a, b| {
                    (a.column - cur.column)
                        .abs()
                        .total_cmp(&(b.column - cur.column).abs())
                })
                .map(|s| s.number)
        }
    }
}

/// Parses an Epic number from user input (e.g. `"120"` or `"#120"`).
pub fn parse_epic_number(input: &str) -> Option<u64> {
    let trimmed = input.trim();
    let digits = trimmed.strip_prefix('#').unwrap_or(trimmed);
    let number: u64 = digits.parse().ok()?;
    (number > 0).then_some(number)
}

/// Why the issues of a graph could not be read: from GitHub, or from the
/// file a repository keeps its local issues in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IssueReadError {
    Github(GithubError),
    Local(LocalIssuesError),
}

impl From<GithubError> for IssueReadError {
    fn from(error: GithubError) -> Self {
        Self::Github(error)
    }
}

impl From<LocalIssuesError> for IssueReadError {
    fn from(error: LocalIssuesError) -> Self {
        Self::Local(error)
    }
}

impl std::fmt::Display for IssueReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Github(error) => error.fmt(f),
            Self::Local(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for IssueReadError {}

/// Returns a user-facing `(reason, fix_hint)` pair for a read error.
pub fn read_error_guidance(error: &IssueReadError) -> (String, String) {
    match error {
        IssueReadError::Github(error) => error_guidance(error),
        IssueReadError::Local(error) => local_error_guidance(error),
    }
}

/// Returns a user-facing `(reason, fix_hint)` pair for a local issues
/// file that could not be read.
pub fn local_error_guidance(error: &LocalIssuesError) -> (String, String) {
    let file = format!(
        "{}/{}",
        benterm_task::LOCAL_ISSUES_DIRECTORY,
        benterm_task::LOCAL_ISSUES_FILE
    );
    match error {
        LocalIssuesError::NotGitRepository(_) => (
            "現在のディレクトリは git リポジトリではありません。".to_owned(),
            "git リポジトリのペインでローカル Issue を開いてください（リモートは不要です）。"
                .to_owned(),
        ),
        LocalIssuesError::Missing => (
            format!("リポジトリに {file} がありません。"),
            format!(
                "リポジトリの直下に {file} を作ってください。例: \
                 {{\"version\": 1, \"issues\": [{{\"number\": 1, \"title\": \"最初の Issue\"}}]}}"
            ),
        ),
        LocalIssuesError::NotPlainFile(_) => (
            format!(
                "{file} またはそのディレクトリが、通常のファイルやディレクトリではありません。"
            ),
            "シンボリックリンクはたどりません。リポジトリの中に実体のファイルを置いてください。"
                .to_owned(),
        ),
        LocalIssuesError::TooLarge { limit } => (
            format!("{file} が大きすぎます（上限 {} KiB）。", limit / 1024),
            "完了した Issue を消すなどして、ファイルを小さくしてください。".to_owned(),
        ),
        LocalIssuesError::Io(detail) => (
            format!("{file} を読めませんでした: {detail}"),
            "ファイルの権限を確認してから再試行してください。".to_owned(),
        ),
        LocalIssuesError::Invalid(detail) => (
            format!("{file} の形式が正しくありません: {detail}"),
            "ファイルを直して保存すると、数秒後に読み直します。".to_owned(),
        ),
        LocalIssuesError::IssueNotFound(number) => (
            format!("{file} に Issue #{number} がありません。"),
            "番号を確認し、Open Local Epic から開き直してください。".to_owned(),
        ),
        LocalIssuesError::Write(detail) => (
            format!("{file} に書き込めませんでした: {detail}"),
            "ファイルとディレクトリの権限を確認してから、もう一度操作してください。".to_owned(),
        ),
        LocalIssuesError::Refused(detail) => (
            format!("変更できませんでした: {detail}"),
            "指定した番号と、親や依存が循環していないかを確認してください。".to_owned(),
        ),
    }
}

/// Returns a user-facing `(reason, fix_hint)` pair for a GitHub read error.
pub fn error_guidance(error: &GithubError) -> (String, String) {
    match error {
        GithubError::GhNotFound => (
            "gh コマンドが見つかりません。".to_owned(),
            "GitHub CLI (gh) をインストールして PATH に通してください（例: brew install gh）。"
                .to_owned(),
        ),
        GithubError::NotLoggedIn(_) => (
            "GitHub にログインしていません。".to_owned(),
            "ターミナルで `gh auth login` を実行してログインしてください。".to_owned(),
        ),
        GithubError::Network(_) => (
            "GitHub に接続できませんでした。".to_owned(),
            "ネットワーク接続やプロキシ設定を確認してから再試行してください。".to_owned(),
        ),
        GithubError::EpicNotFound { repository, number } => (
            format!("{repository} に Epic #{number} が見つかりませんでした。"),
            "Issue 番号・リポジトリ・アクセス権限を確認し、Open Epic から開き直してください。"
                .to_owned(),
        ),
        GithubError::RepositoryNotFound { repository } => (
            format!("リポジトリ {repository} が見つかりませんでした。"),
            "リポジトリ名とアクセス権限（gh auth status）を確認してから再試行してください。"
                .to_owned(),
        ),
        GithubError::NotGithubRepository(_) => (
            "現在のディレクトリは GitHub リポジトリではありません。".to_owned(),
            "GitHub のリモートを持つリポジトリのペインで Issues タブまたは Open Epic を実行してください。"
                .to_owned(),
        ),
        GithubError::TimedOut(_) => (
            "GitHub からの読み取りがタイムアウトしました。".to_owned(),
            "ネットワーク状況を確認し、しばらくしてから再試行してください。".to_owned(),
        ),
        GithubError::OutputTooLarge(_) => (
            "GitHub からの応答サイズが大きすぎます。".to_owned(),
            "Issue 数または Epic の子 Issue 数を確認してください。".to_owned(),
        ),
        GithubError::InvalidResponse(detail) => (
            format!("GitHub からの応答を解釈できませんでした: {detail}"),
            "gh のバージョンを確認し、再試行してください。".to_owned(),
        ),
        GithubError::CommandFailed(detail) => (
            format!("GitHub の読み取りに失敗しました: {detail}"),
            "gh auth status で認証状態を確認してから再試行してください。".to_owned(),
        ),
    }
}

/// Returns a made-up Epic matching `mock/patterns.html` for screenshots and demos.
pub fn made_up_epic() -> (Epic, BTreeMap<u64, IssueAgentState>) {
    let repo = Repository {
        owner: "example".to_owned(),
        name: "api".to_owned(),
    };
    let make_ref = |number: u64, title: &str, state: IssueState| IssueRef {
        number,
        title: title.to_owned(),
        state,
        url: format!("https://github.com/example/api/issues/{number}"),
    };
    let done = IssueState::Closed(Some(ClosedReason::Completed));
    let open = IssueState::Open;

    let r122 = make_ref(122, "refresh のタイムアウトを設定化", open);
    let r124 = make_ref(124, "AuthRefreshError の導入", open);
    let r125 = make_ref(125, "失敗時のリトライ", open);
    let r126 = make_ref(126, "リトライ回数のメトリクス", open);
    let r127 = make_ref(127, "認証エラーのドキュメント", open);

    let make_issue = |number: u64,
                      title: &str,
                      state: IssueState,
                      blocked_by: Vec<IssueRef>,
                      blocking: Vec<IssueRef>| {
        Issue {
            number,
            title: title.to_owned(),
            state,
            url: format!("https://github.com/example/api/issues/{number}"),
            parent: None,
            blocked_by,
            blocking,
            closing_prs: Vec::new(),
        }
    };

    let mut epic = Epic {
        repository: repo,
        issue: make_issue(120, "認証まわりの改修", open, vec![], vec![]),
        sub_issues: vec![
            make_issue(121, "トークン保存先を Keychain へ", done, vec![], vec![]),
            make_issue(
                122,
                "refresh のタイムアウトを設定化",
                open,
                vec![make_ref(130, "認証APIの仕様を確定する", open)],
                vec![r124.clone(), r125.clone()],
            ),
            make_issue(123, "ログイン後のリダイレクト修正", done, vec![], vec![]),
            make_issue(
                124,
                "AuthRefreshError の導入",
                open,
                vec![r122.clone()],
                vec![r127.clone()],
            ),
            make_issue(
                125,
                "失敗時のリトライ",
                open,
                vec![r122],
                vec![r126.clone()],
            ),
            make_issue(126, "リトライ回数のメトリクス", open, vec![r125], vec![]),
            make_issue(127, "認証エラーのドキュメント", open, vec![r124], vec![]),
        ],
    };

    epic.sub_issues[1].closing_prs.push(PullRequest {
        number: 134,
        state: PullRequestState::Draft,
        head_ref_name: "issue-122".into(),
        url: "https://github.com/example/api/pull/134".into(),
    });

    let mut states = BTreeMap::new();
    states.insert(122, IssueAgentState::Waiting);
    states.insert(124, IssueAgentState::Working);
    states.insert(125, IssueAgentState::Working);
    (epic, states)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue_ref(number: u64, state: IssueState) -> IssueRef {
        IssueRef {
            number,
            title: format!("Issue #{number}"),
            state,
            url: format!("https://github.com/example/repo/issues/{number}"),
        }
    }

    fn issue(
        number: u64,
        title: &str,
        state: IssueState,
        blocked_by: Vec<IssueRef>,
        blocking: Vec<IssueRef>,
    ) -> Issue {
        Issue {
            number,
            title: title.to_owned(),
            state,
            url: format!("https://github.com/example/repo/issues/{number}"),
            parent: None,
            blocked_by,
            blocking,
            closing_prs: Vec::new(),
        }
    }

    fn sample_epic_3_children_2_deps() -> Epic {
        // Epic #100 with 3 children: #101, #102, #103
        // Dependencies: #101 -> #102, #102 -> #103
        let r101 = issue_ref(101, IssueState::Closed(Some(ClosedReason::Completed)));
        let r102 = issue_ref(102, IssueState::Open);
        let r103 = issue_ref(103, IssueState::Open);
        Epic {
            repository: Repository {
                owner: "example".to_owned(),
                name: "repo".to_owned(),
            },
            issue: issue(100, "Epic 100", IssueState::Open, vec![], vec![]),
            sub_issues: vec![
                issue(
                    101,
                    "Child 1",
                    IssueState::Closed(Some(ClosedReason::Completed)),
                    vec![],
                    vec![r102.clone()],
                ),
                issue(102, "Child 2", IssueState::Open, vec![r101], vec![r103]),
                issue(103, "Child 3", IssueState::Open, vec![r102], vec![]),
            ],
        }
    }

    #[test]
    fn three_children_and_two_deps_layout_as_four_nodes_three_solid_and_two_dashed() {
        let epic = sample_epic_3_children_2_deps();
        let mut states = BTreeMap::new();
        states.insert(102, IssueAgentState::Working);
        let model = build_model(&epic, &states);
        let layout = layout_graph(&model, false);

        assert_eq!(layout.spots.len(), 4);
        assert_eq!(layout.parent_edges.len(), 3);
        assert_eq!(layout.dep_edges.len(), 2);

        // Epic is at row 0, children are ordered by dependency depth.
        let s100 = layout.spot(100).unwrap();
        let s101 = layout.spot(101).unwrap();
        let s102 = layout.spot(102).unwrap();
        let s103 = layout.spot(103).unwrap();
        assert_eq!(s100.row, 0);
        assert_eq!(s101.row, 1);
        assert_eq!(s102.row, 2);
        assert_eq!(s103.row, 3);
    }

    #[test]
    fn cyclic_dependencies_terminate_and_place_all_nodes() {
        // Cycle: 201 -> 202 -> 203 -> 201
        let r201 = issue_ref(201, IssueState::Open);
        let r202 = issue_ref(202, IssueState::Open);
        let r203 = issue_ref(203, IssueState::Open);
        let epic = Epic {
            repository: Repository {
                owner: "example".to_owned(),
                name: "repo".to_owned(),
            },
            issue: issue(200, "Cyclic Epic", IssueState::Open, vec![], vec![]),
            sub_issues: vec![
                issue(
                    201,
                    "A",
                    IssueState::Open,
                    vec![r203.clone()],
                    vec![r202.clone()],
                ),
                issue(202, "B", IssueState::Open, vec![r201.clone()], vec![r203]),
                issue(203, "C", IssueState::Open, vec![r202], vec![r201]),
            ],
        };
        let model = build_model(&epic, &BTreeMap::new());
        let layout = layout_graph(&model, false);

        assert_eq!(layout.spots.len(), 4);
        for id in [200, 201, 202, 203] {
            assert!(layout.spot(id).is_some(), "missing spot for #{id}");
        }
        assert_eq!(layout.parent_edges.len(), 3);
        assert_eq!(layout.dep_edges.len(), 3);
    }

    #[test]
    fn hide_done_removes_closed_nodes_and_their_edges() {
        let epic = sample_epic_3_children_2_deps();
        let model = build_model(&epic, &BTreeMap::new());
        let layout = layout_graph(&model, true);

        // #101 is closed, so only #100, #102, #103 remain.
        assert_eq!(layout.spots.len(), 3);
        assert!(layout.spot(101).is_none());
        assert!(layout.spot(100).is_some());
        assert!(layout.spot(102).is_some());
        assert!(layout.spot(103).is_some());

        // Parent edge (100, 101) is gone; (100, 102) and (100, 103) remain.
        assert_eq!(layout.parent_edges.len(), 2);
        assert!(
            !layout
                .parent_edges
                .iter()
                .any(|&(a, b)| a == 101 || b == 101)
        );

        // Dependency (101, 102) is gone; (102, 103) remains.
        assert_eq!(layout.dep_edges, vec![(102, 103)]);
    }

    #[test]
    fn external_dependencies_are_excluded_from_graph_edges_but_retained_on_node() {
        let external_blocker = IssueRef {
            url: "https://github.com/example/other/issues/301".into(),
            ..issue_ref(301, IssueState::Open)
        };
        let external_blocked = issue_ref(998, IssueState::Open);
        let epic = Epic {
            repository: Repository {
                owner: "example".to_owned(),
                name: "repo".to_owned(),
            },
            issue: issue(300, "External Dep Epic", IssueState::Open, vec![], vec![]),
            sub_issues: vec![issue(
                301,
                "Child with external deps",
                IssueState::Open,
                vec![external_blocker.clone()],
                vec![external_blocked.clone()],
            )],
        };
        let model = build_model(&epic, &BTreeMap::new());
        assert!(model.dep_edges.is_empty());
        let child = model.nodes.iter().find(|n| n.number == 301).unwrap();
        assert_eq!(child.external_blocked_by, vec![external_blocker]);
        assert_eq!(child.external_blocking, vec![external_blocked]);
    }

    #[test]
    fn child_status_maps_closed_and_agent_states_correctly() {
        assert_eq!(
            child_node_status(
                IssueState::Closed(Some(ClosedReason::Completed)),
                IssueAgentState::Working
            ),
            IssueNodeStatus::Done
        );
        assert_eq!(
            child_node_status(IssueState::Closed(None), IssueAgentState::None),
            IssueNodeStatus::Done
        );
        assert_eq!(
            child_node_status(IssueState::Open, IssueAgentState::Waiting),
            IssueNodeStatus::Waiting
        );
        assert_eq!(
            child_node_status(IssueState::Open, IssueAgentState::Working),
            IssueNodeStatus::Working
        );
        assert_eq!(
            child_node_status(IssueState::Open, IssueAgentState::None),
            IssueNodeStatus::Todo
        );
        assert_eq!(
            child_node_status(IssueState::Open, IssueAgentState::Done),
            IssueNodeStatus::Todo
        );
        assert_eq!(
            child_node_status(IssueState::Open, IssueAgentState::Error),
            IssueNodeStatus::Todo
        );
    }

    #[test]
    fn parse_epic_number_accepts_plain_and_hash_prefixed_numbers() {
        assert_eq!(parse_epic_number("120"), Some(120));
        assert_eq!(parse_epic_number(" #120 "), Some(120));
        assert_eq!(parse_epic_number(""), None);
        assert_eq!(parse_epic_number("0"), None);
        assert_eq!(parse_epic_number("abc"), None);
    }

    #[test]
    fn error_guidance_explains_reason_and_how_to_fix() {
        let (reason, hint) = error_guidance(&GithubError::GhNotFound);
        assert!(!reason.is_empty());
        assert!(hint.contains("gh"));

        let (reason, hint) = error_guidance(&GithubError::NotLoggedIn("no token".into()));
        assert!(!reason.is_empty());
        assert!(hint.contains("gh auth login"));
    }

    #[test]
    fn move_selection_navigates_rows_and_columns() {
        let epic = sample_epic_3_children_2_deps();
        let model = build_model(&epic, &BTreeMap::new());
        let layout = layout_graph(&model, false);

        assert_eq!(move_selection(&layout, None, NavDirection::Down), Some(100));
        assert_eq!(
            move_selection(&layout, Some(100), NavDirection::Down),
            Some(101)
        );
        assert_eq!(
            move_selection(&layout, Some(101), NavDirection::Down),
            Some(102)
        );
        assert_eq!(
            move_selection(&layout, Some(102), NavDirection::Up),
            Some(101)
        );
    }

    #[test]
    fn wide_layer_wraps_into_multiple_rows() {
        let mut sub_issues: Vec<Issue> = (1..=10)
            .map(|i| {
                issue(
                    100 + i,
                    &format!("Child {i}"),
                    IssueState::Open,
                    vec![],
                    vec![],
                )
            })
            .collect();
        // Add a downstream child blocked by #101 so it belongs to logical layer 2.
        let r101 = issue_ref(101, IssueState::Open);
        sub_issues.push(issue(
            111,
            "Downstream",
            IssueState::Open,
            vec![r101],
            vec![],
        ));

        let epic = Epic {
            repository: Repository {
                owner: "example".to_owned(),
                name: "repo".to_owned(),
            },
            issue: issue(100, "Wide Epic", IssueState::Open, vec![], vec![]),
            sub_issues,
        };
        let model = build_model(&epic, &BTreeMap::new());
        let layout = layout_graph(&model, false);

        assert_eq!(layout.spot(100).unwrap().row, 0);
        // 10 independent children wrap into row 1 (8 items) and row 2 (2 items).
        assert_eq!(layout.spot(101).unwrap().row, 1);
        assert_eq!(layout.spot(108).unwrap().row, 1);
        assert_eq!(layout.spot(109).unwrap().row, 2);
        assert_eq!(layout.spot(110).unwrap().row, 2);
        // Downstream child is placed on row 3 below the wrapped layer.
        assert_eq!(layout.spot(111).unwrap().row, 3);
        assert!(layout.columns() <= MAX_ROW_COLUMNS as f32);
    }

    #[test]
    fn repository_model_builds_parent_and_dependency_edges_with_standalone_issues() {
        let r1 = issue_ref(1, IssueState::Open);
        let r2 = issue_ref(2, IssueState::Closed(Some(ClosedReason::Completed)));
        let r3 = issue_ref(3, IssueState::Open);
        let external = IssueRef {
            url: "https://github.com/example/other/issues/99".into(),
            ..issue_ref(99, IssueState::Open)
        };

        let mut child2 = issue(
            2,
            "Child 2",
            IssueState::Closed(Some(ClosedReason::Completed)),
            vec![],
            vec![r3.clone()],
        );
        child2.parent = Some(r1.clone());

        let mut child3 = issue(
            3,
            "Child 3",
            IssueState::Open,
            vec![r2, external.clone()],
            vec![],
        );
        child3.parent = Some(r1);

        let standalone = issue(4, "Standalone", IssueState::Open, vec![], vec![]);

        let repo_issues = RepositoryIssues {
            repository: Repository {
                owner: "example".to_owned(),
                name: "repo".to_owned(),
            },
            issues: vec![
                issue(1, "Parent 1", IssueState::Open, vec![], vec![]),
                child2,
                child3,
                standalone,
            ],
        };

        let mut states = BTreeMap::new();
        states.insert(3, IssueAgentState::Working);
        let model = build_repository_model(&repo_issues, &states);

        assert_eq!(model.scope, IssueGraphScope::Repository);
        assert_eq!(model.nodes.len(), 4);
        assert_eq!(model.parent_edges, vec![(1, 2), (1, 3)]);
        assert_eq!(model.dep_edges, vec![(2, 3)]);
        assert_eq!(model.summary.total_children, 4);
        assert_eq!(model.summary.done, 1);
        assert_eq!(model.summary.working, 1);
        assert_eq!(model.summary.todo, 2);
        assert_eq!(model.node(3).unwrap().external_blocked_by, vec![external]);

        let layout = layout_graph(&model, false);
        assert_eq!(layout.spots.len(), 4);
        // Root parent #1 and standalone #4 sit on row 0; child #2 on row 1; #3 (blocked by #2) on row 2.
        assert_eq!(layout.spot(1).unwrap().row, 0);
        assert_eq!(layout.spot(4).unwrap().row, 0);
        assert_eq!(layout.spot(2).unwrap().row, 1);
        assert_eq!(layout.spot(3).unwrap().row, 2);
    }

    #[test]
    fn repository_hide_done_removes_closed_parents_and_wraps_standalone_issues() {
        let closed_parent_ref = issue_ref(10, IssueState::Closed(Some(ClosedReason::Completed)));
        let mut child = issue(11, "Open child", IssueState::Open, vec![], vec![]);
        child.parent = Some(closed_parent_ref);

        let mut issues = vec![
            issue(
                10,
                "Closed parent",
                IssueState::Closed(Some(ClosedReason::Completed)),
                vec![],
                vec![],
            ),
            child,
        ];
        for i in 12..=21 {
            issues.push(issue(
                i,
                &format!("Standalone {i}"),
                IssueState::Open,
                vec![],
                vec![],
            ));
        }

        let repo_issues = RepositoryIssues {
            repository: Repository {
                owner: "example".to_owned(),
                name: "repo".to_owned(),
            },
            issues,
        };

        let model = build_repository_model(&repo_issues, &BTreeMap::new());
        let layout = layout_graph(&model, true);

        // Closed parent #10 and edge (10, 11) are removed; 11 open issues wrap across 2 rows.
        assert!(layout.spot(10).is_none());
        assert!(layout.parent_edges.is_empty());
        assert_eq!(layout.spots.len(), 11);
        assert_eq!(layout.rows(), 2);
        assert!(layout.columns() <= MAX_ROW_COLUMNS as f32);
    }

    /// Issues as a local file gives them: no URL, known by number alone.
    fn local_issue(number: u64, state: IssueState, blocked_by: &[u64], blocking: &[u64]) -> Issue {
        let reference = |number: &u64| IssueRef {
            number: *number,
            title: format!("Local #{number}"),
            state: IssueState::Open,
            url: String::new(),
        };
        Issue {
            number,
            title: format!("Local #{number}"),
            state,
            url: String::new(),
            parent: None,
            blocked_by: blocked_by.iter().map(reference).collect(),
            blocking: blocking.iter().map(reference).collect(),
            closing_prs: Vec::new(),
        }
    }

    fn local_repository() -> Repository {
        Repository {
            owner: String::new(),
            name: "/r/local".to_owned(),
        }
    }

    #[test]
    fn an_epic_of_issues_without_urls_resolves_dependencies_by_number() {
        let epic = Epic {
            repository: local_repository(),
            issue: local_issue(1, IssueState::Open, &[], &[]),
            sub_issues: vec![
                local_issue(2, IssueState::Closed(None), &[], &[3]),
                local_issue(3, IssueState::Open, &[2, 9], &[]),
            ],
        };
        let model = build_model(&epic, &BTreeMap::new());

        assert_eq!(model.parent_edges, vec![(1, 2), (1, 3)]);
        assert_eq!(model.dep_edges, vec![(2, 3)]);
        let third = model.node(3).unwrap();
        assert_eq!(third.blocked_by_in_epic, vec![2]);
        // #9 is not a child of the epic, so it is kept for the details only.
        assert_eq!(third.external_blocked_by.len(), 1);
        assert_eq!(third.external_blocked_by[0].number, 9);
        assert_eq!(model.node(2).unwrap().blocking_in_epic, vec![3]);
    }

    #[test]
    fn a_repository_of_issues_without_urls_resolves_parents_and_dependencies_by_number() {
        let mut child = local_issue(2, IssueState::Open, &[3], &[]);
        child.parent = Some(IssueRef {
            number: 1,
            title: "Local #1".into(),
            state: IssueState::Open,
            url: String::new(),
        });
        let repo_issues = RepositoryIssues {
            repository: local_repository(),
            issues: vec![
                local_issue(1, IssueState::Open, &[], &[]),
                child,
                local_issue(3, IssueState::Open, &[], &[2]),
            ],
        };
        let model = build_repository_model(&repo_issues, &BTreeMap::new());

        assert_eq!(model.parent_edges, vec![(1, 2)]);
        assert_eq!(model.dep_edges, vec![(3, 2)]);
        assert_eq!(model.node(2).unwrap().blocked_by_in_epic, vec![3]);
        assert!(model.node(2).unwrap().external_blocked_by.is_empty());
        assert_eq!(model.node(3).unwrap().blocking_in_epic, vec![2]);
    }

    #[test]
    fn an_issue_without_a_url_is_not_one_with_a_url_of_the_same_number() {
        // A GitHub issue #2 of another repository is not the local #2.
        let mut third = local_issue(3, IssueState::Open, &[], &[]);
        third.blocked_by.push(IssueRef {
            number: 2,
            title: "Elsewhere".into(),
            state: IssueState::Open,
            url: "https://github.com/example/other/issues/2".into(),
        });
        let repo_issues = RepositoryIssues {
            repository: local_repository(),
            issues: vec![local_issue(2, IssueState::Open, &[], &[]), third],
        };
        let model = build_repository_model(&repo_issues, &BTreeMap::new());
        assert!(model.dep_edges.is_empty());
        assert_eq!(model.node(3).unwrap().external_blocked_by.len(), 1);
    }

    #[test]
    fn error_guidance_handles_repository_not_found() {
        let (reason, hint) = error_guidance(&GithubError::RepositoryNotFound {
            repository: Repository {
                owner: "example".to_owned(),
                name: "missing".to_owned(),
            },
        });
        assert!(reason.contains("example/missing"));
        assert!(!hint.is_empty());
    }
}
