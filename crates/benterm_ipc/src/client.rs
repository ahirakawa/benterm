//! A blocking client for the socket API.

use std::collections::VecDeque;
use std::fmt;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::protocol::{ApiError, Method, ServerMessage, request_line};
use crate::socket::{CONTROL_TOKEN_ENV, PANE_TOKEN_ENV};

/// Why a call did not return an answer.
#[derive(Debug)]
pub enum ClientError {
    /// The socket could not be written or read.
    Io(io::Error),
    /// The server closed the connection before answering.
    Closed,
    /// The server answered with something this client does not understand.
    Protocol(String),
    /// The server answered with an error.
    Api(ApiError),
    /// No answer came within this time. The app may have carried the
    /// request out before the time ran out, but will not after.
    TimedOut(Duration),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "cannot talk to the app: {error}"),
            Self::Closed => f.write_str("the app closed the connection before answering"),
            Self::Protocol(message) => write!(f, "unexpected answer from the app: {message}"),
            Self::Api(error) => error.fmt(f),
            Self::TimedOut(timeout) => write!(
                f,
                "the app did not answer within {}s; it may have carried out the request \
                 before the time ran out, but will not carry it out later",
                timeout.as_secs_f64()
            ),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<io::Error> for ClientError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// One connection to the app.
pub struct Client {
    reader: BufReader<UnixStream>,
    /// The start of a line whose end has not arrived yet. It is kept when
    /// a call gives up, so the next call does not read half a line.
    partial: Vec<u8>,
    next_id: u64,
    /// Notifications that arrived while a call waited for its answer.
    events: VecDeque<ServerMessage>,
    /// Optional authentication token attached to outgoing requests.
    token: Option<String>,
}

impl Client {
    pub fn connect(path: impl AsRef<Path>) -> io::Result<Self> {
        Ok(Self {
            reader: BufReader::new(UnixStream::connect(path)?),
            partial: Vec::new(),
            next_id: 1,
            events: VecDeque::new(),
            token: token_from_env(),
        })
    }

    /// Sets or clears the authentication token attached to requests sent on
    /// this client.
    pub fn with_token(mut self, token: Option<String>) -> Self {
        self.token = token.filter(|token| !token.is_empty());
        self
    }

    /// Sets or clears the authentication token attached to subsequent requests
    /// sent on this client.
    pub fn set_token(&mut self, token: Option<String>) {
        self.token = token.filter(|token| !token.is_empty());
    }

    /// The next notification, in the order the app sent them: a
    /// [`ServerMessage::Event`]. Waits until there is one; the app sends
    /// them only after [`SubscribeAgentState`](crate::SubscribeAgentState).
    pub fn next_event(&mut self) -> Result<ServerMessage, ClientError> {
        if let Some(event) = self.events.pop_front() {
            return Ok(event);
        }
        let mut line = String::new();
        loop {
            line.clear();
            if self.reader.read_line(&mut line)? == 0 {
                return Err(ClientError::Closed);
            }
            let message = ServerMessage::parse(&line)
                .map_err(|error| ClientError::Protocol(error.to_string()))?;
            // An answer nobody is waiting for is skipped.
            if matches!(message, ServerMessage::Event { .. }) {
                return Ok(message);
            }
        }
    }

    /// Sends one request and waits for its answer, however long it takes.
    /// Notifications that arrive in the meantime are kept for
    /// [`next_event`](Self::next_event).
    pub fn call<M: Method>(&mut self, request: &M) -> Result<M::Output, ClientError> {
        self.call_until(request, None, None)
    }

    /// Like [`Client::call`], but gives up with [`ClientError::TimedOut`]
    /// when no answer has come within `timeout`. An answer that comes later
    /// is skipped by the next call on this connection.
    ///
    /// The app is told to start the request within a shorter time, so
    /// once this gives up the app will not start it.
    pub fn call_within<M: Method>(
        &mut self,
        request: &M,
        timeout: Duration,
    ) -> Result<M::Output, ClientError> {
        let deadline = Instant::now() + timeout;
        self.call_until(
            request,
            Some((deadline, timeout)),
            Some(app_timeout(timeout)),
        )
        .map_err(|error| timed_out(error, timeout))
    }

    /// Like [`Client::call_within`], for a request the app answers long
    /// after it has started it, such as a wait: the app has `timeout` to
    /// start it, less the usual margin, and the answer may take `run`
    /// longer than that.
    pub fn call_long<M: Method>(
        &mut self,
        request: &M,
        timeout: Duration,
        run: Duration,
    ) -> Result<M::Output, ClientError> {
        let whole = timeout.saturating_add(run);
        // A time too long to count is no limit at all.
        let deadline = Instant::now()
            .checked_add(whole)
            .map(|deadline| (deadline, whole));
        self.call_until(request, deadline, Some(app_timeout(timeout)))
            .map_err(|error| timed_out(error, whole))
    }

    fn call_until<M: Method>(
        &mut self,
        request: &M,
        deadline: Option<(Instant, Duration)>,
        app_timeout: Option<Duration>,
    ) -> Result<M::Output, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        let line = request_line(id, request, app_timeout, self.token.as_deref());
        self.wait_until(deadline)?;
        if let Err(error) = self.reader.get_mut().write_all(line.as_bytes()) {
            return Err(self.why_cut_off().unwrap_or(error.into()));
        }

        loop {
            self.wait_until(deadline)?;
            if self.reader.read_until(b'\n', &mut self.partial)? == 0 {
                return Err(ClientError::Closed);
            }
            if self.partial.last() != Some(&b'\n') {
                // The server closed the connection in the middle of a line.
                return Err(ClientError::Closed);
            }
            let line = String::from_utf8(std::mem::take(&mut self.partial))
                .map_err(|_| ClientError::Protocol("an answer that is not UTF-8".into()))?;
            let message = ServerMessage::parse(&line)
                .map_err(|error| ClientError::Protocol(error.to_string()))?;
            match message {
                ServerMessage::Response {
                    id: Some(answered),
                    result,
                } if answered == id => {
                    let result = result.map_err(ClientError::Api)?;
                    return serde_json::from_value(result).map_err(|error| {
                        ClientError::Protocol(format!("bad result for {}: {error}", M::NAME))
                    });
                }
                // The server could not find an id in what we sent, so no
                // answer with our id will follow.
                ServerMessage::Response {
                    id: None,
                    result: Err(error),
                } => return Err(ClientError::Protocol(error.message)),
                // An answer nobody is waiting for.
                ServerMessage::Response { .. } => {}
                event @ ServerMessage::Event { .. } => self.events.push_back(event),
            }
        }
    }

    /// Why the app cut the connection off, when it said so before closing
    /// it: the app stops reading a request line that is too long, so
    /// writing it fails, but the error the app sent first is there to read.
    fn why_cut_off(&mut self) -> Option<ClientError> {
        loop {
            match self.reader.read_until(b'\n', &mut self.partial) {
                Ok(0) | Err(_) => return None,
                Ok(_) => {}
            }
            if self.partial.last() != Some(&b'\n') {
                return None;
            }
            let line = String::from_utf8(std::mem::take(&mut self.partial)).ok()?;
            match ServerMessage::parse(&line).ok()? {
                ServerMessage::Response {
                    id: None,
                    result: Err(error),
                } => return Some(ClientError::Protocol(error.message)),
                ServerMessage::Response { .. } => {}
                event @ ServerMessage::Event { .. } => self.events.push_back(event),
            }
        }
    }

    /// Lets the next read or write on the socket wait only until the
    /// deadline, or for ever without one.
    fn wait_until(&self, deadline: Option<(Instant, Duration)>) -> Result<(), ClientError> {
        let stream = self.reader.get_ref();
        let left = match deadline {
            None => None,
            Some((deadline, timeout)) => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(ClientError::TimedOut(timeout));
                }
                Some(left)
            }
        };
        // macOS refuses to set a timeout on a socket the server has shut
        // down. Reading it then ends at once, and writing it fails at
        // once, so there is nothing to wait for anyway.
        let refused = |error: &io::Error| error.raw_os_error() == Some(libc::EINVAL);
        for set in [UnixStream::set_read_timeout, UnixStream::set_write_timeout] {
            match set(stream, left) {
                Err(error) if !refused(&error) => return Err(error.into()),
                _ => {}
            }
        }
        Ok(())
    }
}

/// A read or write that ran into the deadline of its call is the call
/// timing out.
fn timed_out(error: ClientError, timeout: Duration) -> ClientError {
    match error {
        ClientError::Io(error)
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) =>
        {
            ClientError::TimedOut(timeout)
        }
        error => error,
    }
}

/// How long the app is given to start a request the caller waits `timeout`
/// for. The app counts from when it reads the request, which is after the
/// caller started counting, and its answer takes a moment to come back.
/// The difference leaves room for both: a fifth of the time, at most a
/// second.
fn app_timeout(timeout: Duration) -> Duration {
    timeout - (timeout / 5).min(Duration::from_secs(1))
}

/// Reads the default IPC token from the process environment, preferring
/// `BENTERM_CONTROL_TOKEN` over `BENTERM_PANE_TOKEN`.
fn token_from_env() -> Option<String> {
    [CONTROL_TOKEN_ENV, PANE_TOKEN_ENV]
        .into_iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
}
