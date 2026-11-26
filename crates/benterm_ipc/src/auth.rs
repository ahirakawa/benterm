//! Per-request IPC credentials and authorization.
//!
//! Credentials are opaque random tokens minted by the server and mapped to a
//! [`Grant`]. When restricted mode is off (the default during rollout),
//! [`PermissionTable::authorize`] permits every request so older clients work
//! unchanged. When restricted mode is on, every request is checked against the
//! live table, including subsequent requests on an already-open connection.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::protocol::{ApiError, ErrorCode, PaneId, Request};

/// The level of access a credential grants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PermissionScope {
    /// Own-pane lifecycle hooks and reports (`pane.report_hook`,
    /// `pane.report_checked`, `agent.report`).
    Pane,
    /// Full control over the window, workspaces, panes, tasks, and agents.
    Control,
}

/// The authority associated with one credential.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Grant {
    /// The pane this credential is bound to, if any.
    pub pane: Option<PaneId>,
    pub scope: PermissionScope,
}

/// How long a request waits for the app to tell the process of a pane
/// whose program it is starting. The app tells it right after the start,
/// so only a request the program sent in its first moments waits at all.
const START_WAIT: Duration = if cfg!(test) {
    Duration::from_millis(300)
} else {
    Duration::from_secs(2)
};

/// What the table was told of the program a pane runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PaneProcess {
    /// The app is starting the program and has yet to tell its process.
    Starting,
    /// The program is this process.
    Started(u32),
}

#[derive(Default)]
struct State {
    restricted: bool,
    trusts_own_process: bool,
    tokens: HashMap<String, Grant>,
    pane_tokens: HashMap<PaneId, Vec<String>>,
    /// The programs of the panes whose credentials only their own
    /// processes may use.
    processes: HashMap<PaneId, PaneProcess>,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    /// Told when the process of a pane's program becomes known.
    process_told: Condvar,
}

/// Server-side table mapping opaque tokens to their [`Grant`]s.
///
/// Cheap to clone: all clones share the same underlying table.
#[derive(Clone)]
pub struct PermissionTable {
    shared: Arc<Shared>,
}

impl Default for PermissionTable {
    fn default() -> Self {
        Self::new(false)
    }
}

impl fmt::Debug for PermissionTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.lock();
        f.debug_struct("PermissionTable")
            .field("restricted", &state.restricted)
            .field("token_count", &state.tokens.len())
            .finish()
    }
}

impl PermissionTable {
    /// Creates a new permission table. When `restricted` is `false`, all
    /// requests are permitted regardless of token.
    pub fn new(restricted: bool) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    restricted,
                    ..State::default()
                }),
                process_told: Condvar::new(),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Whether restricted mode is currently enabled.
    pub fn is_restricted(&self) -> bool {
        self.lock().restricted
    }

    /// Enables or disables restricted mode.
    pub fn set_restricted(&self, restricted: bool) {
        self.lock().restricted = restricted;
    }

    /// Whether a client in the server's own process may make any request
    /// without a credential. The app turns this on: only its own code runs
    /// in its process, and the programs of its panes are processes of
    /// their own.
    pub fn trusts_own_process(&self) -> bool {
        self.lock().trusts_own_process
    }

    /// Turns [`trusts_own_process`](Self::trusts_own_process) on or off,
    /// for the connections accepted from now on.
    pub fn set_trusts_own_process(&self, trust: bool) {
        self.lock().trusts_own_process = trust;
    }

    /// Issues a new opaque token with [`PermissionScope::Pane`] bound to `pane`.
    pub fn issue_pane_token(&self, pane: PaneId) -> String {
        self.issue(Grant {
            pane: Some(pane),
            scope: PermissionScope::Pane,
        })
    }

    /// Issues a new opaque token with [`PermissionScope::Control`], optionally
    /// bound to a coordinator `pane`.
    pub fn issue_control_token(&self, pane: Option<PaneId>) -> String {
        self.issue(Grant {
            pane,
            scope: PermissionScope::Control,
        })
    }

    fn issue(&self, grant: Grant) -> String {
        let mut state = self.lock();
        let token = loop {
            let candidate = generate_token();
            if !state.tokens.contains_key(&candidate) {
                break candidate;
            }
        };
        state.tokens.insert(token.clone(), grant);
        if let Some(pane) = grant.pane {
            state
                .pane_tokens
                .entry(pane)
                .or_default()
                .push(token.clone());
        }
        token
    }

    /// Revokes a single token. Returns whether the token existed.
    pub fn revoke_token(&self, token: &str) -> bool {
        let mut state = self.lock();
        let Some(grant) = state.tokens.remove(token) else {
            return false;
        };
        if let Some(pane) = grant.pane
            && let Some(list) = state.pane_tokens.get_mut(&pane)
        {
            list.retain(|existing| existing != token);
            if list.is_empty() {
                state.pane_tokens.remove(&pane);
                state.processes.remove(&pane);
            }
        }
        true
    }

    /// Revokes every token bound to `pane`.
    pub fn revoke_pane(&self, pane: PaneId) {
        let mut state = self.lock();
        if let Some(tokens) = state.pane_tokens.remove(&pane) {
            for token in tokens {
                state.tokens.remove(&token);
            }
        }
        state.processes.remove(&pane);
        drop(state);
        // A request that waits for the pane's process finds its
        // credential gone.
        self.shared.process_told.notify_all();
    }

    /// Says that the app is about to start the program of `pane`, and
    /// will tell its process with
    /// [`bind_pane_process`](Self::bind_pane_process). From here on the
    /// credentials bound to `pane` are taken only from the processes of
    /// that program (see [`authorize_from`](Self::authorize_from)), and
    /// from nobody until the process is told.
    ///
    /// The credentials of a pane this was never said for are taken from
    /// any process: there is no program to tell the callers by.
    pub fn expect_pane_process(&self, pane: PaneId) {
        self.lock().processes.insert(pane, PaneProcess::Starting);
    }

    /// Tells that the program of `pane` is the process `pid`. See
    /// [`expect_pane_process`](Self::expect_pane_process).
    pub fn bind_pane_process(&self, pane: PaneId, pid: u32) {
        self.lock()
            .processes
            .insert(pane, PaneProcess::Started(pid));
        self.shared.process_told.notify_all();
    }

    /// Whether a control credential bound to `pane` is active: the pane
    /// is one of the window's, or an agent started with `control`.
    pub fn has_control(&self, pane: PaneId) -> bool {
        let state = self.lock();
        state.pane_tokens.get(&pane).is_some_and(|tokens| {
            tokens.iter().any(|token| {
                state
                    .tokens
                    .get(token)
                    .is_some_and(|grant| grant.scope == PermissionScope::Control)
            })
        })
    }

    /// Looks up the grant for `token`, if it is active.
    pub fn lookup(&self, token: &str) -> Option<Grant> {
        self.lock().tokens.get(token).copied()
    }

    /// Checks whether `token` is authorized to execute `request`, for a
    /// caller whose process is not known. Such a caller cannot use the
    /// credential of a pane whose program the table was told of.
    ///
    /// Never includes the token value in returned error messages.
    pub fn authorize(&self, token: Option<&str>, request: &Request) -> Result<(), ApiError> {
        self.authorize_from(token, request, None)
    }

    /// Checks whether `token` is authorized to execute `request` when it
    /// comes from the process `peer`, as the kernel names the client.
    ///
    /// A credential bound to a pane whose program the table was told of
    /// is taken only from that program and the processes it started. It
    /// is refused when that cannot be told: `peer` is not known, or has
    /// ended. A caller could bring that about at will, by handing its
    /// connection to a child and ending, so taking the credential then
    /// would let anyone who read it use it.
    ///
    /// Never includes the token value in returned error messages.
    pub fn authorize_from(
        &self,
        token: Option<&str>,
        request: &Request,
        peer: Option<u32>,
    ) -> Result<(), ApiError> {
        let mut state = self.lock();
        if !state.restricted {
            return Ok(());
        }
        let Some(token) = token.filter(|token| !token.is_empty()) else {
            return Err(ApiError::new(
                ErrorCode::Unauthorized,
                "authentication token is required",
            ));
        };
        let deadline = Instant::now() + START_WAIT;
        let (grant, program) = loop {
            let Some(&grant) = state.tokens.get(token) else {
                return Err(ApiError::new(
                    ErrorCode::Unauthorized,
                    "authentication token is invalid or revoked",
                ));
            };
            let process = grant.pane.and_then(|pane| state.processes.get(&pane));
            match process.copied() {
                None => break (grant, None),
                Some(PaneProcess::Started(program)) => break (grant, Some(program)),
                Some(PaneProcess::Starting) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Err(not_the_panes(
                            "the app has not told which process the pane runs",
                        ));
                    }
                    state = self
                        .shared
                        .process_told
                        .wait_timeout(state, left)
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .0;
                }
            }
        };
        // The kernel is asked without the lock.
        drop(state);
        if let Some(program) = program {
            match peer.map(|peer| crate::process::belongs_to(peer, program)) {
                Some(Some(true)) => {}
                Some(Some(false)) => {
                    return Err(not_the_panes(
                        "the caller is not a process the pane started",
                    ));
                }
                Some(None) | None => {
                    return Err(not_the_panes(
                        "the app could not tell whether the pane started the caller",
                    ));
                }
            }
        }
        match grant.scope {
            PermissionScope::Control => Ok(()),
            PermissionScope::Pane => authorize_pane_request(grant.pane, request),
        }
    }
}

/// The error for a pane's credential that did not come from the pane.
fn not_the_panes(reason: &str) -> ApiError {
    ApiError::new(
        ErrorCode::Unauthorized,
        format!("authentication token belongs to a pane, and {reason}"),
    )
}

fn authorize_pane_request(pane: Option<PaneId>, request: &Request) -> Result<(), ApiError> {
    let Some(authed_pane) = pane else {
        return Err(ApiError::new(
            ErrorCode::Forbidden,
            "pane credential is not bound to a pane",
        ));
    };
    match request {
        Request::ReportHook(req) => {
            if req.pane == authed_pane {
                Ok(())
            } else {
                Err(ApiError::new(
                    ErrorCode::Forbidden,
                    "pane credential cannot report hooks for another pane",
                ))
            }
        }
        Request::ReportChecked(req) => {
            if req.pane == authed_pane {
                Ok(())
            } else {
                Err(ApiError::new(
                    ErrorCode::Forbidden,
                    "pane credential cannot submit checked reports for another pane",
                ))
            }
        }
        Request::ReportToParent(req) => {
            if req.from == authed_pane {
                Ok(())
            } else {
                Err(ApiError::new(
                    ErrorCode::Forbidden,
                    "pane credential cannot report to parent as another pane",
                ))
            }
        }
        // A worker may message its peers. The sender must be its own
        // pane; the app then refuses any route but the peer one for a
        // pane that holds no control credential.
        Request::SendToAgent(req) => {
            if req.caller == Some(authed_pane) {
                Ok(())
            } else {
                Err(ApiError::new(
                    ErrorCode::Forbidden,
                    "pane credential can send to an agent only as its own pane",
                ))
            }
        }
        // And list them.
        Request::ListAgents(req) => {
            if req.parent.is_none() && req.siblings_of == Some(authed_pane) {
                Ok(())
            } else {
                Err(ApiError::new(
                    ErrorCode::Forbidden,
                    "pane credential can list only the peers of its own pane",
                ))
            }
        }
        _ => Err(ApiError::new(
            ErrorCode::Forbidden,
            format!(
                "method {} requires control permission",
                request.method_name()
            ),
        )),
    }
}

/// Generates a 256-bit cryptographically secure random hex token.
fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    fill_random_bytes(&mut bytes);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly",
))]
fn fill_random_bytes(buf: &mut [u8]) {
    // SAFETY: `arc4random_buf` writes `buf.len()` bytes into the valid mutable
    // buffer and cannot fail.
    unsafe {
        libc::arc4random_buf(buf.as_mut_ptr().cast(), buf.len());
    }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly",
)))]
fn fill_random_bytes(buf: &mut [u8]) {
    use std::io::Read;
    let mut file =
        std::fs::File::open("/dev/urandom").expect("failed to open /dev/urandom for token");
    file.read_exact(buf)
        .expect("failed to read random bytes from /dev/urandom");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        CreateWorkspace, KillAgent, ListPanes, ReadOutput, ReportChecked, ReportHook,
        ReportToParent, SendText, SpawnAgent, SplitDirection, SplitPane,
    };

    fn sample_report_hook(pane: u64) -> Request {
        Request::ReportHook(ReportHook {
            pane: PaneId::new(pane),
            agent: "claude".into(),
            event: "Stop".into(),
            tool_name: None,
            notification_type: None,
            session_id: None,
            agent_id: None,
            tool_input: None,
            tool_response: None,
        })
    }

    fn sample_report_checked(pane: u64) -> Request {
        Request::ReportChecked(ReportChecked {
            pane: PaneId::new(pane),
            agent: "claude".into(),
            session_id: None,
            report: benterm_report::Report {
                title: "done".into(),
                goal: String::new(),
                conclusion: "pass".into(),
                summary: "done".into(),
                decisions: Vec::new(),
                verified: Vec::new(),
                unverified: Vec::new(),
                deliverables: Vec::new(),
            },
            problems: Vec::new(),
        })
    }

    fn sample_report_to_parent(from: u64) -> Request {
        Request::ReportToParent(ReportToParent {
            from: PaneId::new(from),
            text: "finished".into(),
            status: None,
        })
    }

    #[test]
    fn unrestricted_table_allows_everything_without_token() {
        let table = PermissionTable::default();
        assert!(!table.is_restricted());
        assert!(
            table
                .authorize(
                    None,
                    &Request::ReadOutput(ReadOutput {
                        pane: PaneId::new(1),
                        lines: None,
                    })
                )
                .is_ok()
        );
        assert!(
            table
                .authorize(Some("not-a-real-token"), &sample_report_hook(1))
                .is_ok()
        );
    }

    #[test]
    fn restricted_mode_rejects_missing_and_invalid_tokens_without_leaking_token() {
        let table = PermissionTable::new(true);
        let req = sample_report_hook(1);

        let err = table.authorize(None, &req).unwrap_err();
        assert_eq!(err.code, ErrorCode::Unauthorized);

        let err = table.authorize(Some(""), &req).unwrap_err();
        assert_eq!(err.code, ErrorCode::Unauthorized);

        let bogus = "secret-bogus-token-value-12345";
        let err = table.authorize(Some(bogus), &req).unwrap_err();
        assert_eq!(err.code, ErrorCode::Unauthorized);
        assert!(!err.message.contains(bogus));
    }

    #[test]
    fn pane_token_allows_only_own_pane_reports() {
        let table = PermissionTable::new(true);
        let token = table.issue_pane_token(PaneId::new(7));
        assert_eq!(token.len(), 64);

        // Allowed for own pane
        assert!(
            table
                .authorize(Some(&token), &sample_report_hook(7))
                .is_ok()
        );
        assert!(
            table
                .authorize(Some(&token), &sample_report_checked(7))
                .is_ok()
        );
        assert!(
            table
                .authorize(Some(&token), &sample_report_to_parent(7))
                .is_ok()
        );

        // Rejected for another pane
        for req in [
            sample_report_hook(8),
            sample_report_checked(8),
            sample_report_to_parent(8),
        ] {
            let err = table.authorize(Some(&token), &req).unwrap_err();
            assert_eq!(err.code, ErrorCode::Forbidden);
            assert!(!err.message.contains(&token));
        }

        // Rejected for control methods
        let control_requests = [
            Request::ReadOutput(ReadOutput {
                pane: PaneId::new(7),
                lines: None,
            }),
            Request::SendText(SendText {
                pane: PaneId::new(7),
                text: "hi".into(),
            }),
            Request::SplitPane(SplitPane {
                pane: PaneId::new(7),
                direction: SplitDirection::Right,
            }),
            Request::ListPanes(ListPanes {}),
            Request::CreateWorkspace(CreateWorkspace::default()),
            Request::SpawnAgent(SpawnAgent {
                name: "worker".into(),
                ..SpawnAgent::default()
            }),
            Request::KillAgent(KillAgent {
                agent: crate::protocol::AgentTarget::Name("worker".into()),
            }),
        ];
        for req in control_requests {
            let err = table.authorize(Some(&token), &req).unwrap_err();
            assert_eq!(err.code, ErrorCode::Forbidden, "{req:?}");
            assert!(!err.message.contains(&token));
        }
    }

    #[test]
    fn pane_token_may_message_and_list_peers_only_as_its_own_pane() {
        use crate::protocol::{AgentTarget, ListAgents, SendToAgent};
        let table = PermissionTable::new(true);
        let token = table.issue_pane_token(PaneId::new(7));
        let send = |caller: Option<u64>| {
            Request::SendToAgent(SendToAgent {
                agent: AgentTarget::Name("peer".into()),
                text: "hi".into(),
                caller: caller.map(PaneId::new),
            })
        };
        assert!(table.authorize(Some(&token), &send(Some(7))).is_ok());
        for req in [send(Some(8)), send(None)] {
            let err = table.authorize(Some(&token), &req).unwrap_err();
            assert_eq!(err.code, ErrorCode::Forbidden);
        }

        let list = |parent: Option<u64>, siblings_of: Option<u64>| {
            Request::ListAgents(ListAgents {
                parent: parent.map(PaneId::new),
                siblings_of: siblings_of.map(PaneId::new),
            })
        };
        assert!(table.authorize(Some(&token), &list(None, Some(7))).is_ok());
        for req in [list(None, Some(8)), list(None, None), list(Some(7), None)] {
            let err = table.authorize(Some(&token), &req).unwrap_err();
            assert_eq!(err.code, ErrorCode::Forbidden);
        }
    }

    #[test]
    fn has_control_tells_panes_with_a_live_control_credential() {
        let table = PermissionTable::new(true);
        table.issue_pane_token(PaneId::new(1));
        table.issue_pane_token(PaneId::new(2));
        table.issue_control_token(Some(PaneId::new(2)));
        table.issue_control_token(None);

        assert!(!table.has_control(PaneId::new(1)));
        assert!(table.has_control(PaneId::new(2)));
        table.revoke_pane(PaneId::new(2));
        assert!(!table.has_control(PaneId::new(2)));
    }

    #[test]
    fn control_token_allows_all_methods() {
        let table = PermissionTable::new(true);
        let token = table.issue_control_token(None);

        let requests = [
            sample_report_hook(1),
            sample_report_checked(2),
            sample_report_to_parent(3),
            Request::ReadOutput(ReadOutput {
                pane: PaneId::new(1),
                lines: None,
            }),
            Request::SendText(SendText {
                pane: PaneId::new(1),
                text: "hi".into(),
            }),
            Request::SpawnAgent(SpawnAgent {
                name: "worker".into(),
                ..SpawnAgent::default()
            }),
            Request::ListPanes(ListPanes {}),
        ];
        for req in requests {
            assert!(table.authorize(Some(&token), &req).is_ok(), "{req:?}");
        }
    }

    #[test]
    fn revoking_pane_invalidates_its_tokens() {
        let table = PermissionTable::new(true);
        let t1 = table.issue_pane_token(PaneId::new(4));
        let t2 = table.issue_pane_token(PaneId::new(5));

        table.revoke_pane(PaneId::new(4));

        let err = table
            .authorize(Some(&t1), &sample_report_hook(4))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Unauthorized);
        assert!(!err.message.contains(&t1));
        assert!(table.authorize(Some(&t2), &sample_report_hook(5)).is_ok());
    }

    #[test]
    fn a_credential_of_a_pane_with_a_program_needs_a_process_of_the_pane() {
        let table = PermissionTable::new(true);
        let pane = PaneId::new(7);
        table.expect_pane_process(pane);
        let token = table.issue_pane_token(pane);
        let own = std::process::id();
        table.bind_pane_process(pane, own);
        let request = sample_report_hook(7);

        assert!(
            table
                .authorize_from(Some(&token), &request, Some(own))
                .is_ok()
        );
        // A caller whose process is not known cannot be told to be the
        // pane's, and neither can this process's parent.
        // SAFETY: getppid has no preconditions.
        let parent = unsafe { libc::getppid() } as u32;
        for peer in [None, Some(parent)] {
            let err = table
                .authorize_from(Some(&token), &request, peer)
                .unwrap_err();
            assert_eq!(err.code, ErrorCode::Unauthorized, "{peer:?}");
            assert!(!err.message.contains(&token));
        }
        assert_eq!(
            table.authorize(Some(&token), &request).unwrap_err().code,
            ErrorCode::Unauthorized
        );
    }

    #[test]
    fn a_pane_whose_process_was_not_told_in_time_is_refused() {
        let table = PermissionTable::new(true);
        let pane = PaneId::new(7);
        table.expect_pane_process(pane);
        let token = table.issue_pane_token(pane);

        let started = Instant::now();
        let err = table
            .authorize_from(
                Some(&token),
                &sample_report_hook(7),
                Some(std::process::id()),
            )
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Unauthorized);
        assert!(started.elapsed() >= START_WAIT);
    }

    #[test]
    fn revoking_a_pane_forgets_its_program() {
        let table = PermissionTable::new(true);
        let pane = PaneId::new(7);
        table.expect_pane_process(pane);
        table.issue_pane_token(pane);
        table.revoke_pane(pane);

        // The pane's number may be given to a pane without a program.
        let token = table.issue_pane_token(pane);
        assert!(
            table
                .authorize(Some(&token), &sample_report_hook(7))
                .is_ok()
        );
    }

    #[test]
    fn debug_redacts_tokens() {
        let table = PermissionTable::new(true);
        let token = table.issue_pane_token(PaneId::new(1));
        let formatted = format!("{table:?}");
        assert!(!formatted.contains(&token));
    }

    #[test]
    fn generated_tokens_are_unique() {
        let table = PermissionTable::new(true);
        let a = table.issue_pane_token(PaneId::new(1));
        let b = table.issue_pane_token(PaneId::new(1));
        assert_ne!(a, b);
    }
}
