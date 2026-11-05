//! Server mode for gewu-server.
//!
//! Lets other programs connect to a running GeWu agent, send prompts, and
//! receive streamed agent events. Supports two transports:
//!
//! - **WebSocket** (port 8420): text frames, one JSON message per frame.
//! - **TCP** (port 8421): newline-delimited JSON, one message per line.
//!
//! Both transports share the same protocol (see [`protocol`]) and the same
//! session model: each connection gets an agent created lazily on its first
//! session-needing request and reused for the life of the connection, so
//! conversation context persists across requests on that connection.
//! Clients may pass an explicit `session_id` (e.g. one produced by a
//! previous CLI run) to resume an existing conversation. A later request
//! naming a *different* `session_id`/`agent` is rejected instead of silently
//! ignored.
//!
//! One `ask` runs at a time per connection — and per session across
//! connections: a session busy with an ask on one connection rejects asks
//! from other connections. While an ask is running the connection can still
//! send `interrupt`, which aborts the running task, repairs the
//! conversation history (dropping the unanswered prompt), and keeps it
//! consistent.
//!
//! When started with a token, every connection must send an `auth` request
//! before any other operation. TCP lines longer than [`MAX_LINE_BYTES`] and
//! lines that are not valid UTF-8 are rejected with an error frame; the
//! connection stays alive.

use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use gewu_core::ToolRegistry;
use gewu_core::types::{AgentEvent, Message};
pub use gewu_protocol as protocol;
use gewu_protocol::FileEntry;
use registry::create_agent;
use serde_json::Value;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc};
use tokio::task::{JoinHandle, JoinSet};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{info, warn};

use crate::bootstrap::Bootstrap;
use crate::registry;
pub use protocol::{ClientRequest, Op, RequestError, ServerMessage};

/// Default bind address for both listeners.
pub const DEFAULT_HOST: &str = "127.0.0.1";
/// Default port for the WebSocket listener.
pub const DEFAULT_WS_PORT: u16 = 8420;
/// Default port for the TCP listener.
pub const DEFAULT_TCP_PORT: u16 = 8421;

/// Maximum accepted inbound TCP line length. Larger lines (and lines that
/// never terminate within this budget) are rejected with an error frame
/// instead of being buffered without bound.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

/// Default agent profile used when the client does not specify one.
const DEFAULT_AGENT: &str = "build";

/// Shared server context handed to each connection handler.
struct ServerState {
    bootstrap: Bootstrap,
    /// When set, connections must authenticate with an `auth` request
    /// carrying this token before any other operation.
    token: Option<String>,
    /// Session ids with an ask currently running, across all connections.
    /// Guards one conversation from concurrent writes by two connections.
    busy_sessions: Mutex<HashSet<String>>,
}

/// Per-connection session: one agent plus its identity.
#[derive(Clone)]
struct ConnectionSession {
    agent: Arc<Mutex<gewu_core::Agent>>,
    session_id: String,
    agent_name: String,
    /// Cached tool names, taken from the agent at creation. The tool set is
    /// fixed for the life of the session, so `list_tools` can be answered
    /// without locking the agent (which would block while an ask runs).
    tools: Vec<String>,
}

/// A currently running ask on a connection.
struct RunningAsk {
    request_id: String,
    /// Session claimed in `ServerState::busy_sessions` for this ask.
    session_id: String,
    handle: JoinHandle<()>,
}

/// Run the server until shutdown is requested or Ctrl+C is received.
pub async fn serve(
    bootstrap: Bootstrap,
    host: &str,
    ws_port: u16,
    tcp_port: u16,
    token: Option<String>,
) -> Result<()> {
    let ws_addr: SocketAddr = format!("{}:{}", host, ws_port)
        .parse()
        .context("Invalid WebSocket bind address")?;
    let tcp_addr: SocketAddr = format!("{}:{}", host, tcp_port)
        .parse()
        .context("Invalid TCP bind address")?;

    let ws_listener = TcpListener::bind(ws_addr)
        .await
        .with_context(|| format!("Failed to bind WebSocket listener on {}", ws_addr))?;
    let tcp_listener = TcpListener::bind(tcp_addr)
        .await
        .with_context(|| format!("Failed to bind TCP listener on {}", tcp_addr))?;

    let state = Arc::new(ServerState {
        bootstrap,
        token,
        busy_sessions: Mutex::new(HashSet::new()),
    });

    info!("gewu server listening:");
    info!("  WebSocket: ws://{}", ws_addr);
    info!("  TCP (newline-delimited JSON): {}", tcp_addr);
    if state.token.is_some() {
        info!("authentication required (clients send the 'auth' op first)");
    }

    // Per-connection handlers are spawned into a shared tracking set so
    // shutdown can abort and drain them together with the listeners.
    let mut connections: JoinSet<()> = JoinSet::new();
    let (conn_tx, mut conn_rx) = mpsc::unbounded_channel::<JoinHandle<()>>();

    let mut tasks = JoinSet::new();
    tasks.spawn(accept_loop(
        ws_listener,
        state.clone(),
        Transport::WebSocket,
        conn_tx.clone(),
    ));
    tasks.spawn(accept_loop(
        tcp_listener,
        state.clone(),
        Transport::Tcp,
        conn_tx.clone(),
    ));
    drop(conn_tx); // accept loops hold the remaining senders

    // Run until Ctrl+C (or until both accept loops exit). Connection
    // handles arriving from the accept loops are moved into the tracking
    // set as they are created.
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    loop {
        tokio::select! {
            result = &mut ctrl_c => {
                result.context("Failed to listen for Ctrl+C")?;
                info!("Received Ctrl+C, shutting down");
                break;
            }
            handle = conn_rx.recv() => {
                match handle {
                    Some(handle) => {
                        connections.spawn(async move {
                            let _ = handle.await;
                        });
                    }
                    // Both accept loops are gone; nothing left to serve.
                    None => break,
                }
            }
        }
    }

    // Abort listeners first (no new connections), then abort and drain every
    // tracked connection task so nothing outlives the shutdown.
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    Ok(())
}

/// The transport a connection came in on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Transport {
    WebSocket,
    Tcp,
}

/// Accept connections on one listener forever, forwarding each connection
/// task's handle to the tracker.
async fn accept_loop(
    listener: TcpListener,
    state: Arc<ServerState>,
    transport: Transport,
    conn_tx: mpsc::UnboundedSender<JoinHandle<()>>,
) {
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                let state = state.clone();
                let handle = tokio::spawn(async move {
                    let result = match transport {
                        Transport::WebSocket => handle_ws_connection(stream, peer, state).await,
                        Transport::Tcp => handle_tcp_connection(stream, peer, state).await,
                    };
                    if let Err(e) = result {
                        warn!("Connection {} error: {}", peer, e);
                    }
                });
                let _ = conn_tx.send(handle);
            }
            Err(e) => {
                warn!("Accept failed: {}", e);
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

impl ServerState {
    /// Get or create the per-connection session.
    ///
    /// `session_id` and `agent_name` only apply when the session is first
    /// created; subsequent calls on the same connection reuse it. A later
    /// request naming a *different* `session_id` or `agent` is rejected so
    /// mismatches surface as errors instead of being silently ignored.
    async fn get_or_create_session(
        &self,
        current: &mut Option<ConnectionSession>,
        session_id: Option<String>,
        agent_name: Option<String>,
    ) -> Result<ConnectionSession> {
        if let Some(existing) = current {
            if let Some(requested) = &session_id
                && requested != &existing.session_id
            {
                anyhow::bail!(
                    "session mismatch: this connection is bound to session '{}'; \
                     requested '{}'. Open a new connection to use another session.",
                    existing.session_id,
                    requested
                );
            }
            if let Some(requested) = &agent_name
                && requested != &existing.agent_name
            {
                anyhow::bail!(
                    "agent mismatch: this connection is bound to agent '{}'; \
                     requested '{}'. Open a new connection to use another agent.",
                    existing.agent_name,
                    requested
                );
            }
            return Ok(existing.clone());
        }

        let agent_name = agent_name.unwrap_or_else(|| DEFAULT_AGENT.to_string());
        if !registry::list_agent_names().contains(&agent_name) {
            anyhow::bail!("Unknown agent profile: {}", agent_name);
        }

        let session_id = session_id.unwrap_or_else(crate::bootstrap::new_session_id);
        let (agent, _registry) = create_agent(
            &agent_name,
            self.bootstrap.provider.clone(),
            self.bootstrap.pool.clone(),
            session_id.clone(),
            self.bootstrap.mcp_tools.clone(),
            self.bootstrap.log_writer.clone(),
            self.bootstrap.skills.clone(),
        )
        .await?;
        info!("Created session {} (agent: {})", session_id, agent_name);

        let agent = Arc::new(Mutex::new(agent));
        // Snapshot the tool list once; it never changes for this session.
        let tools = agent.lock().await.list_tools();

        let entry = ConnectionSession {
            agent,
            session_id,
            agent_name,
            tools,
        };
        *current = Some(entry.clone());
        Ok(entry)
    }

    /// Claim a session for an ask. Returns `false` when another connection
    /// is already running an ask on that session.
    async fn claim_session(&self, session_id: &str) -> bool {
        self.busy_sessions
            .lock()
            .await
            .insert(session_id.to_string())
    }

    /// Release a session claim after its ask finished, failed, or was
    /// interrupted. Idempotent.
    async fn release_session(&self, session_id: &str) {
        self.busy_sessions.lock().await.remove(session_id);
    }
}

/// Append synthetic tool results for assistant tool calls that never got a
/// result (e.g. after an interrupt aborted the ask mid-tool-execution).
///
/// Without this, the next LLM request would contain an assistant message
/// with tool calls but no matching tool results, which providers reject.
async fn patch_dangling_tool_calls(agent: &mut gewu_core::Agent) {
    let messages = agent.memory.messages();
    let Some(last_assistant_index) = messages.iter().rposition(
        |m| matches!(m, Message::Assistant { tool_calls: Some(calls), .. } if !calls.is_empty()),
    ) else {
        return;
    };

    let pending_ids: Vec<(String, Option<String>)> = match &messages[last_assistant_index] {
        Message::Assistant {
            tool_calls: Some(calls),
            ..
        } => calls
            .iter()
            .map(|c| (c.id.clone(), Some(c.function.name.clone())))
            .collect(),
        _ => return,
    };

    let answered: std::collections::HashSet<String> = messages[last_assistant_index + 1..]
        .iter()
        .filter_map(|m| match m {
            Message::Tool { tool_call_id, .. } => Some(tool_call_id.clone()),
            _ => None,
        })
        .collect();

    let missing: Vec<(String, Option<String>)> = pending_ids
        .into_iter()
        .filter(|(id, _)| !answered.contains(id))
        .collect();
    if missing.is_empty() {
        return;
    }

    for (tool_call_id, tool_name) in missing {
        agent
            .memory
            .add_message(Message::Tool {
                tool_call_id,
                tool_name,
                content: "Interrupted by user before the tool result was recorded.".to_string(),
                media: Vec::new(),
            })
            .await;
    }
}

/// Drop the user prompt of an aborted ask when it produced no output.
///
/// If an ask is interrupted before the model responded, the only trace of it
/// is the trailing user message the ask appended; leaving it in the history
/// would steer the next ask towards the cancelled instruction. When the
/// aborted exchange already produced assistant/tool output, the history is
/// kept (and only repaired by [`patch_dangling_tool_calls`]) so executed
/// work is not lost.
async fn drop_dangling_prompt(agent: &mut gewu_core::Agent) {
    if matches!(agent.memory.messages().last(), Some(Message::User { .. })) {
        let mut kept = agent.memory.messages().to_vec();
        kept.pop();
        agent.memory.replace(kept).await;
    }
}

// ---------------------------------------------------------------------------
// Request handling
// ---------------------------------------------------------------------------

/// What handling a request produced.
enum Handled {
    /// A single reply message.
    Reply(ServerMessage),
    /// An ask to spawn on the connection session.
    Ask {
        ready: ServerMessage,
        request_id: String,
        session_id: String,
        prompt: String,
    },
}

/// Handle one parsed client request, producing a reply or an ask to run.
///
/// An `interrupt` request aborts and awaits the running ask task here and
/// repairs the conversation history, guaranteeing the agent lock is
/// released before any subsequent request is handled.
async fn handle_request(
    state: &ServerState,
    session: &mut Option<ConnectionSession>,
    running: &mut Option<RunningAsk>,
    request: ClientRequest,
) -> Result<Handled, RequestError> {
    match request.op {
        Op::Ping => Ok(Handled::Reply(ServerMessage::Pong { id: request.id })),

        // Auth is normally consumed by the connection loop's auth gate; if a
        // client authenticates twice, accept it idempotently.
        Op::Auth { .. } => Ok(Handled::Reply(ServerMessage::Auth {
            id: request.id,
            ok: true,
        })),

        Op::Info => Ok(Handled::Reply(ServerMessage::Info {
            id: request.id,
            model: state.bootstrap.model.clone(),
            provider: state.bootstrap.provider_name.clone(),
            working_directory: state.bootstrap.working_directory.clone(),
        })),

        Op::Usage => {
            // Measure the conversation against the agent's context window —
            // the same estimate the compaction logic acts on. With no session
            // (and none requested) there is nothing to measure; report zeros.
            if session.is_none() && request.session_id.is_none() {
                return Ok(Handled::Reply(ServerMessage::Usage {
                    id: request.id,
                    used_tokens: 0,
                    context_limit: 0,
                    busy: false,
                }));
            }
            let conn = state
                .get_or_create_session(session, request.session_id, request.agent)
                .await
                .map_err(|e| RequestError::Failed(e.to_string()))?;
            // Never block on a running ask (the agent lock is held for the
            // whole ask): report "unknown" as zeros and flag the reply busy
            // so clients can tell this apart from "no session yet".
            let Ok(agent) = conn.agent.try_lock() else {
                return Ok(Handled::Reply(ServerMessage::Usage {
                    id: request.id,
                    used_tokens: 0,
                    context_limit: 0,
                    busy: true,
                }));
            };
            let used_tokens: usize = agent
                .memory
                .messages()
                .iter()
                .map(gewu_core::compaction::estimate_message_tokens)
                .sum();
            Ok(Handled::Reply(ServerMessage::Usage {
                id: request.id,
                used_tokens: used_tokens as u64,
                context_limit: agent.context_limit as u64,
                busy: false,
            }))
        }

        Op::ListAgents => {
            let agents = registry::list_available_agents()
                .into_iter()
                .map(|a| a.name)
                .collect();
            Ok(Handled::Reply(ServerMessage::Agents {
                id: request.id,
                agents,
            }))
        }

        Op::ListTools => {
            let session = state
                .get_or_create_session(session, request.session_id, request.agent)
                .await
                .map_err(|e| RequestError::Failed(e.to_string()))?;
            // Answer from the snapshot taken at session creation; this
            // avoids locking the agent, which would block while an ask is
            // running on this connection.
            Ok(Handled::Reply(ServerMessage::Tools {
                id: request.id,
                tools: session.tools,
            }))
        }

        Op::ListFiles { path } => {
            let root = &state.bootstrap.working_directory;
            let dir = gewu_tools::fs_utils::resolve_safe_path(root, &path)
                .map_err(|e| RequestError::Invalid(e.to_string()))?;
            let canonical_root = std::fs::canonicalize(root).map_err(|e| {
                RequestError::Failed(format!("cannot resolve working directory: {e}"))
            })?;
            let canonical_dir = std::fs::canonicalize(&dir)
                .map_err(|e| RequestError::Failed(format!("cannot read directory: {e}")))?;
            if !canonical_dir.starts_with(&canonical_root) {
                return Err(RequestError::Invalid(
                    "directory is outside the working directory".into(),
                ));
            }
            if !canonical_dir.is_dir() {
                return Err(RequestError::Invalid("path is not a directory".into()));
            }
            let mut entries = Vec::new();
            for entry in ignore::WalkBuilder::new(&canonical_dir)
                .max_depth(Some(1))
                .hidden(true)
                .git_ignore(true)
                .require_git(false)
                .build()
                .skip(1)
            {
                let entry = entry.map_err(|e| RequestError::Failed(e.to_string()))?;
                let Some(name) = entry.file_name().to_str() else {
                    continue;
                };
                let is_dir = entry.file_type().is_some_and(|kind| kind.is_dir());
                entries.push(FileEntry {
                    name: name.to_string(),
                    is_dir,
                });
            }
            entries.sort_by(|a, b| {
                b.is_dir
                    .cmp(&a.is_dir)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
            Ok(Handled::Reply(ServerMessage::Files {
                id: request.id,
                path,
                entries,
            }))
        }

        Op::ReadFile { path } => {
            const MAX_PREVIEW_BYTES: usize = 512 * 1024;
            let root = &state.bootstrap.working_directory;
            let file = gewu_tools::fs_utils::resolve_safe_path(root, &path)
                .map_err(|e| RequestError::Invalid(e.to_string()))?;
            let canonical_root = std::fs::canonicalize(root).map_err(|e| {
                RequestError::Failed(format!("cannot resolve working directory: {e}"))
            })?;
            let canonical_file = std::fs::canonicalize(&file)
                .map_err(|e| RequestError::Failed(format!("cannot read file: {e}")))?;
            if !canonical_file.starts_with(&canonical_root) {
                return Err(RequestError::Invalid(
                    "file is outside the working directory".into(),
                ));
            }
            let metadata = std::fs::metadata(&canonical_file)
                .map_err(|e| RequestError::Failed(format!("cannot read file: {e}")))?;
            if !metadata.is_file() {
                return Err(RequestError::Invalid("path is not a file".into()));
            }
            let file = std::fs::File::open(&canonical_file)
                .map_err(|e| RequestError::Failed(format!("cannot read file: {e}")))?;
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(
                &mut std::io::Read::take(file, (MAX_PREVIEW_BYTES + 1) as u64),
                &mut bytes,
            )
            .map_err(|e| RequestError::Failed(format!("cannot read file: {e}")))?;
            let truncated = bytes.len() > MAX_PREVIEW_BYTES;
            bytes.truncate(MAX_PREVIEW_BYTES);
            if bytes.iter().take(8192).any(|byte| *byte == 0) {
                return Err(RequestError::Failed(
                    "binary files cannot be previewed".into(),
                ));
            }
            if truncated {
                while let Err(error) = std::str::from_utf8(&bytes) {
                    if error.error_len().is_some() {
                        break;
                    }
                    bytes.truncate(error.valid_up_to());
                }
            }
            let content = String::from_utf8(bytes)
                .map_err(|_| RequestError::Failed("file is not valid UTF-8 text".into()))?;
            Ok(Handled::Reply(ServerMessage::FileContent {
                id: request.id,
                path,
                content,
                truncated,
            }))
        }

        Op::ListSessions => {
            let sessions = gewu_storage::list_sessions(&state.bootstrap.pool)
                .await
                .map_err(|e| RequestError::Failed(e.to_string()))?;
            Ok(Handled::Reply(ServerMessage::Sessions {
                id: request.id,
                sessions,
            }))
        }

        Op::GetSession => {
            let Some(session_id) = request.session_id else {
                return Err(RequestError::Invalid(
                    "op 'get_session' requires a 'session_id' string".to_string(),
                ));
            };
            let stored = gewu_storage::load_session(&state.bootstrap.pool, &session_id)
                .await
                .map_err(|e| RequestError::Failed(e.to_string()))?;
            Ok(Handled::Reply(ServerMessage::Session {
                id: request.id,
                session_id,
                messages: stored.into_iter().map(|stored| stored.message).collect(),
            }))
        }

        Op::DeleteSession => {
            let Some(session_id) = request.session_id else {
                return Err(RequestError::Invalid(
                    "op 'delete_session' requires a 'session_id' string".to_string(),
                ));
            };
            gewu_storage::delete_session(&state.bootstrap.pool, &session_id)
                .await
                .map_err(|e| RequestError::Failed(e.to_string()))?;
            info!("Deleted session {}", session_id);
            Ok(Handled::Reply(ServerMessage::Deleted {
                id: request.id,
                session_id,
            }))
        }

        Op::Interrupt => {
            let request_id = running.as_ref().map(|r| r.request_id.clone());
            if let Some(running) = running.take() {
                abort_ask(running.handle).await;
                // Release the cross-connection claim on this session.
                state.release_session(&running.session_id).await;
                // Repair the conversation history so the next LLM request is
                // well-formed even though the ask was cut short, and drop a
                // prompt that produced no output so it cannot steer the next
                // ask.
                if let Some(session) = session {
                    let mut agent = session.agent.lock().await;
                    patch_dangling_tool_calls(&mut agent).await;
                    drop_dangling_prompt(&mut agent).await;
                }
            }
            Ok(Handled::Reply(ServerMessage::Interrupted {
                id: Some(request.id),
                request_id,
            }))
        }

        Op::Ask { prompt } => {
            if running.is_some() {
                return Err(RequestError::Failed(
                    "an ask is already in progress; send 'interrupt' first".to_string(),
                ));
            }
            let session = state
                .get_or_create_session(session, request.session_id, request.agent)
                .await
                .map_err(|e| RequestError::Failed(e.to_string()))?;
            // Claim the session across connections so two connections cannot
            // interleave writes into one conversation.
            if !state.claim_session(&session.session_id).await {
                return Err(RequestError::Failed(format!(
                    "session '{}' is busy with an ask on another connection",
                    session.session_id
                )));
            }
            let request_id = crate::bootstrap::new_session_id();
            Ok(Handled::Ask {
                ready: ServerMessage::Ready {
                    id: request.id,
                    request_id: request_id.clone(),
                    session_id: session.session_id.clone(),
                    agent: session.agent_name.clone(),
                },
                request_id,
                session_id: session.session_id.clone(),
                prompt,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Ask execution
// ---------------------------------------------------------------------------

/// Execute one ask, streaming protocol lines through `emit`.
///
/// Runs inside a spawned task while holding the agent lock. Emits `result`
/// as its final message on success, or an `error` message on failure.
async fn execute_ask<E, Fut>(
    session: &ConnectionSession,
    request_id: &str,
    prompt: &str,
    mut emit: E,
) where
    E: FnMut(ServerMessage) -> Fut,
    Fut: std::future::Future<Output = Result<(), anyhow::Error>>,
{
    let mut agent = session.agent.lock().await;
    // Repair any history left inconsistent by a previous interrupt.
    patch_dangling_tool_calls(&mut agent).await;

    let mut stream = Box::pin(agent.ask(prompt));
    // Whether a terminal frame (`result` or `error`) has been sent. Only
    // the `Finish` event and stream errors are terminal; an `AgentEvent::Error`
    // can be mid-stream (e.g. one failed tool among several).
    let mut terminal_sent = false;
    while let Some(event_result) = stream.next().await {
        let message = match event_result {
            Ok(AgentEvent::Finish { response }) => {
                terminal_sent = true;
                ServerMessage::Result {
                    id: Value::Null,
                    request_id: request_id.to_string(),
                    session_id: session.session_id.clone(),
                    response: response.clone(),
                }
            }
            Ok(event) => ServerMessage::Event {
                id: Value::Null,
                request_id: request_id.to_string(),
                event: protocol::agent_event_to_value(&event),
            },
            Err(e) => {
                terminal_sent = true;
                ServerMessage::Error {
                    id: None,
                    error: e.to_string(),
                }
            }
        };
        if emit(message).await.is_err() {
            return; // client went away; drop the stream and release the lock
        }
    }

    // The agent stream ended without a terminal event (e.g. max turns
    // reached or a doom loop produced a final `AgentEvent::Error`). Emit a
    // terminal error so the client's ask completes instead of hanging
    // forever.
    if !terminal_sent {
        let _ = emit(ServerMessage::Error {
            id: None,
            error: "ask ended without a final response".to_string(),
        })
        .await;
    }
}

/// Spawn an ask task that emits protocol lines through `out`.
///
/// All failures are reported to the client as `error` messages by
/// `execute_ask`; the task itself always completes normally.
fn spawn_ask(
    session: ConnectionSession,
    request_id: String,
    prompt: String,
    out: mpsc::Sender<String>,
    done: mpsc::UnboundedSender<()>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        execute_ask(&session, &request_id, &prompt, |msg| {
            let out = out.clone();
            async move { out.send(msg.to_line()).await.map_err(anyhow::Error::from) }
        })
        .await;
        let _ = done.send(());
    })
}

/// Abort a running ask and await its task.
///
/// Awaiting the aborted handle guarantees the agent lock has been released
/// before the caller proceeds to patch memory / start a new ask.
async fn abort_ask(handle: JoinHandle<()>) {
    handle.abort();
    let _ = handle.await;
}

// ---------------------------------------------------------------------------
// Connection loop
// ---------------------------------------------------------------------------

/// Shared request/response loop for both transports.
///
/// `incoming` yields raw client message strings; `out` serializes server
/// messages for delivery. Ask tasks run in the background and emit
/// directly into `out`, so this loop stays free to process `interrupt`.
async fn drive_connection(
    state: Arc<ServerState>,
    mut incoming: mpsc::Receiver<String>,
    out: mpsc::Sender<String>,
) -> Result<()> {
    let mut session: Option<ConnectionSession> = None;
    let mut running: Option<RunningAsk> = None;
    // Token-protected servers require an `auth` request first.
    let mut authenticated = state.token.is_none();
    // Completion signals from ask tasks. The loop-side sender keeps this
    // channel open when no ask is running, so recv() simply stays pending.
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<()>();

    let result = loop {
        tokio::select! {
            incoming = incoming.recv() => {
                let Some(raw) = incoming else { break Ok(()) };
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    continue;
                }

                let request = match protocol::parse_request(trimmed) {
                    Ok(r) => r,
                    Err(RequestError::Invalid(msg)) | Err(RequestError::Failed(msg)) => {
                        // Echo the client's id when it can be extracted so
                        // the client can correlate the failure.
                        let id = protocol::extract_id(trimmed);
                        out.send(ServerMessage::Error { id, error: msg }.to_line())
                            .await?;
                        continue;
                    }
                };

                // Remember the correlation id so error replies can echo it.
                let request_id_value = request.id.clone();

                // Gate every operation behind authentication when the server
                // runs with a token.
                if !authenticated {
                    match request.op {
                        Op::Auth { token } => {
                            // Deliberately naive comparison: the threat model
                            // is casual remote access, not timing attacks.
                            let ok = state.token.as_deref() == Some(token.as_str());
                            out.send(
                                ServerMessage::Auth { id: request.id, ok }.to_line(),
                            )
                            .await?;
                            if ok {
                                info!("client authenticated");
                                authenticated = true;
                            } else {
                                warn!("client failed authentication; closing connection");
                                break Ok(());
                            }
                        }
                        _ => {
                            out.send(
                                ServerMessage::Error {
                                    id: Some(request_id_value),
                                    error: "authentication required: send {\"op\":\"auth\",\"token\":\"...\"} first"
                                        .to_string(),
                                }
                                .to_line(),
                            )
                            .await?;
                        }
                    }
                    continue;
                }

                // An interrupt request aborts + awaits the running ask inside
                // handle_request, so the agent lock is released before any
                // further request is handled.
                match handle_request(&state, &mut session, &mut running, request).await {
                    Ok(Handled::Reply(msg)) => {
                        out.send(msg.to_line()).await?;
                    }
                    Ok(Handled::Ask { ready, request_id, session_id, prompt }) => {
                        out.send(ready.to_line()).await?;
                        let session = session
                            .clone()
                            .expect("session created by handle_request");
                        running = Some(RunningAsk {
                            request_id: request_id.clone(),
                            session_id,
                            handle: spawn_ask(session, request_id, prompt, out.clone(), done_tx.clone()),
                        });
                    }
                    Err(RequestError::Invalid(msg)) | Err(RequestError::Failed(msg)) => {
                        out.send(
                            ServerMessage::Error { id: Some(request_id_value), error: msg }
                                .to_line(),
                        )
                        .await?;
                    }
                }
            }
            _ = done_rx.recv() => {
                // The running ask finished on its own; release its claim.
                if let Some(running) = running.take() {
                    state.release_session(&running.session_id).await;
                }
            }
        }
    };

    if let Some(running) = running.take() {
        abort_ask(running.handle).await;
        state.release_session(&running.session_id).await;
    }
    result
}

// ---------------------------------------------------------------------------
// Transports
// ---------------------------------------------------------------------------

/// Handle a single WebSocket connection.
async fn handle_ws_connection(
    stream: TcpStream,
    peer: SocketAddr,
    state: Arc<ServerState>,
) -> Result<()> {
    // Cap inbound messages instead of relying on tungstenite's 64 MB default.
    let mut config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default();
    config.max_message_size = Some(MAX_LINE_BYTES);
    config.max_frame_size = Some(MAX_LINE_BYTES);
    let ws = tokio_tungstenite::accept_async_with_config(stream, Some(config))
        .await
        .context("WebSocket handshake failed")?;
    info!("WebSocket client connected: {}", peer);
    let (mut sink, mut ws_stream) = ws.split();

    let (out_tx, mut out_rx) = mpsc::channel::<String>(256);
    let (in_tx, in_rx) = mpsc::channel::<String>(64);

    // Writer task: drains `out_rx` into the WebSocket sink.
    let writer = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if sink.send(WsMessage::text(msg)).await.is_err() {
                break;
            }
        }
    });

    // Reader task: pushes incoming text frames into `in_tx`.
    let reader = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_stream.next().await {
            match msg {
                WsMessage::Text(text) => {
                    if in_tx.send(text.to_string()).await.is_err() {
                        break;
                    }
                }
                WsMessage::Close(_) => break,
                _ => {}
            }
        }
    });

    let result = drive_connection(state, in_rx, out_tx).await;

    drop(reader); // reader ends when the client disconnects
    let _ = writer.await;
    if let Err(e) = &result {
        warn!("WebSocket client {} error: {}", peer, e);
    } else {
        info!("WebSocket client disconnected: {}", peer);
    }
    result
}

/// Handle a single TCP connection (newline-delimited JSON).
async fn handle_tcp_connection(
    stream: TcpStream,
    peer: SocketAddr,
    state: Arc<ServerState>,
) -> Result<()> {
    info!("TCP client connected: {}", peer);
    let (reader_half, mut writer_half) = stream.into_split();

    let (out_tx, mut out_rx) = mpsc::channel::<String>(256);
    let (in_tx, in_rx) = mpsc::channel::<String>(64);

    // Writer task: drains `out_rx` into the TCP stream.
    let writer = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if writer_half.write_all(msg.as_bytes()).await.is_err()
                || writer_half.write_all(b"\n").await.is_err()
                || writer_half.flush().await.is_err()
            {
                break;
            }
        }
    });

    // Reader task: pushes validated lines into `in_tx`, answering protocol
    // violations (over-long lines, invalid UTF-8) with an error frame so
    // the client learns why its line was dropped and can keep talking.
    let err_tx = out_tx.clone();
    let reader = tokio::spawn(tcp_reader(reader_half, in_tx, err_tx));

    let result = drive_connection(state, in_rx, out_tx).await;

    reader.abort();
    let _ = writer.await;
    if let Err(e) = &result {
        warn!("TCP client {} error: {}", peer, e);
    } else {
        info!("TCP client disconnected: {}", peer);
    }
    result
}

/// Read newline-delimited lines from `stream`, enforcing [`MAX_LINE_BYTES`]
/// and UTF-8 validity.
///
/// Valid lines are forwarded on `in_tx`. Over-long lines and lines that are
/// not valid UTF-8 are answered with an error frame on `err_tx` and skipped;
/// the stream keeps reading, so a bad line no longer kills the connection
/// and no line can buffer without bound.
async fn tcp_reader<R: AsyncRead + Unpin>(
    mut stream: R,
    in_tx: mpsc::Sender<String>,
    err_tx: mpsc::Sender<String>,
) {
    let mut buf: Vec<u8> = Vec::with_capacity(8192);
    let mut chunk = [0u8; 8192];
    // Set while the bytes of an over-long line are being discarded up to
    // (and including) its terminating newline.
    let mut discarding = false;

    let line_too_long = || {
        ServerMessage::Error {
            id: None,
            error: format!(
                "line exceeds the maximum of {} bytes; skipped",
                MAX_LINE_BYTES
            ),
        }
        .to_line()
    };

    loop {
        // Deliver complete buffered lines.
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line = buf.drain(..=pos).collect::<Vec<u8>>();
            let line = &line[..line.len() - 1]; // strip '\n'
            let line = line.strip_suffix(b"\r").unwrap_or(line); // tolerate CRLF
            if discarding {
                // Reached the end of an over-long line: resume normal reads.
                discarding = false;
                continue;
            }
            if line.len() > MAX_LINE_BYTES {
                if err_tx.send(line_too_long()).await.is_err() {
                    return;
                }
                continue;
            }
            match String::from_utf8(line.to_vec()) {
                Ok(text) => {
                    if in_tx.send(text).await.is_err() {
                        return;
                    }
                }
                Err(_) => {
                    let msg = ServerMessage::Error {
                        id: None,
                        error: "line is not valid UTF-8; skipped".to_string(),
                    }
                    .to_line();
                    if err_tx.send(msg).await.is_err() {
                        return;
                    }
                }
            }
        }
        if discarding {
            buf.clear();
        }

        match stream.read(&mut chunk).await {
            Ok(0) => return, // EOF
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                // Enforce the cap between newlines. When the pending bytes
                // exceed the limit with no newline in sight, drop them and
                // keep discarding until the line ends (the error frame is
                // sent once, here). When a newline is already buffered,
                // leave the data alone: the delivery loop will reject the
                // over-long line and keep any following lines intact.
                if !discarding && buf.len() > MAX_LINE_BYTES && !buf.contains(&b'\n') {
                    discarding = true;
                    buf.clear();
                    if err_tx.send(line_too_long()).await.is_err() {
                        return;
                    }
                }
            }
            Err(e) => {
                warn!("TCP read error: {}", e);
                return;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use gewu_core::types::{ContentPart, ToolContext, ToolResult};
    use std::error::Error;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // -- Test doubles -------------------------------------------------------

    struct TestLlm {
        behavior: LlmBehavior,
        /// Artificial delay before yielding events (makes asks observably
        /// long-running for interrupt tests).
        delay_ms: u64,
        /// Interior-mutable count of `send` calls (`send` takes `&self`).
        /// The agent prepends a system prompt to the message history, so
        /// `messages.len()` cannot identify the conversation turn; count
        /// provider calls instead.
        calls: AtomicUsize,
    }

    #[derive(Clone, Copy, PartialEq)]
    enum LlmBehavior {
        /// Reply with text only, then finish.
        Plain,
        /// Ask for one tool call, then (second turn) finish with text.
        ToolCall,
    }

    impl TestLlm {
        fn plain() -> Arc<Self> {
            Arc::new(Self {
                behavior: LlmBehavior::Plain,
                delay_ms: 0,
                calls: AtomicUsize::new(0),
            })
        }
        fn tool_call() -> Arc<Self> {
            Arc::new(Self {
                behavior: LlmBehavior::ToolCall,
                delay_ms: 0,
                calls: AtomicUsize::new(0),
            })
        }
        /// Tool-calling LLM that sleeps before responding: long enough for
        /// interrupt requests to reliably arrive mid-ask.
        fn slow_tool_call() -> Arc<Self> {
            Arc::new(Self {
                behavior: LlmBehavior::ToolCall,
                delay_ms: 500,
                calls: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait::async_trait]
    impl gewu_core::traits::LlmProvider for TestLlm {
        fn build(
            &self,
            messages: &[Message],
            _tools: &[Arc<dyn gewu_core::traits::Tool>],
        ) -> std::result::Result<Value, Box<dyn Error + Send + Sync>> {
            Ok(serde_json::json!({ "turn": messages.len() }))
        }

        async fn send<'a>(
            &'a self,
            request: Value,
        ) -> std::result::Result<
            futures::stream::BoxStream<
                'a,
                std::result::Result<gewu_core::types::ProviderEvent, Box<dyn Error + Send + Sync>>,
            >,
            Box<dyn Error + Send + Sync>,
        > {
            if self.delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
            }
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            let events: Vec<gewu_core::types::ProviderEvent> = match self.behavior {
                LlmBehavior::Plain => vec![
                    gewu_core::types::ProviderEvent::ContentDelta {
                        delta: "Hello from test".to_string(),
                    },
                    gewu_core::types::ProviderEvent::Finish {
                        reason: Some("stop".to_string()),
                    },
                ],
                LlmBehavior::ToolCall if call == 1 => vec![
                    gewu_core::types::ProviderEvent::ToolCallStart {
                        id: "call-1".to_string(),
                        name: "echo".to_string(),
                    },
                    gewu_core::types::ProviderEvent::ToolCallDelta {
                        id: "call-1".to_string(),
                        delta: r#"{"value":42}"#.to_string(),
                    },
                    gewu_core::types::ProviderEvent::Finish {
                        reason: Some("tool_calls".to_string()),
                    },
                ],
                LlmBehavior::ToolCall => vec![
                    gewu_core::types::ProviderEvent::ContentDelta {
                        delta: "Tool answered".to_string(),
                    },
                    gewu_core::types::ProviderEvent::Finish {
                        reason: Some("stop".to_string()),
                    },
                ],
            };
            let s = async_stream::try_stream! {
                for event in events {
                    yield event;
                }
            };
            Ok(Box::pin(s))
        }

        async fn list_models(
            &self,
        ) -> std::result::Result<Vec<String>, Box<dyn Error + Send + Sync>> {
            Ok(Vec::new())
        }
    }

    /// LLM that records the user prompts it is asked with, and delays before
    /// responding (so asks are interruptible). Used to observe exactly what
    /// history reaches the model.
    struct RecordingLlm {
        delay_ms: u64,
        user_prompts: std::sync::Mutex<Vec<String>>,
    }

    impl RecordingLlm {
        fn slow() -> Arc<Self> {
            Arc::new(Self {
                delay_ms: 500,
                user_prompts: std::sync::Mutex::new(Vec::new()),
            })
        }

        fn prompts(&self) -> Vec<String> {
            self.user_prompts.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl gewu_core::traits::LlmProvider for RecordingLlm {
        fn build(
            &self,
            messages: &[Message],
            _tools: &[Arc<dyn gewu_core::traits::Tool>],
        ) -> std::result::Result<Value, Box<dyn Error + Send + Sync>> {
            for message in messages {
                if let Message::User { content } = message {
                    let text = content
                        .iter()
                        .filter_map(|part| match part {
                            ContentPart::Text { text } => Some(text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join(" ");
                    if !text.is_empty() {
                        self.user_prompts.lock().unwrap().push(text);
                    }
                }
            }
            Ok(serde_json::json!({}))
        }

        async fn send<'a>(
            &'a self,
            _request: Value,
        ) -> std::result::Result<
            futures::stream::BoxStream<
                'a,
                std::result::Result<gewu_core::types::ProviderEvent, Box<dyn Error + Send + Sync>>,
            >,
            Box<dyn Error + Send + Sync>,
        > {
            if self.delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
            }
            let s = async_stream::try_stream! {
                yield gewu_core::types::ProviderEvent::ContentDelta {
                    delta: "ok".to_string(),
                };
                yield gewu_core::types::ProviderEvent::Finish {
                    reason: Some("stop".to_string()),
                };
            };
            Ok(Box::pin(s))
        }

        async fn list_models(
            &self,
        ) -> std::result::Result<Vec<String>, Box<dyn Error + Send + Sync>> {
            Ok(Vec::new())
        }
    }

    struct EchoTool;

    #[async_trait::async_trait]
    impl gewu_core::traits::Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "Echoes its arguments"
        }
        fn schema(&self) -> schemars::schema::RootSchema {
            schemars::schema_for!(Value)
        }
        async fn execute(
            &self,
            args: Value,
            _ctx: &ToolContext,
        ) -> std::result::Result<ToolResult, Box<dyn Error + Send + Sync>> {
            Ok(ToolResult::new("echo", format!("echoed {}", args)))
        }
    }

    fn plain_session() -> ConnectionSession {
        let agent = gewu_core::Agent::builder()
            .llm(TestLlm::plain())
            .with_tools(vec![])
            .build()
            .unwrap();
        let tools = gewu_core::ToolRegistry::list_tools(&agent);
        ConnectionSession {
            agent: Arc::new(Mutex::new(agent)),
            session_id: "sess-test".to_string(),
            agent_name: "build".to_string(),
            tools,
        }
    }

    fn tool_session() -> ConnectionSession {
        let tool: Arc<dyn gewu_core::traits::Tool> = Arc::new(EchoTool);
        let agent = gewu_core::Agent::builder()
            .llm(TestLlm::tool_call())
            .with_tools(vec![tool])
            .build()
            .unwrap();
        let tools = gewu_core::ToolRegistry::list_tools(&agent);
        ConnectionSession {
            agent: Arc::new(Mutex::new(agent)),
            session_id: "sess-test".to_string(),
            agent_name: "build".to_string(),
            tools,
        }
    }

    async fn collect_ask(session: &ConnectionSession, prompt: &str) -> Vec<String> {
        let (tx, mut rx) = mpsc::channel::<String>(256);
        let handle = tokio::spawn({
            let session = session.clone();
            let prompt = prompt.to_string();
            async move {
                execute_ask(&session, "req-test", &prompt, |msg| {
                    let tx = tx.clone();
                    async move { tx.send(msg.to_line()).await.map_err(anyhow::Error::from) }
                })
                .await;
            }
        });
        handle.await.unwrap();
        let mut lines = Vec::new();
        while let Ok(line) = rx.try_recv() {
            lines.push(line);
        }
        lines
    }

    /// Read frames until the ask's terminal `result`, skipping streamed
    /// events (the agent emits a `usage` snapshot at every turn boundary).
    async fn recv_result(rx: &mut mpsc::Receiver<String>) -> serde_json::Value {
        loop {
            let msg = recv_json(rx).await;
            match msg["type"].as_str() {
                Some("result") => return msg,
                Some("event") => {}
                other => panic!("unexpected frame while waiting for the result: {other:?} {msg}"),
            }
        }
    }

    // -- execute_ask --------------------------------------------------------

    #[tokio::test]
    async fn ask_streams_events_then_result() {
        let session = plain_session();
        let lines = collect_ask(&session, "hi").await;

        assert!(
            lines[0].contains(r#""type":"event""#),
            "first line: {}",
            lines[0]
        );
        assert!(
            lines[0].contains("content_delta"),
            "first line: {}",
            lines[0]
        );
        assert!(lines[0].contains(r#""request_id":"req-test""#));
        let last = lines.last().unwrap();
        assert!(last.contains(r#""type":"result""#), "last line: {}", last);
        assert!(last.contains("Hello from test"), "last line: {}", last);
    }

    #[tokio::test]
    async fn ask_with_tool_call_emits_tool_events() {
        let session = tool_session();
        let lines = collect_ask(&session, "use the tool").await;

        assert!(
            lines
                .iter()
                .any(|l| l.contains("tool_start") && l.contains("echo")),
            "expected tool_start event, got {:?}",
            lines
        );
        assert!(
            lines.iter().any(|l| l.contains("tool_finish")),
            "expected tool_finish event, got {:?}",
            lines
        );
        let last = lines.last().unwrap();
        assert!(last.contains(r#""type":"result""#), "last line: {}", last);
    }

    #[tokio::test]
    async fn ask_streams_usage_event_before_result() {
        let session = plain_session();
        let lines = collect_ask(&session, "hi").await;

        let is_usage = |line: &str| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .and_then(|v| v.get("event").cloned())
                .map(|e| e["type"] == "usage")
                .unwrap_or(false)
        };
        let usage_index = lines
            .iter()
            .position(|l| is_usage(l))
            .unwrap_or_else(|| panic!("expected a usage event, got {:?}", lines));
        assert!(
            lines[usage_index].contains("used_tokens")
                && lines[usage_index].contains("context_limit"),
            "usage event line: {}",
            lines[usage_index]
        );
        let result_index = lines
            .iter()
            .position(|l| l.contains(r#""type":"result""#))
            .expect("ask should end with a result");
        assert!(usage_index < result_index, "usage must precede the result");
    }

    // -- patch_dangling_tool_calls ------------------------------------------

    #[tokio::test]
    async fn patch_appends_results_for_unanswered_tool_calls() {
        let mut agent = gewu_core::Agent::builder()
            .llm(TestLlm::plain())
            .with_tools(vec![])
            .build()
            .unwrap();

        agent
            .memory
            .add_message(Message::Assistant {
                content: None,
                tool_calls: Some(vec![gewu_core::types::ToolCall {
                    id: "call-1".to_string(),
                    r#type: "function".to_string(),
                    function: gewu_core::types::FunctionCall {
                        name: "echo".to_string(),
                        arguments: "{}".to_string(),
                    },
                }]),
            })
            .await;

        patch_dangling_tool_calls(&mut agent).await;

        let messages = agent.memory.messages();
        let last = messages.last().unwrap();
        match last {
            Message::Tool {
                tool_call_id,
                content,
                ..
            } => {
                assert_eq!(tool_call_id, "call-1");
                assert!(content.contains("Interrupted"));
            }
            other => panic!("expected Tool message, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn patch_is_noop_when_tool_calls_answered() {
        let mut agent = gewu_core::Agent::builder()
            .llm(TestLlm::plain())
            .with_tools(vec![])
            .build()
            .unwrap();

        agent
            .memory
            .add_message(Message::Assistant {
                content: None,
                tool_calls: Some(vec![gewu_core::types::ToolCall {
                    id: "call-1".to_string(),
                    r#type: "function".to_string(),
                    function: gewu_core::types::FunctionCall {
                        name: "echo".to_string(),
                        arguments: "{}".to_string(),
                    },
                }]),
            })
            .await;
        agent
            .memory
            .add_message(Message::Tool {
                tool_call_id: "call-1".to_string(),
                tool_name: Some("echo".to_string()),
                content: "done".to_string(),
                media: Vec::new(),
            })
            .await;

        let before = agent.memory.messages().len();
        patch_dangling_tool_calls(&mut agent).await;
        assert_eq!(agent.memory.messages().len(), before);
    }

    #[tokio::test]
    async fn patch_is_noop_without_assistant_tool_calls() {
        let mut agent = gewu_core::Agent::builder()
            .llm(TestLlm::plain())
            .with_tools(vec![])
            .build()
            .unwrap();

        agent
            .memory
            .add_message(Message::User {
                content: vec![ContentPart::Text {
                    text: "hi".to_string(),
                }],
            })
            .await;

        let before = agent.memory.messages().len();
        patch_dangling_tool_calls(&mut agent).await;
        assert_eq!(agent.memory.messages().len(), before);
    }

    // -- drive_connection ---------------------------------------------------

    /// Starts a `drive_connection` loop with plain test wiring and returns
    /// the request/out channel handles for driving it from the test.
    async fn spawn_connection(
        state: Arc<ServerState>,
    ) -> (
        mpsc::Sender<String>,
        mpsc::Receiver<String>,
        JoinHandle<Result<()>>,
    ) {
        let (in_tx, in_rx) = mpsc::channel::<String>(16);
        let (out_tx, out_rx) = mpsc::channel::<String>(256);
        let handle = tokio::spawn(drive_connection(state, in_rx, out_tx));
        (in_tx, out_rx, handle)
    }

    async fn recv_json(out_rx: &mut mpsc::Receiver<String>) -> Value {
        let line = out_rx.recv().await.expect("server message");
        serde_json::from_str(&line).expect("valid JSON line")
    }

    fn server_state_with(provider: Arc<dyn gewu_core::traits::LlmProvider>) -> Arc<ServerState> {
        server_state_with_token(provider, None)
    }

    fn server_state_with_token(
        provider: Arc<dyn gewu_core::traits::LlmProvider>,
        token: Option<&str>,
    ) -> Arc<ServerState> {
        Arc::new(ServerState {
            bootstrap: Bootstrap {
                provider,
                pool: sqlx::sqlite::SqlitePoolOptions::new()
                    .connect_lazy("sqlite::memory:")
                    .unwrap(),
                mcp_tools: Vec::new(),
                skills: Vec::new(),
                log_writer: None,
                model: "test-model".to_string(),
                provider_name: "test".to_string(),
                working_directory: "/tmp/gewu-test".to_string(),
            },
            token: token.map(str::to_string),
            busy_sessions: Mutex::new(HashSet::new()),
        })
    }

    #[tokio::test]
    async fn connection_replies_to_ping_and_list_agents() {
        let state = server_state_with(TestLlm::plain());
        let (in_tx, mut out_rx, handle) = spawn_connection(state).await;

        in_tx
            .send(r#"{"id": 1, "op": "ping"}"#.to_string())
            .await
            .unwrap();
        let pong = recv_json(&mut out_rx).await;
        assert_eq!(pong["type"], "pong");
        assert_eq!(pong["id"], 1);

        in_tx
            .send(r#"{"id": 2, "op": "list_agents"}"#.to_string())
            .await
            .unwrap();
        let agents = recv_json(&mut out_rx).await;
        assert_eq!(agents["type"], "agents");
        assert!(agents["agents"].as_array().unwrap().len() >= 3);

        drop(in_tx);
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn connection_reports_context_usage() {
        let state = server_state_with(TestLlm::plain());
        let (in_tx, mut out_rx, handle) = spawn_connection(state).await;

        // No session yet: nothing to measure.
        in_tx
            .send(r#"{"id": 1, "op": "usage"}"#.to_string())
            .await
            .unwrap();
        let usage = recv_json(&mut out_rx).await;
        assert_eq!(usage["type"], "usage");
        assert_eq!(usage["used_tokens"], 0);
        assert_eq!(usage["context_limit"], 0);

        // After an ask the conversation has measurable content.
        in_tx
            .send(r#"{"id": 2, "op": "ask", "prompt": "hello"}"#.to_string())
            .await
            .unwrap();
        let _ready = recv_json(&mut out_rx).await;
        let _event = recv_json(&mut out_rx).await;
        let _result = recv_result(&mut out_rx).await;

        in_tx
            .send(r#"{"id": 3, "op": "usage"}"#.to_string())
            .await
            .unwrap();
        let usage = recv_json(&mut out_rx).await;
        assert_eq!(usage["type"], "usage");
        assert!(
            usage["used_tokens"].as_u64().unwrap() > 0,
            "ask history should count: {usage}"
        );
        assert_eq!(usage["context_limit"], 128_000);

        drop(in_tx);
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn connection_reports_model_and_provider() {
        let state = server_state_with(TestLlm::plain());
        let (in_tx, mut out_rx, handle) = spawn_connection(state).await;

        in_tx
            .send(r#"{"id": 1, "op": "info"}"#.to_string())
            .await
            .unwrap();
        let info = recv_json(&mut out_rx).await;
        assert_eq!(info["type"], "info");
        assert_eq!(info["id"], 1);
        assert_eq!(info["model"], "test-model");
        assert_eq!(info["provider"], "test");

        drop(in_tx);
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn connection_lists_and_loads_sessions() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE memory_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at INTEGER NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        let message = Message::User {
            content: vec![ContentPart::Text {
                text: "hi".to_string(),
            }],
        };
        sqlx::query(
            "INSERT INTO memory_messages (session_id, content, created_at) VALUES (?, ?, ?)",
        )
        .bind("s1")
        .bind(serde_json::to_string(&message).unwrap())
        .bind(1)
        .execute(&pool)
        .await
        .unwrap();

        let state = Arc::new(ServerState {
            bootstrap: Bootstrap {
                provider: TestLlm::plain(),
                pool,
                mcp_tools: Vec::new(),
                skills: Vec::new(),
                log_writer: None,
                model: "test-model".to_string(),
                provider_name: "test".to_string(),
                working_directory: "/tmp/gewu-test".to_string(),
            },
            token: None,
            busy_sessions: Mutex::new(HashSet::new()),
        });
        let (in_tx, mut out_rx, handle) = spawn_connection(state).await;

        in_tx
            .send(r#"{"id": 1, "op": "list_sessions"}"#.to_string())
            .await
            .unwrap();
        let sessions = recv_json(&mut out_rx).await;
        assert_eq!(sessions["type"], "sessions");
        assert_eq!(sessions["sessions"][0]["session_id"], "s1");
        assert_eq!(sessions["sessions"][0]["message_count"], 1);

        in_tx
            .send(r#"{"id": 2, "op": "get_session", "session_id": "s1"}"#.to_string())
            .await
            .unwrap();
        let session = recv_json(&mut out_rx).await;
        assert_eq!(session["type"], "session");
        assert_eq!(session["session_id"], "s1");
        assert_eq!(session["messages"].as_array().unwrap().len(), 1);

        in_tx
            .send(r#"{"id": 3, "op": "get_session", "session_id": "missing"}"#.to_string())
            .await
            .unwrap();
        let empty = recv_json(&mut out_rx).await;
        assert_eq!(empty["type"], "session");
        assert_eq!(empty["messages"].as_array().unwrap().len(), 0);

        drop(in_tx);
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn connection_rejects_bad_requests() {
        let state = server_state_with(TestLlm::plain());
        let (in_tx, mut out_rx, handle) = spawn_connection(state).await;

        in_tx.send("not json".to_string()).await.unwrap();
        let err = recv_json(&mut out_rx).await;
        assert_eq!(err["type"], "error");
        assert!(err["error"].as_str().unwrap().contains("JSON"));

        in_tx
            .send(r#"{"id": 1, "op": "unknown"}"#.to_string())
            .await
            .unwrap();
        let err = recv_json(&mut out_rx).await;
        assert_eq!(err["type"], "error");
        assert!(err["error"].as_str().unwrap().contains("unknown op"));

        drop(in_tx);
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn connection_runs_ask_and_streams_events() {
        let state = server_state_with(TestLlm::plain());
        let (in_tx, mut out_rx, handle) = spawn_connection(state).await;

        in_tx
            .send(r#"{"id": 1, "op": "ask", "prompt": "hello"}"#.to_string())
            .await
            .unwrap();

        let ready = recv_json(&mut out_rx).await;
        assert_eq!(ready["type"], "ready");
        assert_eq!(ready["id"], 1);
        let request_id = ready["request_id"].as_str().unwrap().to_string();
        let session_id = ready["session_id"].as_str().unwrap().to_string();
        assert!(!request_id.is_empty());
        assert!(!session_id.is_empty());

        // Events then result.
        let event = recv_json(&mut out_rx).await;
        assert_eq!(event["type"], "event");
        assert_eq!(event["event"]["type"], "content_delta");
        let result = recv_result(&mut out_rx).await;
        assert_eq!(result["response"], "Hello from test");
        assert_eq!(result["request_id"], request_id.as_str());

        drop(in_tx);
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn connection_deletes_a_session_from_the_store() {
        let state = server_state_with(TestLlm::plain());
        let (in_tx, mut out_rx, handle) = spawn_connection(state).await;

        // Produce a stored session.
        in_tx
            .send(r#"{"id": 1, "op": "ask", "prompt": "hello"}"#.to_string())
            .await
            .unwrap();
        let _ready = recv_json(&mut out_rx).await;
        let _event = recv_json(&mut out_rx).await;
        let result = recv_result(&mut out_rx).await;
        assert_eq!(result["type"], "result");
        let session_id = result["session_id"].as_str().unwrap().to_string();

        in_tx
            .send(r#"{"id": 2, "op": "list_sessions"}"#.to_string())
            .await
            .unwrap();
        let sessions = recv_json(&mut out_rx).await;
        assert_eq!(sessions["sessions"].as_array().unwrap().len(), 1);

        in_tx
            .send(
                format!(r#"{{"id": 3, "op": "delete_session", "session_id": "{session_id}"}}"#)
                    .to_string(),
            )
            .await
            .unwrap();
        let deleted = recv_json(&mut out_rx).await;
        assert_eq!(deleted["type"], "deleted");
        assert_eq!(deleted["session_id"], session_id.as_str());

        in_tx
            .send(r#"{"id": 4, "op": "list_sessions"}"#.to_string())
            .await
            .unwrap();
        let sessions = recv_json(&mut out_rx).await;
        assert_eq!(sessions["sessions"].as_array().unwrap().len(), 0);

        in_tx
            .send(
                format!(r#"{{"id": 5, "op": "get_session", "session_id": "{session_id}"}}"#)
                    .to_string(),
            )
            .await
            .unwrap();
        let empty = recv_json(&mut out_rx).await;
        assert_eq!(empty["messages"].as_array().unwrap().len(), 0);

        // Malformed request: missing session id.
        in_tx
            .send(r#"{"id": 6, "op": "delete_session"}"#.to_string())
            .await
            .unwrap();
        let err = recv_json(&mut out_rx).await;
        assert_eq!(err["type"], "error");
        assert!(err["error"].as_str().unwrap().contains("session_id"));

        drop(in_tx);
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn connection_rejects_second_ask_while_running() {
        // A slow tool-calling agent keeps the ask busy across two LLM turns,
        // long enough for the second ask to arrive while the first is still
        // running (a zero-delay ask can finish before it is even read).
        let state = server_state_with(TestLlm::slow_tool_call());
        let (in_tx, mut out_rx, handle) = spawn_connection(state).await;

        in_tx
            .send(r#"{"id": 1, "op": "ask", "prompt": "go"}"#.to_string())
            .await
            .unwrap();
        let ready = recv_json(&mut out_rx).await;
        assert_eq!(ready["type"], "ready");

        // Wait until the first event arrives so the ask is definitely running.
        let event = recv_json(&mut out_rx).await;
        assert_eq!(event["type"], "event");

        in_tx
            .send(r#"{"id": 2, "op": "ask", "prompt": "again"}"#.to_string())
            .await
            .unwrap();
        // The rejection error is emitted by the connection loop while the
        // running ask streams events into the same channel from another
        // task, so drain messages until the error shows up. A `result`
        // first would mean the second ask was accepted, i.e. not rejected.
        let mut rejected = false;
        loop {
            let msg = recv_json(&mut out_rx).await;
            match msg["type"].as_str() {
                Some("error") => {
                    assert!(
                        msg["error"]
                            .as_str()
                            .unwrap()
                            .contains("already in progress"),
                        "unexpected error: {}",
                        msg["error"]
                    );
                    rejected = true;
                    break;
                }
                Some("result") => {
                    panic!("second ask should be rejected while the first is running")
                }
                _ => continue,
            }
        }
        assert!(rejected);

        // Let the first ask finish.
        loop {
            if recv_json(&mut out_rx).await["type"] == "result" {
                break;
            }
        }

        drop(in_tx);
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn connection_interrrupt_aborts_running_ask_and_repairs_history() {
        let state = server_state_with(TestLlm::slow_tool_call());
        let (in_tx, mut out_rx, handle) = spawn_connection(state).await;

        in_tx
            .send(r#"{"id": 1, "op": "ask", "prompt": "go"}"#.to_string())
            .await
            .unwrap();
        let ready = recv_json(&mut out_rx).await;
        assert_eq!(ready["type"], "ready");
        let request_id = ready["request_id"].as_str().unwrap().to_string();

        // The ask is sleeping in the LLM call; interrupt it now.
        in_tx
            .send(r#"{"id": 2, "op": "interrupt"}"#.to_string())
            .await
            .unwrap();
        let interrupted = recv_json(&mut out_rx).await;
        assert_eq!(interrupted["type"], "interrupted");
        assert_eq!(interrupted["request_id"], request_id.as_str());
        assert_eq!(interrupted["id"], 2);

        // The loop should now accept another request (the aborted one
        // released the agent).
        in_tx
            .send(r#"{"id": 3, "op": "ping"}"#.to_string())
            .await
            .unwrap();
        let pong = recv_json(&mut out_rx).await;
        assert_eq!(pong["type"], "pong");

        drop(in_tx);
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn error_replies_echo_the_client_id() {
        let state = server_state_with(TestLlm::plain());
        let (in_tx, mut out_rx, handle) = spawn_connection(state).await;

        // A known op that fails server-side echoes the parsed request id.
        in_tx
            .send(r#"{"id": "abc", "op": "ask", "prompt": "go", "agent": "nope"}"#.to_string())
            .await
            .unwrap();
        let err = recv_json(&mut out_rx).await;
        assert_eq!(err["type"], "error");
        assert_eq!(err["id"], "abc");

        // A malformed message echoes the raw id field when one is present.
        in_tx
            .send(r#"{"id": 42, "op": "bogus"}"#.to_string())
            .await
            .unwrap();
        let err = recv_json(&mut out_rx).await;
        assert_eq!(err["type"], "error");
        assert_eq!(err["id"], 42);

        // No id present at all stays null.
        in_tx.send("not json".to_string()).await.unwrap();
        let err = recv_json(&mut out_rx).await;
        assert_eq!(err["type"], "error");
        assert!(err.get("id").is_none() || err["id"].is_null());

        drop(in_tx);
        handle.await.unwrap().unwrap();
    }

    /// LLM that always asks for the `echo` tool, forever.
    struct ToolCallLoopLlm;

    #[async_trait::async_trait]
    impl gewu_core::traits::LlmProvider for ToolCallLoopLlm {
        fn build(
            &self,
            _messages: &[Message],
            _tools: &[Arc<dyn gewu_core::traits::Tool>],
        ) -> std::result::Result<Value, Box<dyn Error + Send + Sync>> {
            Ok(serde_json::json!({}))
        }

        async fn send<'a>(
            &'a self,
            _request: Value,
        ) -> std::result::Result<
            futures::stream::BoxStream<
                'a,
                std::result::Result<gewu_core::types::ProviderEvent, Box<dyn Error + Send + Sync>>,
            >,
            Box<dyn Error + Send + Sync>,
        > {
            let s = async_stream::try_stream! {
                yield gewu_core::types::ProviderEvent::ToolCallStart {
                    id: "call-1".to_string(),
                    name: "echo".to_string(),
                };
                yield gewu_core::types::ProviderEvent::ToolCallDelta {
                    id: "call-1".to_string(),
                    delta: "{}".to_string(),
                };
                yield gewu_core::types::ProviderEvent::Finish {
                    reason: Some("tool_calls".to_string()),
                };
            };
            Ok(Box::pin(s))
        }

        async fn list_models(
            &self,
        ) -> std::result::Result<Vec<String>, Box<dyn Error + Send + Sync>> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn ask_ending_without_finish_emits_terminal_error() {
        // max_turns(1) forces the agent to end with a final AgentEvent::Error
        // after the first tool turn, never reaching a Finish.
        let agent = gewu_core::Agent::builder()
            .llm(Arc::new(ToolCallLoopLlm))
            .with_tools(vec![Arc::new(EchoTool) as Arc<dyn gewu_core::traits::Tool>])
            .max_turns(Some(1))
            .build()
            .unwrap();
        let session = ConnectionSession {
            agent: Arc::new(Mutex::new(agent)),
            session_id: "sess-test".to_string(),
            agent_name: "build".to_string(),
            tools: vec!["echo".to_string()],
        };

        let (tx, mut rx) = mpsc::channel::<String>(256);
        let handle = tokio::spawn({
            let session = session.clone();
            async move {
                execute_ask(&session, "req-test", "go", |msg| {
                    let tx = tx.clone();
                    async move { tx.send(msg.to_line()).await.map_err(anyhow::Error::from) }
                })
                .await;
            }
        });
        handle.await.unwrap();

        let mut saw_error_event = false;
        let mut terminal = None;
        while let Ok(line) = rx.try_recv() {
            let msg: Value = serde_json::from_str(&line).unwrap();
            match msg["type"].as_str() {
                Some("event") if msg["event"]["type"] == "error" => saw_error_event = true,
                Some("error") => terminal = Some(msg),
                _ => {}
            }
        }
        assert!(
            saw_error_event,
            "expected a streamed error event before the terminal frame"
        );
        let terminal = terminal.expect("terminal error frame");
        assert!(
            terminal["error"]
                .as_str()
                .unwrap()
                .contains("without a final response"),
            "terminal frame: {}",
            terminal
        );
    }

    #[tokio::test]
    async fn list_tools_answers_from_snapshot_while_ask_runs() {
        let state = server_state_with(TestLlm::tool_call());
        let (in_tx, mut out_rx, handle) = spawn_connection(state).await;

        // The first ask creates the connection session (explore profile has
        // a fixed, non-empty tool set).
        in_tx
            .send(r#"{"id": 1, "op": "ask", "prompt": "go", "agent": "explore"}"#.to_string())
            .await
            .unwrap();
        let ready = recv_json(&mut out_rx).await;
        assert_eq!(ready["type"], "ready");

        // Ask is now running; list_tools must still answer promptly from
        // the session snapshot instead of blocking on the agent lock.
        in_tx
            .send(r#"{"id": 2, "op": "list_tools"}"#.to_string())
            .await
            .unwrap();
        let tools = recv_json(&mut out_rx).await;
        assert_eq!(tools["type"], "tools");
        assert_eq!(tools["id"], 2);
        assert!(
            tools["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t.as_str() == Some("read_file")),
            "expected read_file in {:?}",
            tools["tools"]
        );

        // Drain to the result so the ask task completes cleanly.
        let mut saw_result = false;
        for _ in 0..10 {
            let msg = recv_json(&mut out_rx).await;
            if msg["type"] == "result" {
                saw_result = true;
                break;
            }
        }
        assert!(saw_result, "first ask should complete");

        drop(in_tx);
        handle.await.unwrap().unwrap();
    }

    // -- usage busy flag (Bug 2) ---------------------------------------------

    #[tokio::test]
    async fn usage_reports_busy_during_running_ask() {
        let state = server_state_with(TestLlm::slow_tool_call());
        let (in_tx, mut out_rx, handle) = spawn_connection(state).await;

        in_tx
            .send(r#"{"id": 1, "op": "ask", "prompt": "go"}"#.to_string())
            .await
            .unwrap();
        let ready = recv_json(&mut out_rx).await;
        assert_eq!(ready["type"], "ready");

        // While the ask holds the agent lock, usage is 0/0 *and* busy.
        in_tx
            .send(r#"{"id": 2, "op": "usage"}"#.to_string())
            .await
            .unwrap();
        let mut usage = None;
        for _ in 0..10 {
            let msg = recv_json(&mut out_rx).await;
            if msg["type"] == "usage" {
                usage = Some(msg);
                break;
            }
        }
        let usage = usage.expect("usage reply while ask running");
        assert_eq!(usage["used_tokens"], 0);
        assert_eq!(usage["context_limit"], 0);
        assert_eq!(usage["busy"], true, "busy ask must be flagged: {usage}");

        // No session at all stays 0/0 with busy: false.
        let state2 = server_state_with(TestLlm::plain());
        let (in_tx2, mut out_rx2, handle2) = spawn_connection(state2).await;
        in_tx2
            .send(r#"{"id": 1, "op": "usage"}"#.to_string())
            .await
            .unwrap();
        let usage = recv_json(&mut out_rx2).await;
        assert_eq!(usage["used_tokens"], 0);
        assert_eq!(usage["context_limit"], 0);
        assert_eq!(usage["busy"], false);

        drop(in_tx);
        let _ = handle.await.unwrap();
        drop(in_tx2);
        let _ = handle2.await.unwrap();
    }

    // -- TCP reader hardening (Bugs 3 and 6) ---------------------------------

    #[tokio::test]
    async fn tcp_reader_skips_invalid_utf8_and_keeps_reading() {
        let (mut client, server) = tokio::io::duplex(65_536);
        let (in_tx, mut in_rx) = mpsc::channel::<String>(16);
        let (err_tx, mut err_rx) = mpsc::channel::<String>(16);
        tokio::spawn(tcp_reader(server, in_tx, err_tx));

        use tokio::io::AsyncWriteExt;
        client
            .write_all(b"{\"id\":1,\"op\":\"ping\"}\n")
            .await
            .unwrap();
        client.write_all(b"\xff\xfe bad\n").await.unwrap();
        client
            .write_all(b"{\"id\":2,\"op\":\"ping\"}\n")
            .await
            .unwrap();

        let good = in_rx.recv().await.unwrap();
        assert!(good.contains("\"id\":1"), "{good}");
        let err = err_rx.recv().await.unwrap();
        assert!(err.contains("UTF-8"), "{err}");
        // The connection is still usable after the bad line.
        let good2 = in_rx.recv().await.unwrap();
        assert!(good2.contains("\"id\":2"), "{good2}");
    }

    #[tokio::test]
    async fn tcp_reader_rejects_overlong_lines_and_keeps_reading() {
        let (mut client, server) = tokio::io::duplex(65_536);
        let (in_tx, mut in_rx) = mpsc::channel::<String>(16);
        let (err_tx, mut err_rx) = mpsc::channel::<String>(16);
        tokio::spawn(tcp_reader(server, in_tx, err_tx));

        use tokio::io::AsyncWriteExt;
        // A line far over the cap, written in pieces, then a normal line.
        let pad = vec![b'x'; MAX_LINE_BYTES + 4096];
        client.write_all(&pad).await.unwrap();
        client.write_all(b"\n").await.unwrap();
        client
            .write_all(b"{\"id\":2,\"op\":\"ping\"}\n")
            .await
            .unwrap();

        let err = err_rx.recv().await.unwrap();
        assert!(err.contains("maximum"), "{err}");
        let good = in_rx.recv().await.unwrap();
        assert!(good.contains("\"id\":2"), "{good}");
        // Exactly one error frame for the one bad line.
        assert!(err_rx.try_recv().is_err());
    }

    // -- auth (Bug 5) ---------------------------------------------------------

    #[tokio::test]
    async fn token_protected_server_requires_auth_first() {
        let state = server_state_with_token(TestLlm::plain(), Some("secret"));
        let (in_tx, mut out_rx, handle) = spawn_connection(state).await;

        // Any other op is rejected until auth succeeds.
        in_tx
            .send(r#"{"id": 1, "op": "ping"}"#.to_string())
            .await
            .unwrap();
        let err = recv_json(&mut out_rx).await;
        assert_eq!(err["type"], "error");
        assert!(err["error"].as_str().unwrap().contains("authentication"));

        // Correct token unlocks the connection.
        in_tx
            .send(r#"{"id": 2, "op": "auth", "token": "secret"}"#.to_string())
            .await
            .unwrap();
        let auth = recv_json(&mut out_rx).await;
        assert_eq!(auth["type"], "auth");
        assert_eq!(auth["ok"], true);
        assert_eq!(auth["id"], 2);

        in_tx
            .send(r#"{"id": 3, "op": "ping"}"#.to_string())
            .await
            .unwrap();
        let pong = recv_json(&mut out_rx).await;
        assert_eq!(pong["type"], "pong");

        drop(in_tx);
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn token_protected_server_rejects_wrong_token_and_closes() {
        let state = server_state_with_token(TestLlm::plain(), Some("secret"));
        let (in_tx, mut out_rx, handle) = spawn_connection(state).await;

        in_tx
            .send(r#"{"id": 1, "op": "auth", "token": "wrong"}"#.to_string())
            .await
            .unwrap();
        let auth = recv_json(&mut out_rx).await;
        assert_eq!(auth["type"], "auth");
        assert_eq!(auth["ok"], false);

        // The connection is closed after a failed auth attempt.
        let closed = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
        assert!(closed.is_ok(), "connection should close after bad auth");
        assert!(closed.unwrap().unwrap().is_ok());
    }

    // -- session binding (Bug 9) ----------------------------------------------

    #[tokio::test]
    async fn session_mismatch_is_rejected() {
        let state = server_state_with(TestLlm::plain());
        let (in_tx, mut out_rx, handle) = spawn_connection(state).await;

        // Bind the connection to session s1.
        in_tx
            .send(r#"{"id": 1, "op": "ask", "prompt": "hi", "session_id": "s1"}"#.to_string())
            .await
            .unwrap();
        let ready = recv_json(&mut out_rx).await;
        assert_eq!(ready["session_id"], "s1");
        loop {
            if recv_json(&mut out_rx).await["type"] == "result" {
                break;
            }
        }

        // A different session_id is now an error, not a silent ignore.
        in_tx
            .send(r#"{"id": 2, "op": "list_tools", "session_id": "other"}"#.to_string())
            .await
            .unwrap();
        let err = recv_json(&mut out_rx).await;
        assert_eq!(err["type"], "error");
        assert!(
            err["error"].as_str().unwrap().contains("session mismatch"),
            "{err}"
        );

        // So is a different agent.
        in_tx
            .send(r#"{"id": 3, "op": "list_tools", "agent": "explore"}"#.to_string())
            .await
            .unwrap();
        let err = recv_json(&mut out_rx).await;
        assert_eq!(err["type"], "error");
        assert!(
            err["error"].as_str().unwrap().contains("agent mismatch"),
            "{err}"
        );

        drop(in_tx);
        handle.await.unwrap().unwrap();
    }

    // -- cross-connection session locking (Bug 4) ------------------------------

    #[tokio::test]
    async fn second_connection_cannot_ask_a_busy_session() {
        let state = server_state_with(TestLlm::slow_tool_call());
        let (in_a, mut out_a, handle_a) = spawn_connection(state.clone()).await;
        let (in_b, mut out_b, handle_b) = spawn_connection(state.clone()).await;

        // Connection A starts an ask, creating session s1.
        in_a.send(r#"{"id": 1, "op": "ask", "prompt": "go", "session_id": "s1"}"#.to_string())
            .await
            .unwrap();
        let ready = recv_json(&mut out_a).await;
        assert_eq!(ready["type"], "ready");

        // Connection B targets the same session and is rejected.
        in_b.send(r#"{"id": 1, "op": "ask", "prompt": "me too", "session_id": "s1"}"#.to_string())
            .await
            .unwrap();
        let mut rejected = false;
        for _ in 0..10 {
            let msg = recv_json(&mut out_b).await;
            if msg["type"] == "error" {
                assert!(msg["error"].as_str().unwrap().contains("busy"), "{msg}");
                rejected = true;
                break;
            }
        }
        assert!(rejected, "second connection should be rejected");

        // A finishes; the claim is released.
        loop {
            if recv_json(&mut out_a).await["type"] == "result" {
                break;
            }
        }

        // B can now run an ask on the same session.
        in_b.send(r#"{"id": 2, "op": "ask", "prompt": "me too", "session_id": "s1"}"#.to_string())
            .await
            .unwrap();
        let ready_b = recv_json(&mut out_b).await;
        assert_eq!(
            ready_b["type"], "ready",
            "claim should be released: {ready_b}"
        );
        loop {
            if recv_json(&mut out_b).await["type"] == "result" {
                break;
            }
        }

        drop(in_a);
        drop(in_b);
        handle_a.await.unwrap().unwrap();
        handle_b.await.unwrap().unwrap();
    }

    // -- interrupted prompt handling (Bug 11) ---------------------------------

    #[tokio::test]
    async fn interrupt_drops_prompt_without_output() {
        let mut agent = gewu_core::Agent::builder()
            .llm(TestLlm::plain())
            .with_tools(vec![])
            .build()
            .unwrap();

        // Simulate an ask that was aborted before the model responded: the
        // prompt is the last message in the history.
        agent
            .memory
            .add_message(Message::User {
                content: vec![ContentPart::Text {
                    text: "write an essay".to_string(),
                }],
            })
            .await;

        drop_dangling_prompt(&mut agent).await;

        assert!(
            agent
                .memory
                .messages()
                .iter()
                .all(|m| !matches!(m, Message::User { .. })),
            "the unanswered prompt should be dropped: {:?}",
            agent.memory.messages()
        );
    }

    #[tokio::test]
    async fn drop_prompt_keeps_exchanges_that_produced_output() {
        let mut agent = gewu_core::Agent::builder()
            .llm(TestLlm::plain())
            .with_tools(vec![])
            .build()
            .unwrap();

        // An aborted exchange that already ran a tool: user, assistant tool
        // call, and (after the interrupt patch) the synthetic tool result.
        agent
            .memory
            .add_message(Message::User {
                content: vec![ContentPart::Text {
                    text: "do the thing".to_string(),
                }],
            })
            .await;
        agent
            .memory
            .add_message(Message::Assistant {
                content: None,
                tool_calls: Some(vec![gewu_core::types::ToolCall {
                    id: "call-1".to_string(),
                    r#type: "function".to_string(),
                    function: gewu_core::types::FunctionCall {
                        name: "echo".to_string(),
                        arguments: "{}".to_string(),
                    },
                }]),
            })
            .await;
        patch_dangling_tool_calls(&mut agent).await;
        drop_dangling_prompt(&mut agent).await;

        let messages = agent.memory.messages();
        assert_eq!(messages.len(), 3, "history with output is kept");
        assert!(matches!(messages[0], Message::User { .. }));
        assert!(matches!(messages.last().unwrap(), Message::Tool { .. }));
    }

    #[tokio::test]
    async fn interrupt_drops_prompt_across_full_connection() {
        // The next ask must not be steered by an interrupted prompt that
        // never produced output (Bug 11).
        let llm = RecordingLlm::slow();
        let state = server_state_with(llm.clone() as Arc<dyn gewu_core::traits::LlmProvider>);
        let (in_tx, mut out_rx, handle) = spawn_connection(state).await;

        in_tx
            .send(r#"{"id": 1, "op": "ask", "prompt": "write an essay"}"#.to_string())
            .await
            .unwrap();
        let ready = recv_json(&mut out_rx).await;
        assert_eq!(ready["type"], "ready");

        // Interrupt before the (delayed) LLM produced anything.
        in_tx
            .send(r#"{"id": 2, "op": "interrupt"}"#.to_string())
            .await
            .unwrap();
        let interrupted = recv_json(&mut out_rx).await;
        assert_eq!(interrupted["type"], "interrupted");

        // The next ask's LLM request must not contain the aborted prompt.
        in_tx
            .send(r#"{"id": 3, "op": "ask", "prompt": "Say ok"}"#.to_string())
            .await
            .unwrap();
        loop {
            if recv_json(&mut out_rx).await["type"] == "result" {
                break;
            }
        }

        let prompts = llm.prompts();
        assert!(
            !prompts.iter().any(|p| p.contains("essay")),
            "aborted prompt leaked into the next LLM request: {prompts:?}"
        );
        assert!(
            prompts.iter().any(|p| p.contains("Say ok")),
            "the new prompt should reach the LLM: {prompts:?}"
        );

        drop(in_tx);
        handle.await.unwrap().unwrap();
    }
}
