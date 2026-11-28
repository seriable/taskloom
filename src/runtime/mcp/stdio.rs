//! One owned MCP subprocess with serialized, cancellation-safe JSON-RPC IO.
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot, Notify};
use tokio::time::Instant;

use super::{error, McpInteractions, McpToolInfo, RuntimeError, PROTOCOLS};

/// Explicit host process configuration. Intentionally neither Debug nor Serialize:
/// arguments may contain confidential host data. Prefer environment names for secrets.
pub struct McpStdioConfig {
    /// Existing absolute executable file; no shell, PATH search or command string.
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Existing absolute working directory, chosen by the host.
    pub cwd: PathBuf,
    /// Only these current host environment values are copied to the subprocess.
    /// No ambient environment (including PATH) is inherited otherwise.
    pub environment: BTreeSet<String>,
    /// Stable nonsecret host identity; change on credential/account changes.
    pub namespace: String,
    pub timeout: Duration,
    pub max_message_bytes: usize,
    pub max_tools: usize,
    /// Pending requests are bounded; wire IO remains serialized.
    pub max_pending_requests: usize,
}
impl McpStdioConfig {
    pub fn new(program: impl Into<PathBuf>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: cwd.into(),
            environment: BTreeSet::new(),
            namespace: "default".into(),
            timeout: Duration::from_secs(30),
            max_message_bytes: 2 * 1024 * 1024,
            max_tools: 256,
            max_pending_requests: 16,
        }
    }
}
struct WireRequest {
    id: Option<u64>,
    method: String,
    params: Value,
    protocol: String,
}
struct Request {
    wire: WireRequest,
    deadline: Instant,
    response: oneshot::Sender<Result<Value, RuntimeError>>,
}
enum Message {
    Request(Request),
}
struct ProcessState {
    closed: AtomicBool,
    shutdown: Notify,
    stopped: Notify,
    finished: AtomicBool,
    cleanup_failed: AtomicBool,
    identity: String,
    connection_scope: String,
    interactions: Option<McpInteractions>,
}
pub(super) struct StdioConnection {
    sender: mpsc::Sender<Message>,
    state: Arc<ProcessState>,
    next_id: AtomicU64,
    timeout: Duration,
    max_bytes: usize,
    pub(super) identity: String,
    pub(super) protocol: String,
}
impl StdioConnection {
    pub(super) fn interactions(&self) -> Option<&McpInteractions> {
        self.state.interactions.as_ref()
    }
    pub(super) async fn request(&self, method: &str, params: Value) -> Result<Value, RuntimeError> {
        let mut id = self.next_id.load(Ordering::Relaxed);
        loop {
            let next = id
                .checked_add(1)
                .ok_or_else(|| error("stdio request ID exhausted"))?;
            match self
                .next_id
                .compare_exchange_weak(id, next, Ordering::SeqCst, Ordering::Relaxed)
            {
                Ok(_) => break,
                Err(current) => id = current,
            }
        }
        self.send(Some(id), method, params).await
    }
    async fn send(
        &self,
        id: Option<u64>,
        method: &str,
        params: Value,
    ) -> Result<Value, RuntimeError> {
        if self.state.closed.load(Ordering::SeqCst) {
            return Err(error("stdio session is closed"));
        }
        let wire = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        if serde_json::to_vec(&wire)
            .map_err(|_| error("invalid stdio request"))?
            .len()
            > self.max_bytes
        {
            return Err(error("stdio request exceeds byte limit"));
        }
        let deadline = Instant::now() + self.timeout;
        let (response, receiver) = oneshot::channel();
        self.sender
            .try_send(Message::Request(Request {
                wire: WireRequest {
                    id,
                    method: method.into(),
                    params,
                    protocol: self.protocol.clone(),
                },
                deadline,
                response,
            }))
            .map_err(|_| error("stdio queue full or session closed; no dispatch admitted"))?;
        // Dropping this future closes the oneshot receiver. The actor observes
        // that cancellation and permanently closes the process before reuse.
        tokio::time::timeout_at(deadline, receiver)
            .await
            .map_err(|_| error("stdio request deadline exceeded; no automatic retry"))?
            .map_err(|_| error("stdio session closed; request outcome may be unknown"))?
    }
    pub(super) async fn close(&self) -> Result<(), RuntimeError> {
        let stopped = self.state.stopped.notified();
        tokio::pin!(stopped);
        stopped.as_mut().enable();
        if self.state.finished.load(Ordering::SeqCst) {
            return self.cleanup_result();
        }
        self.state.closed.store(true, Ordering::SeqCst);
        self.state.shutdown.notify_one();
        tokio::time::timeout(Duration::from_secs(6), stopped)
            .await
            .map_err(|_| error("stdio teardown deadline exceeded"))?;
        self.cleanup_result()
    }
    fn cleanup_result(&self) -> Result<(), RuntimeError> {
        if self.state.cleanup_failed.load(Ordering::SeqCst) {
            Err(error(
                "stdio owned child could not be reaped within teardown bound",
            ))
        } else {
            Ok(())
        }
    }
}

pub(super) async fn connect(
    config: McpStdioConfig,
    interactions: Option<McpInteractions>,
) -> Result<(StdioConnection, BTreeMap<String, McpToolInfo>, bool), RuntimeError> {
    if !config.program.is_absolute()
        || !config.cwd.is_absolute()
        || config.timeout.is_zero()
        || config.timeout > Duration::from_secs(3600)
        || !(128..=16 * 1024 * 1024).contains(&config.max_message_bytes)
        || !(1..=1024).contains(&config.max_tools)
        || !(1..=64).contains(&config.max_pending_requests)
        || config.namespace.trim().is_empty()
        || config.namespace.len() > 256
        || config.args.len() > 128
        || config.args.iter().map(String::len).sum::<usize>() > 65536
        || config.args.iter().any(|v| v.contains('\0'))
        || config.environment.len() > 64
        || config.environment.iter().any(|v| {
            v.is_empty()
                || v.len() > 256
                || !v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                || v.as_bytes()[0].is_ascii_digit()
        })
    {
        return Err(error("invalid explicit stdio process configuration"));
    }
    let program = std::fs::canonicalize(&config.program)
        .map_err(|_| error("stdio executable is unavailable"))?;
    let cwd = std::fs::canonicalize(&config.cwd)
        .map_err(|_| error("stdio working directory is unavailable"))?;
    if !program.is_file()
        || !cwd.is_dir()
        || (cfg!(windows)
            && program
                .extension()
                .and_then(|v| v.to_str())
                .is_none_or(|v| !v.eq_ignore_ascii_case("exe")))
    {
        return Err(error("stdio executable/cwd type mismatch"));
    }
    let identity = hex::encode(Sha256::digest(
        serde_json::to_vec(&(
            &program,
            &config.args,
            &cwd,
            &config.environment,
            &config.namespace,
            config.max_message_bytes,
            config.max_tools,
            config.max_pending_requests,
            config.timeout.as_nanos(),
        ))
        .map_err(|_| error("cannot fingerprint stdio configuration"))?,
    ));
    let identity = match &interactions {
        None => identity,
        Some(host) => hex::encode(Sha256::digest(
            serde_json::to_vec(&(&identity, host.identity()))
                .map_err(|_| error("cannot fingerprint stdio host interactions"))?,
        )),
    };
    let capabilities = interactions
        .as_ref()
        .map_or_else(|| json!({}), McpInteractions::capabilities);
    let mut command = Command::new(program);
    command
        .args(&config.args)
        .current_dir(cwd)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    for name in &config.environment {
        let value = std::env::var_os(name)
            .ok_or_else(|| error("stdio allowlisted environment value is missing"))?;
        command.env(name, value);
    }
    let mut child = command
        .spawn()
        .map_err(|_| error("cannot start configured stdio subprocess"))?;
    let input = child
        .stdin
        .take()
        .ok_or_else(|| error("stdio input unavailable"))?;
    let output = child
        .stdout
        .take()
        .ok_or_else(|| error("stdio output unavailable"))?;
    let (sender, receiver) = mpsc::channel(config.max_pending_requests);
    let state = Arc::new(ProcessState {
        closed: AtomicBool::new(false),
        shutdown: Notify::new(),
        stopped: Notify::new(),
        finished: AtomicBool::new(false),
        cleanup_failed: AtomicBool::new(false),
        identity: identity.clone(),
        connection_scope: uuid::Uuid::new_v4().simple().to_string(),
        interactions,
    });
    tokio::spawn(actor(
        child,
        input,
        output,
        receiver,
        state.clone(),
        config.max_message_bytes,
    ));
    let mut connection = StdioConnection {
        sender,
        state,
        next_id: AtomicU64::new(1),
        timeout: config.timeout,
        max_bytes: config.max_message_bytes,
        identity,
        protocol: String::new(),
    };
    let initialized=connection.request("initialize",json!({"protocolVersion":PROTOCOLS[0],"capabilities":capabilities,"clientInfo":{"name":"taskloom","version":env!("CARGO_PKG_VERSION")}})).await?;
    connection.protocol = initialized
        .get("protocolVersion")
        .and_then(Value::as_str)
        .filter(|v| PROTOCOLS.contains(v))
        .ok_or_else(|| error("unsupported stdio protocol version"))?
        .into();
    let capabilities = initialized
        .get("capabilities")
        .and_then(Value::as_object)
        .ok_or_else(|| error("invalid stdio capabilities"))?;
    let resources = capabilities.get("resources").is_some_and(Value::is_object);
    if !capabilities.get("tools").is_some_and(Value::is_object) {
        return Err(error("stdio server has no tools capability"));
    }
    connection
        .send(None, "notifications/initialized", json!({}))
        .await?;
    let mut tools = BTreeMap::new();
    let mut cursor = None;
    let mut cursors = BTreeSet::new();
    for _ in 0..16 {
        let params = cursor
            .as_ref()
            .map_or_else(|| json!({}), |v| json!({"cursor":v}));
        let page = connection.request("tools/list", params).await?;
        let listed = page
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| error("invalid stdio tools/list"))?;
        if tools.len().saturating_add(listed.len()) > config.max_tools {
            return Err(error("stdio tool catalog exceeds limit"));
        }
        for value in listed {
            let info: McpToolInfo = serde_json::from_value(value.clone())
                .map_err(|_| error("malformed stdio tool definition"))?;
            if info.name.trim().is_empty()
                || info.name.len() > 256
                || info.description.len() > 16384
                || !info.input_schema.is_object()
                || tools.insert(info.name.clone(), info).is_some()
            {
                return Err(error("invalid or duplicate stdio tool"));
            }
        }
        cursor = match page.get("nextCursor") {
            None | Some(Value::Null) => None,
            Some(Value::String(v)) if !v.is_empty() && v.len() <= 4096 => Some(v.clone()),
            _ => return Err(error("invalid stdio tool cursor")),
        };
        match &cursor {
            None => return Ok((connection, tools, resources)),
            Some(v) if !cursors.insert(v.clone()) => {
                return Err(error("repeated stdio tool cursor"))
            }
            _ => {}
        }
    }
    Err(error("stdio tool pagination limit exceeded"))
}

async fn stop(child: &mut Child) -> Result<(), RuntimeError> {
    let _ = child.start_kill();
    tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .map_err(|_| error("stdio child reaping timeout"))?
        .map_err(|_| error("stdio child reaping failed"))?;
    Ok(())
}
async fn actor(
    mut child: Child,
    mut input: ChildStdin,
    output: ChildStdout,
    mut receiver: mpsc::Receiver<Message>,
    state: Arc<ProcessState>,
    max_bytes: usize,
) {
    let mut output = BufReader::new(output);
    loop {
        let message = tokio::select! {_ = state.shutdown.notified()=>break,value=receiver.recv()=>match value{Some(v)=>v,None=>break}};
        match message {
            Message::Request(request) => {
                if state.closed.load(Ordering::SeqCst)
                    || request.response.is_closed()
                    || Instant::now() >= request.deadline
                {
                    let _ = request.response.send(Err(error(
                        "stdio request cancelled/expired before dispatch",
                    )));
                    continue;
                }
                let mut response = request.response;
                let result = tokio::select! {
                    _=state.shutdown.notified()=>Err(error("stdio session explicitly closed; dispatched outcome may be unknown")),
                    _=response.closed()=>Err(error("stdio caller cancelled; dispatched outcome may be unknown")),
                    value=tokio::time::timeout_at(request.deadline, exchange(&mut input,&mut output,&request.wire,&state,max_bytes))=>value.unwrap_or_else(|_|Err(error("stdio request deadline exceeded; outcome may be unknown"))),
                };
                let terminal = result.is_err();
                let _ = response.send(result);
                if terminal {
                    break;
                }
            }
        }
    }
    state.closed.store(true, Ordering::SeqCst);
    receiver.close();
    state
        .cleanup_failed
        .store(stop(&mut child).await.is_err(), Ordering::SeqCst);
    state.finished.store(true, Ordering::SeqCst);
    state.stopped.notify_waiters();
}
async fn exchange(
    input: &mut ChildStdin,
    output: &mut BufReader<ChildStdout>,
    request: &WireRequest,
    state: &ProcessState,
    max_bytes: usize,
) -> Result<Value, RuntimeError> {
    let id = request.id;
    let mut wire = json!({"jsonrpc":"2.0","method":request.method,"params":request.params});
    if let Some(id) = id {
        wire["id"] = json!(id);
    }
    let mut bytes = serde_json::to_vec(&wire).map_err(|_| error("invalid stdio JSON"))?;
    bytes.push(b'\n');
    input
        .write_all(&bytes)
        .await
        .map_err(|_| error("stdio write failed; outcome may be unknown"))?;
    input
        .flush()
        .await
        .map_err(|_| error("stdio flush failed; outcome may be unknown"))?;
    let Some(id) = id else {
        return Ok(json!({}));
    };
    let mut total = 0usize;
    loop {
        let mut line = Vec::new();
        loop {
            let byte = output.read_u8().await.map_err(|_| {
                error("stdio closed before matching response; outcome may be unknown")
            })?;
            total += 1;
            if total > max_bytes {
                return Err(error("stdio response exceeds byte limit"));
            }
            if byte == b'\n' {
                break;
            }
            line.push(byte);
        }
        let message: Value =
            serde_json::from_slice(&line).map_err(|_| error("malformed stdio JSON-RPC frame"))?;
        if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(error("invalid stdio JSON-RPC version"));
        }
        if message.get("id").is_none() {
            if !message.get("method").is_some_and(Value::is_string) {
                return Err(error("invalid stdio notification"));
            }
            continue;
        }
        if message.get("method").is_some() {
            if request.method != "tools/call" || request.protocol.is_empty() {
                return Err(error("server host interaction outside explicit tool call"));
            }
            let host = state.interactions.as_ref().ok_or_else(|| {
                error("server requested host interaction without explicit opt-in")
            })?;
            let scope = crate::runtime::state::fingerprint(&(
                &state.identity,
                &state.connection_scope,
                request.id,
                &request.method,
                &request.params,
            ))?;
            let response = host.handle(&scope, &request.protocol, &message).await?;
            let mut bytes = serde_json::to_vec(&response)
                .map_err(|_| error("cannot encode host interaction reply"))?;
            if bytes.len().saturating_add(1) > max_bytes {
                return Err(error("host interaction reply exceeds transport limit"));
            }
            bytes.push(b'\n');
            input
                .write_all(&bytes)
                .await
                .map_err(|_| error("stdio host reply write failed; no automatic retry"))?;
            input
                .flush()
                .await
                .map_err(|_| error("stdio host reply flush failed"))?;
            continue;
        }
        if message.get("id").and_then(Value::as_u64) != Some(id) {
            return Err(error("stdio response ID mismatch"));
        }
        if message.get("error").is_some() {
            return Err(error(
                "stdio server returned JSON-RPC error; no automatic retry",
            ));
        }
        return message
            .get("result")
            .filter(|v| v.is_object())
            .cloned()
            .ok_or_else(|| error("stdio response has no object result"));
    }
}
