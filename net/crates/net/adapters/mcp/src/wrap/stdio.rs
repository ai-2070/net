//! [`StdioMcpClient`] — spawn a stdio MCP server and speak JSON-RPC 2.0 to
//! it over its stdin/stdout (`wrap/stdio.rs` in `MCP_BRIDGE_PLAN.md`).
//!
//! The transport is newline-delimited JSON: each request/notification is one
//! line written to the child's stdin; each response/notification is one line
//! read from its stdout. A background reader task demultiplexes stdout —
//! routing responses to the awaiting caller by JSON-RPC `id`, forwarding
//! `tools/list_changed` notifications to subscribers, and politely rejecting
//! any (unsupported) server-initiated request.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{broadcast, oneshot, watch, Mutex};
use tokio::task::JoinHandle;

use super::McpError;
use crate::spec::{
    self, CallToolParams, CallToolResult, Implementation, InitializeParams, InitializeResult,
    JsonRpcError, JsonRpcNotification, JsonRpcRequest, ListToolsResult, Tool, METHOD_NOT_FOUND,
};

/// Depth of the `tools/list_changed` broadcast. Small: subscribers only need
/// "something changed, re-read `tools/list`", not a lossless event log.
const LIST_CHANGED_CAP: usize = 16;

/// A running, connected stdio MCP server. Dropping it kills the child
/// process (via `kill_on_drop`), which closes stdout and ends the reader.
pub struct StdioMcpClient {
    inner: Arc<Inner>,
    client_info: Implementation,
    /// Reader task handle. Detached on drop; it ends on stdout EOF.
    _reader: JoinHandle<()>,
    /// Kept so the child lives as long as the client. `kill_on_drop(true)`
    /// tears it down when this struct drops.
    _child: Child,
}

/// Shared state the client methods and the reader task both touch.
struct Inner {
    stdin: Mutex<ChildStdin>,
    /// In-flight requests, keyed by the id we assigned, awaiting a response.
    pending: Mutex<HashMap<i64, oneshot::Sender<Result<Value, JsonRpcError>>>>,
    next_id: AtomicI64,
    /// Fires once per `notifications/tools/list_changed` from the server.
    list_changed_tx: broadcast::Sender<()>,
    /// Flips to `true` when the reader task ends (server stdout closed — a
    /// clean exit or a crash). Backs [`StdioMcpClient::closed`].
    closed_tx: watch::Sender<bool>,
}

impl Inner {
    /// Write one framed JSON message (line + `\n`) to the server's stdin.
    async fn write_line(&self, line: &str) -> Result<(), McpError> {
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(McpError::Io)?;
        stdin.write_all(b"\n").await.map_err(McpError::Io)?;
        stdin.flush().await.map_err(McpError::Io)?;
        Ok(())
    }
}

impl StdioMcpClient {
    /// Spawn `program args…` as a stdio MCP server and connect. `envs` are
    /// added to the child's environment — this is where a wrapped tool's
    /// credentials live, on the owning machine, never transiting the mesh.
    ///
    /// The returned client is connected but **not** initialized; call
    /// [`initialize`](Self::initialize) before any `tools/*` call.
    pub async fn spawn(
        program: &str,
        args: &[String],
        envs: &[(String, String)],
        client_info: Implementation,
    ) -> Result<Self, McpError> {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // The server's own logs go to our stderr. Credentials stay in
            // the child on this machine; nothing here transits the mesh.
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().map_err(McpError::Spawn)?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Transport("child stdin pipe missing".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Transport("child stdout pipe missing".into()))?;

        let inner = Arc::new(Inner {
            stdin: Mutex::new(stdin),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicI64::new(1),
            list_changed_tx: broadcast::channel(LIST_CHANGED_CAP).0,
            closed_tx: watch::channel(false).0,
        });
        let reader = tokio::spawn(read_loop(Arc::clone(&inner), stdout));

        Ok(Self {
            inner,
            client_info,
            _reader: reader,
            _child: child,
        })
    }

    /// Perform the MCP `initialize` handshake: send our identity + the pinned
    /// protocol version, then the required `notifications/initialized`. Must
    /// be called once before any tool call.
    pub async fn initialize(&self) -> Result<InitializeResult, McpError> {
        let params = InitializeParams::new(self.client_info.clone());
        let result: InitializeResult = self
            .request(spec::method::INITIALIZE, Some(to_value(&params)?))
            .await?;
        // The spec requires the client to notify the server once ready.
        self.notify(spec::method::INITIALIZED, None).await?;
        Ok(result)
    }

    /// Read the server's advertised tools (`tools/list`).
    ///
    /// v0 reads a single page; `next_cursor` pagination lands with the
    /// descriptor-lowering slice (Phase 1). A conforming small-mesh server
    /// returns everything in one page.
    pub async fn list_tools(&self) -> Result<Vec<Tool>, McpError> {
        let result: ListToolsResult = self.request(spec::method::TOOLS_LIST, None).await?;
        Ok(result.tools)
    }

    /// Invoke a tool (`tools/call`). A returned `Ok(result)` with
    /// `result.is_error == true` is a **tool-level** failure the tool
    /// reported in-band; an `Err(McpError::Protocol(..))` is a JSON-RPC
    /// protocol failure. Callers (and the nRPC bridge) must keep them apart.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
    ) -> Result<CallToolResult, McpError> {
        let params = CallToolParams {
            name: name.to_string(),
            arguments,
        };
        self.request(spec::method::TOOLS_CALL, Some(to_value(&params)?))
            .await
    }

    /// Subscribe to `tools/list_changed` notifications — each recv means
    /// "the tool set changed, re-read `list_tools`". Backed by a broadcast
    /// channel, so a lagging subscriber may miss intermediate signals but
    /// always learns that *a* change occurred.
    pub fn subscribe_list_changed(&self) -> broadcast::Receiver<()> {
        self.inner.list_changed_tx.subscribe()
    }

    /// Resolve when the wrapped server's stdout closes — a clean exit or a
    /// crash. A driver can `select!` on this to withdraw the wrapped tools the
    /// moment the server dies, rather than leaving stale capabilities up.
    /// Returns immediately if it has already closed.
    pub async fn closed(&self) {
        let mut rx = self.inner.closed_tx.subscribe();
        if *rx.borrow() {
            return;
        }
        // Resolves when the reader sends `true` on exit, or (defensively) if
        // the sender is dropped.
        let _ = rx.changed().await;
    }

    /// Send a fire-and-forget notification (no response expected).
    async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), McpError> {
        let note = JsonRpcNotification::new(method, params);
        let line = serde_json::to_string(&note).map_err(McpError::Decode)?;
        self.inner.write_line(&line).await
    }

    /// Send a request and await its typed result.
    async fn request<T: DeserializeOwned>(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<T, McpError> {
        let value = self.request_raw(method, params).await?;
        serde_json::from_value(value).map_err(McpError::Decode)
    }

    /// Send a request and await the raw JSON `result` value (or the mapped
    /// JSON-RPC error).
    async fn request_raw(&self, method: &str, params: Option<Value>) -> Result<Value, McpError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().await.insert(id, tx);

        let req = JsonRpcRequest::new(id, method, params);
        let line = serde_json::to_string(&req).map_err(McpError::Decode)?;
        if let Err(e) = self.inner.write_line(&line).await {
            // The request never went out — reclaim the slot so it can't leak.
            self.inner.pending.lock().await.remove(&id);
            return Err(e);
        }

        match rx.await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(rpc_err)) => Err(McpError::Protocol(rpc_err)),
            // Sender dropped without sending → the reader tore down (server
            // exited / stdout closed) before this request was answered.
            Err(_) => Err(McpError::Transport(
                "connection closed before response".into(),
            )),
        }
    }
}

/// Serialize a value into JSON, mapping the error into [`McpError::Decode`].
fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, McpError> {
    serde_json::to_value(v).map_err(McpError::Decode)
}

/// Upper bound on a single stdout line (one JSON-RPC message) from the wrapped
/// server. A well-behaved MCP server's messages are far smaller; the cap exists
/// only so a malicious or buggy server that streams an unbounded line *without*
/// a newline can't grow the read buffer until the wrapping node runs out of
/// memory. An over-length line is drained and discarded, not buffered.
const MAX_LINE_BYTES: usize = 32 * 1024 * 1024;

/// The background reader: demultiplex the server's stdout until EOF.
async fn read_loop(inner: Arc<Inner>, stdout: ChildStdout) {
    let mut reader = BufReader::new(stdout);
    let mut buf: Vec<u8> = Vec::new();
    loop {
        // Bounded line read: `next_line()` would accumulate an unbounded String
        // for a newline-less flood, so frame lines ourselves with a byte cap.
        match read_capped_line(&mut reader, &mut buf, MAX_LINE_BYTES).await {
            // A complete line within the cap.
            Ok(Some(true)) => {
                let Ok(line) = std::str::from_utf8(&buf) else {
                    continue; // non-UTF-8 stdout noise — ignore, don't tear down
                };
                if line.trim().is_empty() {
                    continue;
                }
                dispatch_line(&inner, line).await;
            }
            // A line that exceeded the cap — discard it (its awaiting call, if
            // any, times out) and keep the connection up rather than OOM.
            Ok(Some(false)) => continue,
            // EOF (`None`) or a read error — either way the server is gone.
            Ok(None) | Err(_) => break,
        }
    }
    // Fail every outstanding request: dropping the senders resolves each
    // awaiting `rx` to a transport error rather than hanging forever.
    inner.pending.lock().await.clear();
    // Signal closure so a driver can withdraw the wrapped tools. Use
    // `send_replace`, not `send`: `send` drops the value when there are no
    // receivers, so if the server exits before anyone calls `closed()` the
    // stored value would stay `false` and a later `closed()` would wait on
    // `changed()` forever (the sender lives as long as the client, so it never
    // resolves). `send_replace` always stores `true`, so a later `subscribe()`
    // observes it immediately.
    inner.closed_tx.send_replace(true);
}

/// Read one `\n`-terminated line (newline stripped) into `buf`, buffering at
/// most `max` bytes. Bytes of a line that exceeds `max` are drained and
/// discarded up to its terminating newline (or EOF), so memory stays bounded
/// regardless of what the wrapped server emits.
///
/// Returns `Ok(Some(true))` for a line within the cap (`buf` holds it),
/// `Ok(Some(false))` for an over-length line (`buf` holds only its capped
/// prefix, meant to be ignored), or `Ok(None)` at EOF with nothing pending.
async fn read_capped_line<R: AsyncBufReadExt + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    max: usize,
) -> std::io::Result<Option<bool>> {
    buf.clear();
    let mut within_cap = true;
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            // EOF: emit a trailing unterminated line if we accumulated one.
            return if buf.is_empty() && within_cap {
                Ok(None)
            } else {
                Ok(Some(within_cap))
            };
        }
        // How many bytes of this chunk belong to the current line, how many to
        // consume from the reader, and whether the line terminates here.
        let (line_bytes, consume, done) = match chunk.iter().position(|&b| b == b'\n') {
            Some(nl) => (nl, nl + 1, true),
            None => (chunk.len(), chunk.len(), false),
        };
        // Copy up to `max` bytes; once over the cap, keep draining the rest of
        // the line (advancing the reader) but stop growing `buf`.
        if within_cap {
            let room = max.saturating_sub(buf.len());
            if line_bytes > room {
                buf.extend_from_slice(&chunk[..room]);
                within_cap = false;
            } else {
                buf.extend_from_slice(&chunk[..line_bytes]);
            }
        }
        reader.consume(consume);
        if done {
            return Ok(Some(within_cap));
        }
    }
}

/// A loosely-typed inbound message, classified by which fields are present.
#[derive(serde::Deserialize)]
struct Incoming {
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<JsonRpcError>,
}

/// Classify and act on one inbound line.
async fn dispatch_line(inner: &Arc<Inner>, line: &str) {
    let msg: Incoming = match serde_json::from_str(line) {
        Ok(m) => m,
        // Not JSON-RPC (e.g. a server that prints stray text to stdout) —
        // ignore rather than tearing the connection down.
        Err(_) => return,
    };

    match (msg.method, msg.id) {
        // Server-initiated request (method + id). The bridge is a pure
        // request/response client; reject sampling / elicitation politely so
        // the server isn't left waiting.
        //
        // Reply OFF the read path. The reader is the *sole* drainer of the
        // child's stdout; if it blocked here writing to a full stdin (while the
        // child is itself blocked writing a full stdout, not reading stdin),
        // both pipes would wedge — a two-pipe deadlock hanging every in-flight
        // request. Spawning keeps the reader draining; `write_line` still
        // serializes on the stdin mutex, so replies and client requests can't
        // interleave mid-line.
        (Some(_method), Some(id)) => {
            let inner = Arc::clone(inner);
            tokio::spawn(async move { reply_method_not_found(&inner, id).await });
        }
        // Notification (method, no id).
        (Some(method), None) => {
            if method == spec::method::TOOLS_LIST_CHANGED {
                let _ = inner.list_changed_tx.send(());
            }
            // Other notifications are not part of the compat tier — ignore.
        }
        // Response to one of our requests (id, no method).
        (None, Some(id)) => match id.as_i64() {
            Some(id) => {
                if let Some(tx) = inner.pending.lock().await.remove(&id) {
                    let payload = match msg.error {
                        Some(err) => Err(err),
                        None => Ok(msg.result.unwrap_or(Value::Null)),
                    };
                    let _ = tx.send(payload);
                }
                // An unmatched integer id (a duplicate or already-resolved
                // response) is benign — ignore it.
            }
            // We only ever send integer ids, so a non-integer response id is a
            // protocol violation. It can never match a pending call, so that
            // call would hang forever waiting for its real reply — fail every
            // in-flight call instead (each `rx` resolves to a transport error).
            None => inner.pending.lock().await.clear(),
        },
        // Neither method nor id — malformed; ignore.
        (None, None) => {}
    }
}

/// Reply to an unsupported server-initiated request with JSON-RPC
/// `method not found`, echoing the request id verbatim.
async fn reply_method_not_found(inner: &Arc<Inner>, id: Value) {
    let reply = serde_json::json!({
        "jsonrpc": spec::JSONRPC_VERSION,
        "id": id,
        "error": {
            "code": METHOD_NOT_FOUND,
            "message": "server-initiated requests are not supported by the Net MCP bridge",
        }
    });
    // Best-effort: if the pipe is gone the connection is ending anyway.
    if let Ok(line) = serde_json::to_string(&reply) {
        let _ = inner.write_line(&line).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn read_line(reader: &mut (impl AsyncBufReadExt + Unpin), max: usize) -> Option<String> {
        let mut buf = Vec::new();
        match read_capped_line(reader, &mut buf, max).await.unwrap() {
            Some(true) => Some(String::from_utf8(buf).unwrap()),
            Some(false) => None, // over-length line: caller discards it
            None => Some("<EOF>".to_string()),
        }
    }

    #[tokio::test]
    async fn capped_reader_frames_lines_within_the_cap() {
        let data = b"hello\nworld\n".to_vec();
        let mut r = BufReader::new(&data[..]);
        assert_eq!(read_line(&mut r, 64).await.as_deref(), Some("hello"));
        assert_eq!(read_line(&mut r, 64).await.as_deref(), Some("world"));
        assert_eq!(read_line(&mut r, 64).await.as_deref(), Some("<EOF>"));
    }

    #[tokio::test]
    async fn capped_reader_discards_an_overlong_line_and_recovers() {
        // F7: an over-length line (no attacker can grow the buffer past `max`)
        // is dropped, and the *next* line still frames correctly — proving the
        // overlong line's terminating newline was consumed, not left to corrupt
        // the following message.
        let data = b"ok\nTHIS-LINE-IS-WAY-TOO-LONG\nnext\n".to_vec();
        let mut r = BufReader::new(&data[..]);
        assert_eq!(read_line(&mut r, 8).await.as_deref(), Some("ok"));
        // "THIS-LINE-IS-WAY-TOO-LONG" is 25 bytes > 8 → discarded (None here).
        assert_eq!(read_line(&mut r, 8).await, None);
        assert_eq!(read_line(&mut r, 8).await.as_deref(), Some("next"));
        assert_eq!(read_line(&mut r, 8).await.as_deref(), Some("<EOF>"));
    }

    #[tokio::test]
    async fn capped_reader_emits_a_trailing_unterminated_line() {
        // A final line with no newline is still delivered (within the cap).
        let data = b"tail-no-newline".to_vec();
        let mut r = BufReader::new(&data[..]);
        assert_eq!(
            read_line(&mut r, 64).await.as_deref(),
            Some("tail-no-newline")
        );
        assert_eq!(read_line(&mut r, 64).await.as_deref(), Some("<EOF>"));
    }

    #[tokio::test]
    async fn capped_reader_bounds_a_newlineless_flood() {
        // The core property: a long run of bytes with NO newline is bounded to
        // `max`, never buffered whole (an OOM without the cap).
        let data = vec![b'x'; 10_000];
        let mut r = BufReader::new(&data[..]);
        // Over the cap → discarded, reported as an over-length line.
        assert_eq!(read_line(&mut r, 16).await, None);
        assert_eq!(read_line(&mut r, 16).await.as_deref(), Some("<EOF>"));
    }
}
