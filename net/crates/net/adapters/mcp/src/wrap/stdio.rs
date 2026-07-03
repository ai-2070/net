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

/// The background reader: demultiplex the server's stdout until EOF.
async fn read_loop(inner: Arc<Inner>, stdout: ChildStdout) {
    let mut lines = BufReader::new(stdout).lines();
    // Ends on `Ok(None)` (EOF) or `Err(_)` (read error) — either way the
    // server is gone.
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        dispatch_line(&inner, &line).await;
    }
    // Fail every outstanding request: dropping the senders resolves each
    // awaiting `rx` to a transport error rather than hanging forever.
    inner.pending.lock().await.clear();
    // Signal closure so a driver can withdraw the wrapped tools.
    let _ = inner.closed_tx.send(true);
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
        (Some(_method), Some(id)) => {
            reply_method_not_found(inner, id).await;
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
