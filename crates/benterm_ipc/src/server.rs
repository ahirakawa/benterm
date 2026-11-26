//! The server side of the socket: accepts clients and hands their requests
//! to the app.
//!
//! Each connection has a reader thread and a writer thread. The reader
//! parses requests and calls the app with a [`Reply`] for each. The app may
//! answer at once or keep the reply and answer later from any thread.
//! Answers go through a queue to the writer, so answering never blocks,
//! even when the client has stopped reading. A connection that subscribed
//! gets events through the same queue, from an [`EventSink`] the app keeps.
//! A connection that falls too far behind is disconnected. A request line
//! is limited in length and, once begun, in the time it may take to come
//! (see [`ServerLimits`]); a client that breaks either is told why and cut
//! off, while one that keeps quiet between lines is left alone.

use std::collections::{HashMap, VecDeque};
use std::io::{self, ErrorKind, Read, Write};
use std::marker::PhantomData;
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::auth::PermissionTable;
use crate::protocol::{
    AgentHistory, AgentList, AgentShown, AgentSpawned, AgentsWaited, ApiError, CloseTask,
    CreateTask, CreateWorkspace, ErrorCode, Event, KillAgent, ListAgents, ListPanes, ListTasks,
    OpenBrowser, PaneAgentState, PaneCreated, PaneList, PaneOutput, ReadAgent, ReadAgentHistory,
    ReadAgentState, ReadOutput, ReportAccepted, ReportChecked, ReportHook, ReportToParent, Request,
    RequestFrame, SendKeys, SendText, SendToAgent, ServerMessage, ShowAgent, SpawnAgent, SplitPane,
    SubscribeAgentState, TaskClosed, TaskCreated, TaskList, WaitForAgents, WorkspaceCreated,
};

/// Default maximum number of queued responses, events and outstanding replies
/// per connection.
pub const EVENT_BACKLOG: usize = 1024;

/// How the server reads the lines of its clients.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerLimits {
    /// Maximum number of clients served at once.
    pub max_connections: usize,
    /// How long an idle client can keep its connection without sending.
    pub idle_timeout: Duration,
    /// The most bytes a request line may have, its newline not counted.
    pub max_line: usize,
    /// How long the rest of a line may take to come once its first byte
    /// has come.
    pub partial_line_timeout: Duration,
    /// Time from acceptance to the first authenticated request.
    pub authentication_timeout: Duration,
    /// Maximum number of queued lines and outstanding replies combined.
    pub max_pending_lines: usize,
    /// Maximum bytes of serialized responses and events waiting to be sent.
    pub max_pending_bytes: usize,
    /// Time to write a complete response or event line.
    pub write_timeout: Duration,
}

impl Default for ServerLimits {
    fn default() -> Self {
        Self {
            max_connections: 64,
            idle_timeout: Duration::from_secs(60),
            max_line: 8 << 20,
            partial_line_timeout: Duration::from_secs(10),
            authentication_timeout: Duration::from_secs(5),
            max_pending_lines: EVENT_BACKLOG,
            max_pending_bytes: 16 << 20,
            write_timeout: Duration::from_secs(10),
        }
    }
}

/// The operations of the socket API, carried out by the app.
///
/// The methods are called on the connection's reader thread, one request at
/// a time per connection, so they must return quickly. Anything that takes
/// time, or has to run on another thread, takes the reply with it and
/// answers there.
pub trait App: Send + Sync + 'static {
    fn split_pane(&self, request: SplitPane, reply: Reply<PaneCreated>);
    fn open_browser(&self, request: OpenBrowser, reply: Reply<PaneCreated>);
    fn create_workspace(&self, request: CreateWorkspace, reply: Reply<WorkspaceCreated>);
    fn send_text(&self, request: SendText, reply: Reply<()>);
    fn read_output(&self, request: ReadOutput, reply: Reply<PaneOutput>);
    fn report_hook(&self, request: ReportHook, reply: Reply<()>);
    fn report_checked(&self, request: ReportChecked, reply: Reply<()>);
    fn list_panes(&self, request: ListPanes, reply: Reply<PaneList>);
    fn create_task(&self, request: CreateTask, reply: Reply<TaskCreated>);
    fn list_tasks(&self, request: ListTasks, reply: Reply<TaskList>);
    fn close_task(&self, request: CloseTask, reply: Reply<TaskClosed>);
    fn read_agent_state(&self, request: ReadAgentState, reply: Reply<PaneAgentState>);
    fn read_agent_history(&self, request: ReadAgentHistory, reply: Reply<AgentHistory>);
    /// `events` sends to the connection that asked. To keep the events
    /// behind the answer on the wire, answer first and send afterwards.
    fn subscribe_agent_state(
        &self,
        request: SubscribeAgentState,
        reply: Reply<()>,
        events: EventSink,
    );
    fn spawn_agent(&self, request: SpawnAgent, reply: Reply<AgentSpawned>);
    fn list_agents(&self, request: ListAgents, reply: Reply<AgentList>);
    fn show_agent(&self, request: ShowAgent, reply: Reply<AgentShown>);
    fn send_to_agent(&self, request: SendToAgent, reply: Reply<()>);
    fn read_agent(&self, request: ReadAgent, reply: Reply<PaneOutput>);
    fn send_keys(&self, request: SendKeys, reply: Reply<()>);
    fn kill_agent(&self, request: KillAgent, reply: Reply<()>);
    /// A wait is answered much later than it is asked, if ever, so the app
    /// keeps the reply. `connection` is where it came from: the app is
    /// told with [`hung_up`](Self::hung_up) when nobody is there to read
    /// the answer any more. Returns whether the app kept the reply, and
    /// so wants to be told; false when it has answered already.
    fn wait_for_agents(
        &self,
        request: WaitForAgents,
        reply: Reply<AgentsWaited>,
        connection: ConnectionId,
    ) -> bool;
    fn report_to_parent(&self, request: ReportToParent, reply: Reply<ReportAccepted>);
    /// A connection the app kept a wait of has stopped sending: its client
    /// is gone, or was disconnected. Called once, on the connection's
    /// reader thread, after every wait of the connection was handed over,
    /// also when the waits have been answered by then. The app answers
    /// the waits it still holds for the connection, or drops their
    /// replies; until it does, the connection is not closed.
    fn hung_up(&self, connection: ConnectionId);
}

/// Tells the connections of a server apart, for as long as the server
/// runs: an id is not used again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ConnectionId(u64);

/// Sends events to one connection, from any thread and at any later time.
///
/// Sending never blocks. Once the client is gone, or was disconnected for
/// not reading, the events are discarded and [`send`](Self::send) says so,
/// which is when the app forgets the sink.
///
/// A sink does not keep its connection: once the connection's threads have
/// ended and its requests are answered, the socket is closed, whoever
/// still holds a sink.
#[derive(Clone)]
pub struct EventSink {
    connection: Weak<Connection>,
}

impl EventSink {
    /// Queues `event` for the client. Returns false when the connection is
    /// closed, including when this event exceeded the connection backlog.
    pub fn send<E: Event>(&self, event: &E) -> bool {
        self.connection
            .upgrade()
            .is_some_and(|connection| connection.push(ServerMessage::event(event)))
    }

    /// Whether nothing more will be written to the client.
    pub fn is_closed(&self) -> bool {
        self.connection
            .upgrade()
            .is_none_or(|connection| connection.lock().done_writing())
    }

    /// Whether both sinks send to the same connection, as when a client
    /// subscribes twice.
    pub fn same_connection(&self, other: &Self) -> bool {
        self.connection.ptr_eq(&other.connection)
    }
}

/// The right to answer one request, once.
///
/// It can be sent to another thread and used at any later time. Answering
/// never blocks. If the client is gone by then, the answer is discarded. A
/// reply that is dropped unanswered sends an `internal` error, so a client
/// never waits forever for a request the app lost.
pub struct Reply<T> {
    target: Option<(Arc<Connection>, u64)>,
    /// When the time the request gave the app runs out.
    deadline: Option<Instant>,
    // `fn(T)` keeps the reply `Send` whatever `T` is: it holds no `T`.
    answer: PhantomData<fn(T)>,
}

impl<T> Reply<T> {
    /// Whether the time the request gave the app has run out. The app does
    /// not start a request whose time has run out; it answers it with
    /// [`Reply::expire`]. The caller has given up on it, or is about to,
    /// and may send it again.
    pub fn expired(&self) -> bool {
        self.deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
    }
}

impl<T: Serialize> Reply<T> {
    /// Answers that the request was not carried out because its time ran
    /// out.
    pub fn expire(self) {
        self.err(ApiError::new(
            ErrorCode::Expired,
            "the app got to the request after its time had run out and did not carry it out",
        ));
    }

    pub fn send(mut self, result: Result<T, ApiError>) {
        if let Some((connection, id)) = self.target.take() {
            let message = match result {
                Ok(result) => ServerMessage::result(id, &result),
                Err(error) => ServerMessage::error(Some(id), error),
            };
            connection.finish_reply(message);
        }
    }

    pub fn ok(self, result: T) {
        self.send(Ok(result));
    }

    pub fn err(self, error: ApiError) {
        self.send(Err(error));
    }
}

impl<T> Drop for Reply<T> {
    fn drop(&mut self) {
        if let Some((connection, id)) = self.target.take() {
            let error = ApiError::new(ErrorCode::Internal, "the request was dropped unanswered");
            connection.finish_reply(ServerMessage::error(Some(id), error));
        }
    }
}

/// What the reader, the writer and the replies of one connection share.
struct Connection {
    state: Mutex<Outbox>,
    changed: Condvar,
    /// The socket, to shut it down under a writer that is stuck in a
    /// write to a client that does not read.
    socket: UnixStream,
    id: ConnectionId,
    /// Whether the app has kept a wait of the connection, which it is
    /// told the end of the connection for. Only the reader thread sets
    /// it.
    waited: AtomicBool,
    limits: ServerLimits,
    permissions: PermissionTable,
    /// The client runs in the server's own process, which the table
    /// trusts: its requests need no credential.
    trusted: bool,
    /// The client's process, which a pane's credential is checked against.
    peer: Option<u32>,
}

#[derive(Default)]
struct Outbox {
    /// A client can wait indefinitely for events after subscribing.
    subscribed: bool,
    /// Lines waiting to be written to the client.
    lines: VecDeque<String>,
    queued_bytes: usize,
    /// Requests handed to the app and not answered yet.
    open_replies: usize,
    /// The reader has seen the end of the client's requests.
    reader_done: bool,
    /// Nothing more will be written: the client is gone or the server is
    /// shutting down.
    closed: bool,
    /// The reader cut the client off: the lines already queued, the last
    /// of them the error that says why, are written and nothing more.
    ending: bool,
}

impl Outbox {
    /// Whether no more lines are taken for the client.
    fn done_writing(&self) -> bool {
        self.closed || self.ending
    }
}

impl Connection {
    fn lock(&self) -> MutexGuard<'_, Outbox> {
        // The lock is only held for a few field updates, none of which can
        // panic half way, so the state is sound even after a poisoning.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn enqueue(
        &self,
        mut outbox: MutexGuard<'_, Outbox>,
        message: ServerMessage,
        ending: bool,
    ) -> bool {
        if outbox.done_writing() {
            return false;
        }
        let line = self
            .has_slot(&outbox)
            .then(|| message.to_line())
            .filter(|line| {
                line.len()
                    <= self
                        .limits
                        .max_pending_bytes
                        .saturating_sub(outbox.queued_bytes)
            });
        if let Some(line) = line {
            outbox.queued_bytes += line.len();
            outbox.lines.push_back(line);
            outbox.ending = ending;
            drop(outbox);
            self.changed.notify_all();
            true
        } else {
            self.disconnect(outbox);
            false
        }
    }

    fn has_slot(&self, outbox: &Outbox) -> bool {
        outbox.lines.len() + outbox.open_replies < self.limits.max_pending_lines
    }

    fn disconnect(&self, mut outbox: MutexGuard<'_, Outbox>) {
        outbox.closed = true;
        outbox.lines.clear();
        outbox.queued_bytes = 0;
        drop(outbox);
        self.changed.notify_all();
        let _ = self.socket.shutdown(Shutdown::Both);
    }

    fn push(&self, message: ServerMessage) -> bool {
        self.enqueue(self.lock(), message, false)
    }

    fn reserve_reply(&self) -> bool {
        let mut outbox = self.lock();
        if outbox.done_writing() {
            return false;
        }
        if !self.has_slot(&outbox) {
            self.disconnect(outbox);
            return false;
        }
        outbox.open_replies += 1;
        true
    }

    fn open_reply<T>(self: &Arc<Self>, id: u64, deadline: Option<Instant>) -> Reply<T> {
        Reply {
            target: Some((self.clone(), id)),
            deadline,
            answer: PhantomData,
        }
    }

    fn finish_reply(&self, message: ServerMessage) {
        let mut outbox = self.lock();
        outbox.open_replies -= 1;
        self.enqueue(outbox, message, false);
    }

    /// Queues the last line when there is room, then drains the queue.
    fn cut(&self, message: ServerMessage) {
        self.enqueue(self.lock(), message, true);
    }

    fn reader_done(&self) {
        self.lock().reader_done = true;
        self.changed.notify_all();
    }

    fn close(&self) {
        let mut outbox = self.lock();
        outbox.closed = true;
        outbox.lines.clear();
        outbox.queued_bytes = 0;
        drop(outbox);
        self.changed.notify_all();
    }

    /// The next line to write. `None` when there will never be one: the
    /// connection is closed, or the client has stopped sending and every
    /// request it sent is answered.
    fn next_line(&self) -> Option<String> {
        let mut outbox = self.lock();
        loop {
            if outbox.closed {
                return None;
            }
            if let Some(line) = outbox.lines.pop_front() {
                outbox.queued_bytes -= line.len();
                return Some(line);
            }
            if outbox.ending || (outbox.reader_done && outbox.open_replies == 0) {
                return None;
            }
            outbox = self
                .changed
                .wait(outbox)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
}

/// The connections that are still open, so shutting down can close them.
type Connections = Arc<Mutex<HashMap<u64, (Arc<Connection>, UnixStream)>>>;

/// A listening socket and the threads that serve it. Dropping it shuts the
/// server down.
pub struct Server {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    /// Writing a byte here wakes the accept loop.
    wake: UnixStream,
    accept: Option<JoinHandle<Vec<JoinHandle<()>>>>,
    connections: Connections,
    permissions: PermissionTable,
}

impl Server {
    /// Listens on a socket at `path` that only the owner can use.
    ///
    /// The directory is created for the owner only when it is missing. An
    /// existing directory must belong to the user and be closed to everyone
    /// else. A socket file left behind by a server that is gone is replaced.
    pub fn bind(path: impl AsRef<Path>, app: Arc<dyn App>) -> io::Result<Self> {
        Self::bind_with(path, app, ServerLimits::default())
    }

    /// Like [`Server::bind`], reading the clients' lines with `limits`.
    pub fn bind_with(
        path: impl AsRef<Path>,
        app: Arc<dyn App>,
        limits: ServerLimits,
    ) -> io::Result<Self> {
        Self::bind_with_permissions(path, app, limits, PermissionTable::default())
    }

    /// Like [`Server::bind_with`], checking every request against `permissions`.
    pub fn bind_with_permissions(
        path: impl AsRef<Path>,
        app: Arc<dyn App>,
        limits: ServerLimits,
        permissions: PermissionTable,
    ) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        prepare_directory(&path)?;
        remove_dead_socket(&path)?;

        let listener = UnixListener::bind(&path)?;
        // The directory is already private, so nobody can reach the socket
        // between the bind and this change.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;

        let (wake, woken) = UnixStream::pair()?;
        let stop = Arc::new(AtomicBool::new(false));
        let connections = Connections::default();
        let accept = {
            let stop = stop.clone();
            let connections = connections.clone();
            let permissions = permissions.clone();
            thread::Builder::new()
                .name("benterm-ipc-accept".into())
                .spawn(move || {
                    accept_loop(listener, woken, stop, connections, app, limits, permissions)
                })?
        };

        Ok(Self {
            path,
            stop,
            wake,
            accept: Some(accept),
            connections,
            permissions,
        })
    }

    /// The path of the socket.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn permissions(&self) -> &PermissionTable {
        &self.permissions
    }

    /// The number of connections whose threads are still running.
    pub fn connection_count(&self) -> usize {
        lock_connections(&self.connections).len()
    }

    /// Stops accepting, closes every connection, waits for the threads and
    /// removes the socket. Replies the app still holds stay valid; their
    /// answers are discarded.
    pub fn shutdown(mut self) {
        self.stop_serving();
    }

    fn stop_serving(&mut self) {
        let Some(accept) = self.accept.take() else {
            return;
        };
        self.stop.store(true, Ordering::SeqCst);
        // A full pipe means a wake-up is already pending.
        let _ = (&self.wake).write(&[0]);
        let readers = accept.join().unwrap_or_default();

        // The accept loop has ended, so no connection is added from here on.
        for (connection, stream) in lock_connections(&self.connections).values() {
            connection.close();
            let _ = stream.shutdown(Shutdown::Both);
        }
        for reader in readers {
            let _ = reader.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop_serving();
    }
}

fn lock_connections(
    connections: &Connections,
) -> MutexGuard<'_, HashMap<u64, (Arc<Connection>, UnixStream)>> {
    connections
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn prepare_directory(path: &Path) -> io::Result<()> {
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidInput, "the socket needs a directory"))?;
    if !directory.exists() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)?;
    }

    let metadata = std::fs::metadata(directory)?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let user = unsafe { libc::geteuid() };
    if !metadata.is_dir() || metadata.uid() != user || metadata.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            format!(
                "{} must be a directory that only its owner, the current user, can use",
                directory.display()
            ),
        ));
    }
    Ok(())
}

/// Removes a socket file nobody listens on. Leaves everything else alone.
fn remove_dead_socket(path: &Path) -> io::Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            ErrorKind::AlreadyExists,
            format!("{} exists and is not a socket", path.display()),
        ));
    }
    match UnixStream::connect(path) {
        Ok(_) => Err(io::Error::new(
            ErrorKind::AddrInUse,
            format!("another server is listening on {}", path.display()),
        )),
        Err(error) if error.kind() == ErrorKind::ConnectionRefused => std::fs::remove_file(path),
        Err(error) => Err(error),
    }
}

/// Accepts clients until told to stop. Returns the reader threads that may
/// still be running.
fn accept_loop(
    listener: UnixListener,
    woken: UnixStream,
    stop: Arc<AtomicBool>,
    connections: Connections,
    app: Arc<dyn App>,
    limits: ServerLimits,
    permissions: PermissionTable,
) -> Vec<JoinHandle<()>> {
    let mut readers: Vec<JoinHandle<()>> = Vec::new();
    let mut next_key = 0u64;

    while wait_for_client(&listener, &woken) && !stop.load(Ordering::SeqCst) {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            // The client gave up before we got to it: nothing to accept.
            Err(error) if error.kind() == ErrorKind::WouldBlock => continue,
            // Most likely out of file descriptors. The client stays in the
            // queue, so poll returns at once; pause instead of spinning.
            Err(_) => {
                thread::sleep(Duration::from_millis(50));
                continue;
            }
        };
        readers.retain(|reader| !reader.is_finished());
        if lock_connections(&connections).len() >= limits.max_connections {
            continue;
        }

        let key = next_key;
        next_key += 1;
        if let Ok(reader) = start_connection(
            key,
            stream,
            &connections,
            app.clone(),
            limits,
            permissions.clone(),
        ) {
            readers.push(reader);
        }
    }
    readers
}

/// The process at the other end of `stream`, as the kernel tells it.
#[cfg(target_vendor = "apple")]
fn peer_pid(stream: &UnixStream) -> Option<u32> {
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    // SAFETY: `pid` and `len` are valid for writes of the sizes given, and
    // the descriptor is open because we borrow its owner.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&raw mut pid).cast(),
            &raw mut len,
        )
    };
    (result == 0).then_some(pid as u32)
}

/// The process at the other end of `stream`, as the kernel tells it.
#[cfg(target_os = "linux")]
fn peer_pid(stream: &UnixStream) -> Option<u32> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: as above, for a `ucred`.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &raw mut len,
        )
    };
    (result == 0).then_some(cred.pid as u32)
}

#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
fn peer_pid(_stream: &UnixStream) -> Option<u32> {
    None
}

/// Blocks until a client is waiting or the wake socket has a byte. Returns
/// false when polling itself fails for good.
fn wait_for_client(listener: &UnixListener, woken: &UnixStream) -> bool {
    let mut fds = [
        libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: woken.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        // SAFETY: `fds` is a valid array of two pollfd for the whole call,
        // and both descriptors are open because we borrow their owners.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if ready >= 0 {
            return true;
        }
        if io::Error::last_os_error().kind() != ErrorKind::Interrupted {
            return false;
        }
    }
}

fn start_connection(
    key: u64,
    stream: UnixStream,
    connections: &Connections,
    app: Arc<dyn App>,
    limits: ServerLimits,
    permissions: PermissionTable,
) -> io::Result<JoinHandle<()>> {
    // On macOS an accepted socket inherits the listener's non-blocking mode.
    stream.set_nonblocking(false)?;
    let accepted = Instant::now();
    let peer = peer_pid(&stream);
    let trusted = permissions.trusts_own_process() && peer == Some(std::process::id());
    let connection = Arc::new(Connection {
        state: Mutex::new(Outbox::default()),
        changed: Condvar::new(),
        socket: stream.try_clone()?,
        id: ConnectionId(key),
        waited: AtomicBool::new(false),
        limits,
        permissions,
        trusted,
        peer,
    });
    let for_table = stream.try_clone()?;
    let for_writer = stream.try_clone()?;
    lock_connections(connections).insert(key, (connection.clone(), for_table));

    let connections = connections.clone();
    let spawned = thread::Builder::new()
        .name("benterm-ipc-reader".into())
        .spawn({
            let connections = connections.clone();
            move || {
                let writer = {
                    let connection = connection.clone();
                    thread::Builder::new()
                        .name("benterm-ipc-writer".into())
                        .spawn(move || write_loop(&connection, for_writer))
                };
                if writer.is_ok() {
                    read_loop(&connection, stream, app.as_ref(), accepted);
                }
                // The waits the app holds for this connection would keep
                // the writer and the connection alive until they finish.
                if connection.waited.load(Ordering::Acquire) {
                    app.hung_up(connection.id);
                }
                connection.reader_done();
                match writer {
                    Ok(writer) => {
                        let _ = writer.join();
                    }
                    Err(_) => connection.close(),
                }
                lock_connections(&connections).remove(&key);
            }
        });
    if spawned.is_err() {
        lock_connections(&connections).remove(&key);
    }
    spawned
}

/// Reads requests until the client stops sending, and hands each to the app.
///
/// A line may be at most `max_line` bytes long, and once its first byte
/// has come, the rest has `partial_line_timeout` to follow. A client that
/// breaks either is sent an error and cut off. Idle clients time out
/// between lines unless they are watching events or awaiting an answer.
fn read_loop(
    connection: &Arc<Connection>,
    mut stream: UnixStream,
    app: &dyn App,
    accepted: Instant,
) {
    let limits = connection.limits;
    // What has come and is not yet a whole line, apart from the lines
    // taken out of the bytes of the last read.
    let mut pending = Vec::new();
    let mut chunk = [0u8; 8192];
    // When the first byte of the line in `pending` came.
    let mut started: Option<Instant> = None;
    let idle_timeout = limits.idle_timeout.max(Duration::from_millis(1));
    let mut idle_since = accepted;
    let mut authenticated = connection.trusted || !connection.permissions.is_restricted();
    loop {
        let mut awaiting_reply = false;
        let timeout = match started {
            None => {
                let outbox = connection.lock();
                if outbox.subscribed {
                    None
                } else if outbox.open_replies > 0 || !outbox.lines.is_empty() {
                    awaiting_reply = true;
                    Some(idle_timeout)
                } else {
                    let left = idle_timeout.saturating_sub(idle_since.elapsed());
                    if left.is_zero() {
                        return;
                    }
                    Some(left)
                }
            }
            Some(started) => {
                let left = (started + limits.partial_line_timeout)
                    .saturating_duration_since(Instant::now());
                if left.is_zero() {
                    connection.cut(cut_off(format!(
                        "the rest of the request line did not come within {}s",
                        limits.partial_line_timeout.as_secs_f64()
                    )));
                    return;
                }
                Some(left)
            }
        };
        let timeout = if authenticated {
            timeout
        } else {
            let left = limits
                .authentication_timeout
                .saturating_sub(accepted.elapsed());
            if left.is_zero() {
                return;
            }
            Some(timeout.map_or(left, |timeout| timeout.min(left)))
        };
        // macOS refuses a timeout on a socket that was shut down, which
        // is the end of the connection anyway.
        if stream.set_read_timeout(timeout).is_err() {
            return;
        }
        let read = match stream.read(&mut chunk) {
            // End of file, or the connection was closed under us. A line
            // without its newline does not count: the client stopped in
            // the middle of writing it.
            Ok(0) => return,
            Ok(read) => read,
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::Interrupted | ErrorKind::WouldBlock | ErrorKind::TimedOut
                ) =>
            {
                if awaiting_reply {
                    idle_since = Instant::now();
                }
                continue;
            }
            Err(_) => return,
        };
        let now = Instant::now();
        let mut rest = &chunk[..read];
        while let Some(newline) = rest.iter().position(|&byte| byte == b'\n') {
            if pending.len() + newline > limits.max_line {
                connection.cut(too_long(limits.max_line));
                return;
            }
            pending.extend_from_slice(&rest[..newline]);
            rest = &rest[newline + 1..];
            if !pending.iter().all(u8::is_ascii_whitespace) {
                idle_since = now;
                if (!authenticated && accepted.elapsed() >= limits.authentication_timeout)
                    || !handle_line(connection, app, &pending, &mut authenticated)
                {
                    return;
                }
            }
            pending.clear();
        }
        if pending.len() + rest.len() > limits.max_line {
            connection.cut(too_long(limits.max_line));
            return;
        }
        if pending.is_empty() {
            started = (!rest.is_empty()).then_some(now);
        }
        pending.extend_from_slice(rest);
    }
}

/// The error a client is cut off with: it has no id, since its line was
/// not read.
fn cut_off(reason: String) -> ServerMessage {
    ServerMessage::error(
        None,
        ApiError::new(
            ErrorCode::InvalidRequest,
            format!("{reason}; the app closed the connection"),
        ),
    )
}

fn too_long(max_line: usize) -> ServerMessage {
    cut_off(format!(
        "a request line may be at most {max_line} bytes long"
    ))
}

/// Hands one nonblank line to the app. Returns false when the connection is cut.
fn handle_line(
    connection: &Arc<Connection>,
    app: &dyn App,
    line: &[u8],
    authenticated: &mut bool,
) -> bool {
    let frame = std::str::from_utf8(line)
        .map_err(|_| crate::protocol::RequestError {
            id: None,
            error: ApiError::new(ErrorCode::InvalidRequest, "a request must be UTF-8"),
        })
        .and_then(RequestFrame::parse);
    match frame {
        Ok(RequestFrame {
            id,
            request,
            timeout,
            token,
        }) => {
            if !connection.trusted
                && let Err(error) = connection.permissions.authorize_from(
                    token.as_deref(),
                    &request,
                    connection.peer,
                )
            {
                let unauthorized = error.code == ErrorCode::Unauthorized;
                let message = ServerMessage::error(Some(id), error);
                if unauthorized {
                    connection.cut(message);
                    return false;
                }
                // A forbidden request had a valid credential.
                *authenticated = true;
                return connection.push(message);
            }
            *authenticated = true;
            if !connection.reserve_reply() {
                return false;
            }
            // The time is counted from here: the caller started
            // counting earlier and gives the app less than it waits.
            let deadline = timeout.map(|timeout| Instant::now() + timeout);
            match request {
                Request::SplitPane(request) => {
                    app.split_pane(request, connection.open_reply(id, deadline));
                }
                Request::OpenBrowser(request) => {
                    app.open_browser(request, connection.open_reply(id, deadline));
                }
                Request::CreateWorkspace(request) => {
                    app.create_workspace(request, connection.open_reply(id, deadline));
                }
                Request::SendText(request) => {
                    app.send_text(request, connection.open_reply(id, deadline));
                }
                Request::ReadOutput(request) => {
                    app.read_output(request, connection.open_reply(id, deadline));
                }
                Request::ReportHook(request) => {
                    app.report_hook(request, connection.open_reply(id, deadline));
                }
                Request::ReportChecked(request) => {
                    app.report_checked(request, connection.open_reply(id, deadline));
                }
                Request::ReadAgentState(request) => {
                    app.read_agent_state(request, connection.open_reply(id, deadline));
                }
                Request::ReadAgentHistory(request) => {
                    app.read_agent_history(request, connection.open_reply(id, deadline));
                }
                Request::SubscribeAgentState(request) => {
                    connection.lock().subscribed = true;
                    let events = EventSink {
                        connection: Arc::downgrade(connection),
                    };
                    app.subscribe_agent_state(request, connection.open_reply(id, deadline), events);
                }
                Request::SpawnAgent(request) => {
                    app.spawn_agent(request, connection.open_reply(id, deadline));
                }
                Request::ListAgents(request) => {
                    app.list_agents(request, connection.open_reply(id, deadline));
                }
                Request::ShowAgent(request) => {
                    app.show_agent(request, connection.open_reply(id, deadline));
                }
                Request::SendToAgent(request) => {
                    app.send_to_agent(request, connection.open_reply(id, deadline));
                }
                Request::ReadAgent(request) => {
                    app.read_agent(request, connection.open_reply(id, deadline));
                }
                Request::SendKeys(request) => {
                    app.send_keys(request, connection.open_reply(id, deadline));
                }
                Request::KillAgent(request) => {
                    app.kill_agent(request, connection.open_reply(id, deadline));
                }
                Request::WaitForAgents(request) => {
                    let reply = connection.open_reply(id, deadline);
                    if app.wait_for_agents(request, reply, connection.id) {
                        connection.waited.store(true, Ordering::Release);
                    }
                }
                Request::ReportToParent(request) => {
                    app.report_to_parent(request, connection.open_reply(id, deadline));
                }
                Request::ListPanes(request) => {
                    app.list_panes(request, connection.open_reply(id, deadline));
                }
                Request::CreateTask(request) => {
                    app.create_task(request, connection.open_reply(id, deadline));
                }
                Request::ListTasks(request) => {
                    app.list_tasks(request, connection.open_reply(id, deadline));
                }
                Request::CloseTask(request) => {
                    app.close_task(request, connection.open_reply(id, deadline));
                }
            }
        }
        Err(error) => {
            let message = ServerMessage::error(error.id, error.error);
            if !*authenticated {
                connection.cut(message);
                return false;
            }
            return connection.push(message);
        }
    }
    true
}

/// Writes queued lines until there will be no more or the client is gone.
fn write_loop(connection: &Connection, mut stream: UnixStream) {
    while let Some(line) = connection.next_line() {
        if write_line(
            &mut stream,
            line.as_bytes(),
            connection.limits.write_timeout,
        )
        .is_err()
        {
            break;
        }
    }
    connection.close();
    // Ends the reader too, if it is still waiting for requests, and tells a
    // client that only stopped writing that every answer has been sent.
    let _ = stream.shutdown(Shutdown::Both);
}

fn write_line(stream: &mut UnixStream, mut bytes: &[u8], timeout: Duration) -> io::Result<()> {
    let started = Instant::now();
    while !bytes.is_empty() {
        let left = timeout.saturating_sub(started.elapsed());
        if left.is_zero() {
            return Err(ErrorKind::TimedOut.into());
        }
        stream.set_write_timeout(Some(left))?;
        match stream.write(bytes) {
            Ok(0) => return Err(ErrorKind::WriteZero.into()),
            Ok(written) => bytes = &bytes[written..],
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "server_limits_tests.rs"]
mod limits_tests;

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};

    use serde_json::{Value, json};

    use super::*;
    use crate::protocol::PaneId;

    #[derive(Default)]
    pub(super) struct CountingApp {
        calls: Mutex<Vec<&'static str>>,
    }

    impl CountingApp {
        fn record(&self, name: &'static str) {
            self.calls.lock().unwrap().push(name);
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl App for CountingApp {
        fn split_pane(&self, _: SplitPane, reply: Reply<PaneCreated>) {
            self.record("pane.split");
            reply.ok(PaneCreated {
                pane: PaneId::new(1),
            });
        }
        fn open_browser(&self, _: OpenBrowser, reply: Reply<PaneCreated>) {
            self.record("browser.open");
            reply.ok(PaneCreated {
                pane: PaneId::new(1),
            });
        }
        fn create_workspace(&self, _: CreateWorkspace, reply: Reply<WorkspaceCreated>) {
            self.record("workspace.create");
            reply.ok(WorkspaceCreated {
                workspace: crate::protocol::WorkspaceId::new(1),
                pane: PaneId::new(1),
            });
        }
        fn send_text(&self, _: SendText, reply: Reply<()>) {
            self.record("pane.send_text");
            reply.ok(());
        }
        fn read_output(&self, _: ReadOutput, reply: Reply<PaneOutput>) {
            self.record("pane.read");
            reply.ok(PaneOutput { text: "ok".into() });
        }
        fn report_hook(&self, _: ReportHook, reply: Reply<()>) {
            self.record("pane.report_hook");
            reply.ok(());
        }
        fn report_checked(&self, _: ReportChecked, reply: Reply<()>) {
            self.record("pane.report_checked");
            reply.ok(());
        }
        fn list_panes(&self, _: ListPanes, reply: Reply<PaneList>) {
            self.record("pane.list");
            reply.ok(PaneList::default());
        }
        fn create_task(&self, _: CreateTask, reply: Reply<TaskCreated>) {
            self.record("task.create");
            reply.err(ApiError::new(ErrorCode::Failed, "unused"));
        }
        fn list_tasks(&self, _: ListTasks, reply: Reply<TaskList>) {
            self.record("task.list");
            reply.ok(TaskList { tasks: Vec::new() });
        }
        fn close_task(&self, _: CloseTask, reply: Reply<TaskClosed>) {
            self.record("task.close");
            reply.err(ApiError::new(ErrorCode::Failed, "unused"));
        }
        fn read_agent_state(&self, req: ReadAgentState, reply: Reply<PaneAgentState>) {
            self.record("pane.agent_state");
            reply.ok(PaneAgentState {
                pane: req.pane,
                agent: None,
                state: crate::protocol::AgentActivity::Unknown,
                reason: None,
            });
        }
        fn read_agent_history(&self, _: ReadAgentHistory, reply: Reply<AgentHistory>) {
            self.record("agent.history");
            reply.ok(AgentHistory::default());
        }
        fn subscribe_agent_state(&self, _: SubscribeAgentState, reply: Reply<()>, _: EventSink) {
            self.record("pane.subscribe_agent_state");
            reply.ok(());
        }
        fn spawn_agent(&self, req: SpawnAgent, reply: Reply<AgentSpawned>) {
            self.record("agent.spawn");
            reply.ok(AgentSpawned {
                name: req.name,
                pane: PaneId::new(2),
            });
        }
        fn list_agents(&self, _: ListAgents, reply: Reply<AgentList>) {
            self.record("agent.list");
            reply.ok(AgentList { agents: Vec::new() });
        }
        fn show_agent(&self, _: ShowAgent, reply: Reply<AgentShown>) {
            self.record("agent.show");
            reply.ok(AgentShown {
                pane: PaneId::new(1),
                report_status: None,
                stalled: false,
                changed_at_ms: None,
            });
        }
        fn send_to_agent(&self, _: SendToAgent, reply: Reply<()>) {
            self.record("agent.send");
            reply.ok(());
        }
        fn read_agent(&self, _: ReadAgent, reply: Reply<PaneOutput>) {
            self.record("agent.read");
            reply.ok(PaneOutput { text: "ok".into() });
        }
        fn send_keys(&self, _: SendKeys, reply: Reply<()>) {
            self.record("agent.keys");
            reply.ok(());
        }
        fn kill_agent(&self, _: KillAgent, reply: Reply<()>) {
            self.record("agent.kill");
            reply.ok(());
        }
        fn wait_for_agents(
            &self,
            _: WaitForAgents,
            reply: Reply<AgentsWaited>,
            _: ConnectionId,
        ) -> bool {
            self.record("agent.wait");
            reply.ok(AgentsWaited { agents: Vec::new() });
            false
        }
        fn report_to_parent(&self, _: ReportToParent, reply: Reply<ReportAccepted>) {
            self.record("agent.report");
            reply.ok(ReportAccepted {
                parent: PaneId::new(1),
            });
        }
        fn hung_up(&self, _: ConnectionId) {}
    }

    fn call_line(
        writer: &mut UnixStream,
        reader: &mut BufReader<UnixStream>,
        payload: Value,
    ) -> Value {
        let mut line = payload.to_string();
        line.push('\n');
        writer.write_all(line.as_bytes()).unwrap();
        let mut response = String::new();
        reader.read_line(&mut response).unwrap();
        serde_json::from_str(&response).unwrap()
    }

    #[test]
    fn a_client_in_the_own_process_needs_no_credential_once_trusted() {
        let app = Arc::new(CountingApp::default());
        let permissions = PermissionTable::new(true);
        let connections = Connections::default();
        let open = |key| {
            let (server_stream, client) = UnixStream::pair().unwrap();
            start_connection(
                key,
                server_stream,
                &connections,
                app.clone(),
                ServerLimits::default(),
                permissions.clone(),
            )
            .unwrap();
            let read = BufReader::new(client.try_clone().unwrap());
            (client, read)
        };
        let list = json!({"id": 1, "method": "pane.list", "params": {}});

        let (mut write, mut read) = open(1);
        let resp = call_line(&mut write, &mut read, list.clone());
        assert_eq!(resp["error"]["code"], "unauthorized");

        permissions.set_trusts_own_process(true);
        let (mut write, mut read) = open(2);
        let resp = call_line(&mut write, &mut read, list);
        assert!(resp.get("error").is_none(), "{resp}");
    }

    #[test]
    fn a_client_in_another_process_is_not_trusted() {
        if !std::path::Path::new("/usr/bin/nc").exists() {
            return;
        }
        let dir = tempfile::Builder::new()
            .permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .tempdir()
            .unwrap();
        let path = dir.path().join("s.sock");
        let permissions = PermissionTable::new(true);
        permissions.set_trusts_own_process(true);
        let server = Server::bind_with_permissions(
            &path,
            Arc::new(CountingApp::default()),
            ServerLimits::default(),
            permissions,
        )
        .unwrap();
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(r#"printf '{"id":1,"method":"pane.list","params":{}}\n' | /usr/bin/nc -U -w 2 "$0""#)
            .arg(&path)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains("unauthorized"), "{text}");
        drop(server);
    }

    #[test]
    fn authentication_failure_sends_an_error_then_closes_the_connection() {
        let permissions = PermissionTable::new(true);
        let connections = Connections::default();

        for (key, token) in [(1, None), (2, Some("invalid-test-token"))] {
            let (server_stream, mut client) = UnixStream::pair().unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut read = BufReader::new(client.try_clone().unwrap());
            let reader = start_connection(
                key,
                server_stream,
                &connections,
                Arc::new(CountingApp::default()),
                ServerLimits::default(),
                permissions.clone(),
            )
            .unwrap();

            let mut request = json!({"id": key, "method": "pane.list", "params": {}});
            if let Some(token) = token {
                request["token"] = json!(token);
            }
            let response = call_line(&mut client, &mut read, request);
            assert_eq!(response["id"], key);
            assert_eq!(response["error"]["code"], "unauthorized");
            if let Some(token) = token {
                assert!(
                    !response["error"]["message"]
                        .as_str()
                        .unwrap()
                        .contains(token)
                );
            }
            assert_eq!(read.read_line(&mut String::new()).unwrap(), 0);
            reader.join().unwrap();
        }
    }

    #[test]
    fn restricted_connection_enforces_scope_mismatch_unknown_and_revocation() {
        let (server_stream, mut client_write) = UnixStream::pair().unwrap();
        let mut client_read = BufReader::new(client_write.try_clone().unwrap());

        let app = Arc::new(CountingApp::default());
        let permissions = PermissionTable::new(true);
        let pane_token = permissions.issue_pane_token(PaneId::new(10));
        let control_token = permissions.issue_control_token(None);
        let connections = Connections::default();
        let reader = start_connection(
            1,
            server_stream,
            &connections,
            app.clone(),
            ServerLimits::default(),
            permissions.clone(),
        )
        .unwrap();

        // Pane token on own pane hook -> allowed
        let resp = call_line(
            &mut client_write,
            &mut client_read,
            json!({
                "id": 3,
                "method": "pane.report_hook",
                "params": {"pane": 10, "agent": "claude", "event": "Stop"},
                "token": pane_token,
            }),
        );
        assert!(resp.get("result").is_some(), "{resp}");

        // Pane token on other pane -> forbidden, token not leaked
        let resp = call_line(
            &mut client_write,
            &mut client_read,
            json!({
                "id": 4,
                "method": "pane.report_hook",
                "params": {"pane": 11, "agent": "claude", "event": "Stop"},
                "token": pane_token,
            }),
        );
        assert_eq!(resp["error"]["code"], "forbidden");
        assert!(
            !resp["error"]["message"]
                .as_str()
                .unwrap()
                .contains(&pane_token)
        );

        // Pane token on control method -> forbidden
        let resp = call_line(
            &mut client_write,
            &mut client_read,
            json!({
                "id": 5,
                "method": "pane.read",
                "params": {"pane": 10},
                "token": pane_token,
            }),
        );
        assert_eq!(resp["error"]["code"], "forbidden");

        // Unknown method -> unknown_method
        let resp = call_line(
            &mut client_write,
            &mut client_read,
            json!({"id": 6, "method": "pane.explode", "token": pane_token}),
        );
        assert_eq!(resp["error"]["code"], "unknown_method");

        // Control token on control method -> allowed
        let resp = call_line(
            &mut client_write,
            &mut client_read,
            json!({
                "id": 7,
                "method": "pane.read",
                "params": {"pane": 10},
                "token": control_token,
            }),
        );
        assert_eq!(resp["result"]["text"], "ok");

        // Revoke pane 10 -> next request on SAME open connection is rejected
        permissions.revoke_pane(PaneId::new(10));
        let resp = call_line(
            &mut client_write,
            &mut client_read,
            json!({
                "id": 8,
                "method": "pane.report_hook",
                "params": {"pane": 10, "agent": "claude", "event": "Stop"},
                "token": pane_token,
            }),
        );
        assert_eq!(resp["error"]["code"], "unauthorized");
        assert!(
            !resp["error"]["message"]
                .as_str()
                .unwrap()
                .contains(&pane_token)
        );

        assert_eq!(app.calls(), vec!["pane.report_hook", "pane.read"]);

        drop(client_write);
        drop(client_read);
        reader.join().unwrap();
    }
}
