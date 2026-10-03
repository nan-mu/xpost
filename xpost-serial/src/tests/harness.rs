//! Injectable-IO session harness: memory duplex + client/server SerialHandlers.

use std::convert::Infallible;
use std::fmt;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::client::error::{Interrupted, ParseError, RequestTimeout};
use crate::client::{MemoryPair, MemoryServer, SerialHandler};
use crate::r#type::reply::{ErrReply, ServerError};
use crate::server::serial::{DefaultHandler, RequestHandler, SerialHandlerBuilder};
use crate::server::SerialHandler as ServerHandler;

pub type ReadFn = Box<dyn FnMut() -> Option<u8> + Send>;
pub type WriteFn = Box<dyn FnMut(&[u8]) -> Result<(), Infallible> + Send>;
pub type TestBuilder = SerialHandlerBuilder<ReadFn, WriteFn, DefaultHandler, 128>;

/// Concrete `E` for [`SerialHandler::request`] in session tests.
pub enum TestError {
    Parse(ParseError),
    Server(ServerError),
    Timeout(RequestTimeout),
    Interrupted,
    Io(io::Error),
}

impl ErrReply for TestError {}

impl From<ParseError> for TestError {
    fn from(e: ParseError) -> Self {
        Self::Parse(e)
    }
}

impl From<ServerError> for TestError {
    fn from(e: ServerError) -> Self {
        Self::Server(e)
    }
}

impl From<RequestTimeout> for TestError {
    fn from(e: RequestTimeout) -> Self {
        Self::Timeout(e)
    }
}

impl From<Interrupted> for TestError {
    fn from(_: Interrupted) -> Self {
        Self::Interrupted
    }
}

impl From<io::Error> for TestError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl fmt::Debug for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "Parse({e})"),
            Self::Server(e) => write!(f, "Server({e:?})"),
            Self::Timeout(e) => write!(f, "Timeout({e})"),
            Self::Interrupted => write!(f, "Interrupted"),
            Self::Io(e) => write!(f, "Io({e})"),
        }
    }
}

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

struct ServerTask {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl Drop for ServerTask {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn spawn_server<H>(mut handler: ServerHandler<ReadFn, WriteFn, H, 128>) -> ServerTask
where
    H: RequestHandler + Send + 'static,
{
    let stop = Arc::new(AtomicBool::new(false));
    let stop_t = Arc::clone(&stop);
    let join = thread::spawn(move || {
        handler.start();
        while !stop_t.load(Ordering::Acquire) {
            if !handler.pump() {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
    });
    ServerTask {
        stop,
        join: Some(join),
    }
}

fn builder(server: MemoryServer) -> TestBuilder {
    let read_end = server.clone();
    let write_end = server;
    let read: ReadFn = Box::new(move || read_end.read_byte());
    let write: WriteFn = Box::new(move |bytes: &[u8]| write_end.write_all(bytes));
    ServerHandler::builder(read, write, [0u8; 128])
}

/// Start memory server (with user `on_*` registrations) + client, run `body`, then shut down.
///
/// `register` must end with [`.build()`](SerialHandlerBuilder::build).
/// Client log/file tee is not exercised ([`SerialHandler::open_memory`]).
pub fn with_session<H, F>(
    register: impl FnOnce(TestBuilder) -> ServerHandler<ReadFn, WriteFn, H, 128>,
    body: F,
) where
    H: RequestHandler + Send + 'static,
    F: FnOnce(&SerialHandler),
{
    let pair = MemoryPair::new();
    let client = SerialHandler::open_memory(pair.client);
    let handler = register(builder(pair.server));
    let _server = spawn_server(handler);
    // Let the server emit `OK init` and the client reader attach.
    thread::sleep(Duration::from_millis(20));
    body(&client);
    client.shutdown();
}

/// Like [`with_session`], but tees the client transcript to a temp file and returns its lines.
pub fn with_session_logged<H, F>(
    register: impl FnOnce(TestBuilder) -> ServerHandler<ReadFn, WriteFn, H, 128>,
    body: F,
) -> Vec<String>
where
    H: RequestHandler + Send + 'static,
    F: FnOnce(&SerialHandler),
{
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let log_path: PathBuf = std::env::temp_dir().join(format!(
        "xpost-serial-log-{}-{nanos}.log",
        std::process::id()
    ));

    let pair = MemoryPair::new();
    let client = SerialHandler::open_memory_with_log(pair.client, &log_path)
        .expect("open_memory_with_log");
    let handler = register(builder(pair.server));
    let _server = spawn_server(handler);
    thread::sleep(Duration::from_millis(20));
    body(&client);
    let lines = client.log_lines().expect("read session log");
    client.shutdown();
    let _ = std::fs::remove_file(&log_path);
    lines
}
