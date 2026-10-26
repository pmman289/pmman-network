//! Bounded, one-request JSON-line transport for Tokio named pipes and Unix streams.
//!
//! Integration (the listener still owns pipe ACLs / Unix socket permissions):
//! ```ignore
//! mod ipc_transport;
//! use ipc_transport::{IpcTransport, TransportConfig};
//!
//! // Construct ONCE outside the accept loop; clones share the same limit.
//! let transport = IpcTransport::new(TransportConfig::default(), |error| {
//!     diagnostics::log("ipc_transport", &error.to_string());
//! });
//! // Unix: after listener.accept().await, pass the accepted stream below.
//! // Windows: create the FIRST instance with first_pipe_instance(true).
//! // After pending.connect().await, keep the connected instance alive, create
//! // its successor with first_pipe_instance(false) and the same ACL, THEN
//! // dispatch the connected instance below. Keeping a pending instance alive
//! // also avoids a gap in pipe ownership between clients.
//! let state = state.clone();
//! let _ = transport.try_spawn(stream, move |request: IpcRequest| {
//!     handle_request(&state, request)
//! });
//! // Immediately accept on the already-created next instance. Do not await the
//! // returned task here or propagate per-connection rejection with `?`.
//! ```
//!
//! Malformed, incomplete, oversized and timed-out requests are logged and closed
//! without invoking the handler or sending a protocol response. Application
//! failures should be represented in the handler's normal response type.
//! There is deliberately no handler deadline: blocking work cannot be forcibly
//! cancelled safely. Even if a connection task is aborted, its blocking worker
//! retains its admission permit until it actually exits. This bounds queued and
//! running workers as well as live connection tasks. Handlers must supply their
//! own bounded external operations; a permanently stuck handler retains a slot.
//! The logger must be fast and non-panicking, and must not log request bodies.
//! This module never formats panic payloads; the process-wide panic hook remains
//! owned by the application and must independently avoid sensitive diagnostics.

use std::{fmt, io, num::NonZeroUsize, sync::Arc, time::Duration};

use serde::{de::DeserializeOwned, Serialize};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::Semaphore,
    task::JoinHandle,
    time::timeout,
};

/// Maximum wire request size, including the terminating LF (and CR if present).
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct TransportConfig {
    /// Shared across connections and blocking handlers, including aborted tasks.
    pub max_connections: NonZeroUsize,
    /// Total time to receive a complete line, not a per-read idle timeout.
    pub read_timeout: Duration,
    /// Total time for response write_all + flush, starting after the handler.
    pub write_timeout: Duration,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            max_connections: NonZeroUsize::new(16).unwrap(),
            read_timeout: Duration::from_secs(3),
            write_timeout: Duration::from_secs(5),
        }
    }
}

/// Safe to log with either Display or Debug. Never stores bodies, serde error
/// messages, OS error text, response values or panic payloads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportError {
    Busy,
    ReadTimeout,
    Read(io::ErrorKind),
    EmptyRequest,
    IncompleteRequest,
    RequestTooLarge,
    InvalidRequest,
    HandlerPanicked,
    HandlerCancelled,
    InvalidResponse,
    WriteTimeout,
    Write(io::ErrorKind),
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => f.write_str("IPC connection limit reached"),
            Self::ReadTimeout => f.write_str("IPC request read timed out"),
            Self::Read(kind) => write!(f, "IPC request read failed ({kind:?})"),
            Self::EmptyRequest => f.write_str("IPC request is empty"),
            Self::IncompleteRequest => f.write_str("IPC request ended before LF"),
            Self::RequestTooLarge => f.write_str("IPC request exceeds 64 KiB including LF"),
            Self::InvalidRequest => f.write_str("IPC request is invalid JSON or schema"),
            Self::HandlerPanicked => f.write_str("IPC blocking worker panicked"),
            Self::HandlerCancelled => f.write_str("IPC blocking worker was cancelled"),
            Self::InvalidResponse => f.write_str("IPC response serialization failed"),
            Self::WriteTimeout => f.write_str("IPC response write timed out"),
            Self::Write(kind) => write!(f, "IPC response write failed ({kind:?})"),
        }
    }
}

impl std::error::Error for TransportError {}

#[derive(Clone)]
pub struct IpcTransport {
    config: TransportConfig,
    slots: Arc<Semaphore>,
    log_error: Arc<dyn Fn(TransportError) + Send + Sync>,
}

impl IpcTransport {
    pub fn new(
        config: TransportConfig,
        log_error: impl Fn(TransportError) + Send + Sync + 'static,
    ) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(config.max_connections.get())),
            config,
            log_error: Arc::new(log_error),
        }
    }

    /// Admit an already-connected AsyncRead + AsyncWrite stream without waiting.
    /// On overload, logs Busy, drops the stream and spawns no task. The returned
    /// task owns the stream and logs all subsequent connection errors itself;
    /// dropping its JoinHandle detaches it (normal listener usage).
    ///
    /// Use with tokio::net::windows::named_pipe::NamedPipeServer or
    /// tokio::net::UnixStream. No listener handle is moved into the task.
    pub fn try_spawn<S, Request, Response, Handler>(
        &self,
        mut stream: S,
        handler: Handler,
    ) -> Result<JoinHandle<()>, TransportError>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
        Request: DeserializeOwned + 'static,
        Response: Serialize + 'static,
        Handler: FnOnce(Request) -> Response + Send + 'static,
    {
        let permit = match self.slots.clone().try_acquire_owned() {
            Ok(permit) => Arc::new(permit),
            Err(_) => {
                (self.log_error)(TransportError::Busy);
                return Err(TransportError::Busy);
            }
        };
        let config = self.config;
        let log_error = self.log_error.clone();
        Ok(tokio::spawn(async move {
            // Both owners must exit before admission is released. In particular,
            // aborting this future cannot free a still-running blocking worker.
            let connection_permit = permit;
            let result = async {
                let line = timeout(config.read_timeout, read_request(&mut stream))
                    .await
                    .map_err(|_| TransportError::ReadTimeout)??;
                let worker_permit = connection_permit.clone();
                let response = tokio::task::spawn_blocking(move || {
                    let _permit = worker_permit;
                    let request: Request = serde_json::from_slice(&line)
                        .map_err(|_| TransportError::InvalidRequest)?;
                    let response = handler(request);
                    let mut bytes = serde_json::to_vec(&response)
                        .map_err(|_| TransportError::InvalidResponse)?;
                    bytes.push(b'\n');
                    Ok::<_, TransportError>(bytes)
                })
                .await
                .map_err(|error| {
                    if error.is_panic() {
                        TransportError::HandlerPanicked
                    } else {
                        TransportError::HandlerCancelled
                    }
                })??;
                timeout(config.write_timeout, async {
                    stream
                        .write_all(&response)
                        .await
                        .map_err(|error| TransportError::Write(error.kind()))?;
                    stream
                        .flush()
                        .await
                        .map_err(|error| TransportError::Write(error.kind()))
                })
                .await
                .map_err(|_| TransportError::WriteTimeout)?
            }
            .await;
            if let Err(error) = result {
                log_error(error);
            }
        }))
    }
}

async fn read_request<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Vec<u8>, TransportError> {
    let mut line = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let remaining = MAX_REQUEST_BYTES - line.len();
        if remaining == 0 {
            return Err(TransportError::RequestTooLarge);
        }
        let capacity = remaining.min(chunk.len());
        let count = stream
            .read(&mut chunk[..capacity])
            .await
            .map_err(|error| TransportError::Read(error.kind()))?;
        if count == 0 {
            return Err(if line.is_empty() {
                TransportError::EmptyRequest
            } else {
                TransportError::IncompleteRequest
            });
        }
        if let Some(end) = chunk[..count].iter().position(|byte| *byte == b'\n') {
            line.extend_from_slice(&chunk[..end]);
            return if line.iter().all(u8::is_ascii_whitespace) {
                Err(TransportError::EmptyRequest)
            } else {
                Ok(line)
            };
        }
        line.extend_from_slice(&chunk[..count]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::{
        pin::Pin,
        sync::{mpsc, Mutex},
        task::{Context, Poll},
    };
    use tokio::io::{duplex, DuplexStream, ReadBuf};

    #[derive(Deserialize)]
    struct Request {
        value: String,
    }

    enum Fault {
        Read,
        Write,
        FlushPending,
    }

    struct FaultyIo(DuplexStream, Fault);

    impl AsyncRead for FaultyIo {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buffer: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            if matches!(self.1, Fault::Read) {
                Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "sensitive-io-body",
                )))
            } else {
                Pin::new(&mut self.0).poll_read(cx, buffer)
            }
        }
    }

    impl AsyncWrite for FaultyIo {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            if matches!(self.1, Fault::Write) {
                Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "sensitive-io-body",
                )))
            } else {
                Pin::new(&mut self.0).poll_write(cx, bytes)
            }
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            if matches!(self.1, Fault::FlushPending) {
                Poll::Pending
            } else {
                Pin::new(&mut self.0).poll_flush(cx)
            }
        }

        fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.0).poll_shutdown(cx)
        }
    }

    fn transport(limit: usize) -> (IpcTransport, Arc<Mutex<Vec<TransportError>>>) {
        let errors = Arc::new(Mutex::new(Vec::new()));
        let sink = errors.clone();
        let transport = IpcTransport::new(
            TransportConfig {
                max_connections: NonZeroUsize::new(limit).unwrap(),
                read_timeout: Duration::from_millis(200),
                write_timeout: Duration::from_millis(200),
            },
            move |error| sink.lock().unwrap().push(error),
        );
        (transport, errors)
    }

    async fn finish(task: JoinHandle<()>) {
        timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap();
    }

    async fn response(client: &mut DuplexStream) -> String {
        let mut bytes = Vec::new();
        timeout(Duration::from_secs(2), client.read_to_end(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        String::from_utf8(bytes).unwrap()
    }

    async fn round_trip(transport: &IpcTransport, wire: &[u8]) -> String {
        let (mut client, server) = duplex(MAX_REQUEST_BYTES * 2);
        let task = transport
            .try_spawn(server, |request: Request| request.value)
            .unwrap();
        client.write_all(wire).await.unwrap();
        let response = response(&mut client).await;
        finish(task).await;
        response
    }

    #[tokio::test]
    async fn one_request_per_connection_and_crlf_is_supported() {
        let (transport, errors) = transport(2);
        assert_eq!(
            round_trip(
                &transport,
                b"{\"value\":\"first\"}\r\n{\"value\":\"second\"}\n"
            )
            .await,
            "\"first\"\n"
        );
        assert!(errors.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn closed_empty_blank_and_incomplete_connections_are_isolated() {
        let (transport, errors) = transport(1);
        for (wire, expected) in [
            (&b""[..], TransportError::EmptyRequest),
            (&b" \r\n"[..], TransportError::EmptyRequest),
            (
                &b"{\"value\":\"partial\"}"[..],
                TransportError::IncompleteRequest,
            ),
        ] {
            let (mut client, server) = duplex(128);
            let task = transport
                .try_spawn(server, |_: Request| -> () {
                    panic!("must not invoke handler")
                })
                .unwrap();
            client.write_all(wire).await.unwrap();
            client.shutdown().await.unwrap();
            assert_eq!(response(&mut client).await, "");
            finish(task).await;
            assert_eq!(errors.lock().unwrap().pop(), Some(expected));
        }
        assert_eq!(
            round_trip(&transport, b"{\"value\":\"ok\"}\n").await,
            "\"ok\"\n"
        );
    }

    #[tokio::test]
    async fn idle_connection_times_out_without_blocking_another_request() {
        let (transport, errors) = transport(2);
        let (_idle, server) = duplex(32);
        let task = transport.try_spawn(server, |r: Request| r.value).unwrap();
        assert_eq!(
            round_trip(&transport, b"{\"value\":\"ok\"}\n").await,
            "\"ok\"\n"
        );
        finish(task).await;
        assert_eq!(*errors.lock().unwrap(), [TransportError::ReadTimeout]);
    }

    #[tokio::test]
    async fn slow_drip_cannot_extend_total_read_deadline() {
        let (transport, errors) = transport(1);
        let (mut client, server) = duplex(32);
        let task = transport.try_spawn(server, |r: Request| r.value).unwrap();
        let drip = tokio::spawn(async move {
            loop {
                if client.write_all(b" ").await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        });
        finish(task).await;
        finish(drip).await;
        assert_eq!(*errors.lock().unwrap(), [TransportError::ReadTimeout]);
    }

    #[tokio::test]
    async fn accepts_exact_limit_and_rejects_larger_lines_without_handler() {
        let (transport, errors) = transport(1);
        let mut exact = b"{\"value\":\"ok\"}".to_vec();
        exact.resize(MAX_REQUEST_BYTES - 1, b' ');
        exact.push(b'\n');
        assert_eq!(round_trip(&transport, &exact).await, "\"ok\"\n");
        for terminated in [false, true] {
            let (mut client, server) = duplex(MAX_REQUEST_BYTES * 2);
            let task = transport
                .try_spawn(server, |_: Request| -> () {
                    panic!("must not invoke handler")
                })
                .unwrap();
            let mut oversized = vec![b'x'; MAX_REQUEST_BYTES + 1];
            if terminated {
                oversized.push(b'\n');
            }
            client.write_all(&oversized).await.unwrap();
            assert_eq!(response(&mut client).await, "");
            finish(task).await;
        }
        assert_eq!(
            *errors.lock().unwrap(),
            [
                TransportError::RequestTooLarge,
                TransportError::RequestTooLarge
            ]
        );
    }

    #[tokio::test]
    async fn malformed_json_schema_and_utf8_log_no_body() {
        let (transport, errors) = transport(1);
        for wire in [
            &b"sensitive-body-is-not-json\n"[..],
            &b"{\"value\":{\"password\":\"sensitive-body\"}}\n"[..],
            &b"{\"value\":\"\xff\"}\n"[..],
        ] {
            assert_eq!(round_trip(&transport, wire).await, "");
        }
        assert_eq!(*errors.lock().unwrap(), [TransportError::InvalidRequest; 3]);
        for error in errors.lock().unwrap().iter() {
            assert!(!format!("{error} {error:?}").contains("sensitive-body"));
        }
        assert_eq!(
            round_trip(&transport, b"{\"value\":\"ok\"}\n").await,
            "\"ok\"\n"
        );
    }

    #[tokio::test]
    async fn nonreading_peer_times_out_and_releases_slot() {
        let (transport, errors) = transport(1);
        let (mut client, server) = duplex(32);
        let task = transport
            .try_spawn(server, |_: Request| "x".repeat(1024))
            .unwrap();
        client.write_all(b"{\"value\":\"ok\"}\n").await.unwrap();
        finish(task).await;
        assert_eq!(*errors.lock().unwrap(), [TransportError::WriteTimeout]);
        assert_eq!(
            round_trip(&transport, b"{\"value\":\"ok\"}\n").await,
            "\"ok\"\n"
        );
    }

    #[tokio::test]
    async fn read_and_write_errors_discard_sensitive_io_error_text() {
        let (transport, errors) = transport(1);
        for (fault, expected) in [
            (
                Fault::Read,
                TransportError::Read(io::ErrorKind::ConnectionReset),
            ),
            (
                Fault::Write,
                TransportError::Write(io::ErrorKind::BrokenPipe),
            ),
        ] {
            let (mut client, server) = duplex(128);
            let task = transport
                .try_spawn(FaultyIo(server, fault), |r: Request| r.value)
                .unwrap();
            client.write_all(b"{\"value\":\"ok\"}\n").await.unwrap();
            assert_eq!(response(&mut client).await, "");
            finish(task).await;
            assert_eq!(errors.lock().unwrap().pop(), Some(expected));
            assert!(!format!("{expected} {expected:?}").contains("sensitive-io-body"));
        }
        assert_eq!(
            round_trip(&transport, b"{\"value\":\"ok\"}\n").await,
            "\"ok\"\n"
        );
    }

    #[tokio::test]
    async fn write_deadline_also_covers_flush() {
        let (transport, errors) = transport(1);
        let (mut client, server) = duplex(128);
        let task = transport
            .try_spawn(FaultyIo(server, Fault::FlushPending), |r: Request| r.value)
            .unwrap();
        client.write_all(b"{\"value\":\"ok\"}\n").await.unwrap();
        finish(task).await;
        assert_eq!(response(&mut client).await, "\"ok\"\n");
        assert_eq!(*errors.lock().unwrap(), [TransportError::WriteTimeout]);
        assert_eq!(
            round_trip(&transport, b"{\"value\":\"ok\"}\n").await,
            "\"ok\"\n"
        );
    }

    #[tokio::test]
    async fn serialization_failure_discards_sensitive_error_text() {
        struct InvalidResponse;
        impl Serialize for InvalidResponse {
            fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("sensitive-response-body"))
            }
        }
        let (transport, errors) = transport(1);
        let (mut client, server) = duplex(128);
        let task = transport
            .try_spawn(server, |_: Request| InvalidResponse)
            .unwrap();
        client.write_all(b"{\"value\":\"ok\"}\n").await.unwrap();
        assert_eq!(response(&mut client).await, "");
        finish(task).await;
        assert_eq!(*errors.lock().unwrap(), [TransportError::InvalidResponse]);
        assert_eq!(
            round_trip(&transport, b"{\"value\":\"ok\"}\n").await,
            "\"ok\"\n"
        );
    }

    // current_thread is intentional: a blocking handler on the Tokio executor
    // would prevent the second request from completing before gate release.
    #[tokio::test(flavor = "current_thread")]
    async fn slow_handler_does_not_stall_another_request() {
        let (transport, errors) = transport(2);
        let (mut client, server) = duplex(128);
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let task = transport
            .try_spawn(server, move |request: Request| {
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                request.value
            })
            .unwrap();
        client.write_all(b"{\"value\":\"slow\"}\n").await.unwrap();
        timeout(Duration::from_secs(2), entered_rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            round_trip(&transport, b"{\"value\":\"fast\"}\n").await,
            "\"fast\"\n"
        );
        release_tx.send(()).unwrap();
        assert_eq!(response(&mut client).await, "\"slow\"\n");
        finish(task).await;
        assert!(errors.lock().unwrap().is_empty());
    }

    #[cfg(windows)]
    #[tokio::test(flavor = "current_thread")]
    async fn windows_pipe_successor_accepts_status_while_first_request_is_running() {
        use tokio::io::{AsyncBufReadExt, BufReader};
        use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};
        let name = format!(r"\\.\pipe\pmman-ipc-unit-{}", uuid::Uuid::new_v4());
        let pending = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .unwrap();
        // A second backend cannot attach itself as the owner of this name.
        assert!(ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .is_err());
        let (transport, errors) = transport(2);
        let mut first_client = ClientOptions::new().open(&name).unwrap();
        pending.connect().await.unwrap();
        let successor = ServerOptions::new().create(&name).unwrap();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let first = transport
            .try_spawn(pending, move |request: Request| {
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                request.value
            })
            .unwrap();
        first_client
            .write_all(b"{\"value\":\"slow\"}\n")
            .await
            .unwrap();
        timeout(Duration::from_secs(2), entered_rx)
            .await
            .unwrap()
            .unwrap();
        // Opening the successor must not return ERROR_PIPE_BUSY (231).
        let mut second_client = ClientOptions::new().open(&name).unwrap();
        successor.connect().await.unwrap();
        let _next = ServerOptions::new().create(&name).unwrap();
        let second = transport
            .try_spawn(successor, |request: Request| request.value)
            .unwrap();
        second_client
            .write_all(b"{\"value\":\"status\"}\n")
            .await
            .unwrap();
        let mut line = String::new();
        timeout(
            Duration::from_secs(1),
            BufReader::new(second_client).read_line(&mut line),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(line, "\"status\"\n");
        release_tx.send(()).unwrap();
        finish(first).await;
        finish(second).await;
        assert!(errors.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn admission_is_shared_bounded_and_recovers_after_read_timeout() {
        let (transport, errors) = transport(1);
        let (_idle, server) = duplex(32);
        let task = transport.try_spawn(server, |r: Request| r.value).unwrap();
        for _ in 0..32 {
            let (mut client, server) = duplex(32);
            assert!(matches!(
                transport.clone().try_spawn(server, |r: Request| r.value),
                Err(TransportError::Busy)
            ));
            assert_eq!(response(&mut client).await, "");
        }
        finish(task).await;
        assert_eq!(errors.lock().unwrap().len(), 33);
        assert_eq!(
            round_trip(&transport, b"{\"value\":\"ok\"}\n").await,
            "\"ok\"\n"
        );
    }

    #[tokio::test]
    async fn aborting_connection_does_not_release_running_handler_slot() {
        let (transport, errors) = transport(1);
        let (mut client, server) = duplex(128);
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let task = transport
            .try_spawn(server, move |r: Request| {
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                r.value
            })
            .unwrap();
        client.write_all(b"{\"value\":\"slow\"}\n").await.unwrap();
        timeout(Duration::from_secs(2), entered_rx)
            .await
            .unwrap()
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let (_client, server) = duplex(32);
        assert!(matches!(
            transport.try_spawn(server, |r: Request| r.value),
            Err(TransportError::Busy)
        ));
        release_tx.send(()).unwrap();
        // Waiting only in the test; production admission never queues waiters.
        let permit = timeout(Duration::from_secs(2), transport.slots.acquire())
            .await
            .unwrap()
            .unwrap();
        drop(permit);
        assert_eq!(
            round_trip(&transport, b"{\"value\":\"ok\"}\n").await,
            "\"ok\"\n"
        );
        assert_eq!(*errors.lock().unwrap(), [TransportError::Busy]);
    }

    #[tokio::test]
    async fn handler_panic_is_isolated_and_slot_recovers() {
        let (transport, errors) = transport(1);
        let (mut client, server) = duplex(128);
        let task = transport
            .try_spawn(server, |_: Request| -> String {
                panic!("test handler panic")
            })
            .unwrap();
        client.write_all(b"{\"value\":\"ok\"}\n").await.unwrap();
        assert_eq!(response(&mut client).await, "");
        finish(task).await;
        assert_eq!(*errors.lock().unwrap(), [TransportError::HandlerPanicked]);
        assert_eq!(
            round_trip(&transport, b"{\"value\":\"ok\"}\n").await,
            "\"ok\"\n"
        );
    }
}
