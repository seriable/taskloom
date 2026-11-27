use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::{
    state::fingerprint,
    store::{absolute_root, atomic_write, blocking, read_json, storage},
    types::{pending_calls, validate_history},
    DurableAgent, Message, RunEventKind, RunState, RunStatus, RuntimeError, StopReason,
    ToolProgress, ToolResult,
};

pub const SESSION_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionBinding {
    pub turn_id: String,
    pub run_id: String,
    pub input: String,
    pub base_message_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTurn {
    pub binding: SessionBinding,
    pub message_end_count: usize,
    pub report_fingerprint: String,
    pub outcome: StopReason,
    pub actor: Option<String>,
    pub note: Option<String>,
}

/// Append-only conversation plus one persistent active-turn binding. A paused
/// or approval-waiting job keeps the session occupied across process restarts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub version: u32,
    pub revision: u64,
    pub id: String,
    pub system_message_count: usize,
    pub messages: Vec<Message>,
    pub turns: Vec<SessionTurn>,
    pub active: Option<SessionBinding>,
}

impl Session {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        let invalid = |detail: &str| RuntimeError::Storage(format!("invalid session: {detail}"));
        let revision = u64::try_from(self.turns.len())
            .ok()
            .and_then(|count| count.checked_mul(2))
            .and_then(|n| n.checked_add(1 + u64::from(self.active.is_some())));
        if self.version != SESSION_VERSION
            || self.id.trim().is_empty()
            || revision != Some(self.revision)
        {
            return Err(invalid("version, identity or revision mismatch"));
        }
        if self.system_message_count > self.messages.len()
            || self.messages[..self.system_message_count]
                .iter()
                .any(|m| !matches!(m, Message::System { .. }))
        {
            return Err(invalid("invalid system prefix"));
        }
        if self.messages.len() > self.system_message_count {
            validate_history(&self.messages).map_err(|error| invalid(&error.to_string()))?;
        }
        let mut turn_ids = BTreeSet::new();
        let mut end = self.system_message_count;
        for turn in &self.turns {
            validate_binding(&self.id, &turn.binding, end)?;
            if !turn_ids.insert(&turn.binding.turn_id)
                || turn.message_end_count <= end
                || turn.message_end_count > self.messages.len()
                || !matches!(self.messages.get(end),Some(Message::User {content}) if content == &turn.binding.input)
                || turn.report_fingerprint.len() != 64
                || !turn
                    .report_fingerprint
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit())
            {
                return Err(invalid("turn boundary or identity mismatch"));
            }
            // The full history was validated once. Each next turn starts at
            // an actual User message, which cannot cross an unresolved tool
            // batch; the final boundary is the end of that validated history.
            if turn.outcome == StopReason::Completed {
                if turn.actor.is_some()
                    || turn.note.is_some()
                    || !matches!(&self.messages[turn.message_end_count-1],Message::Assistant {turn} if turn.tool_calls.is_empty())
                {
                    return Err(invalid("completed turn has no final answer"));
                }
            } else if !turn
                .actor
                .as_ref()
                .is_some_and(|actor| !actor.trim().is_empty() && actor.len() <= 256)
                || !turn.note.as_ref().is_some_and(|note| note.len() <= 4096)
            {
                return Err(invalid("failed turn has no operator acknowledgement"));
            }
            end = turn.message_end_count;
        }
        if end != self.messages.len() {
            return Err(invalid("unbound messages at the end of the session"));
        }
        if let Some(binding) = &self.active {
            validate_binding(&self.id, binding, end)?;
            if turn_ids.contains(&binding.turn_id) {
                return Err(invalid("active turn is already closed"));
            }
        }
        Ok(())
    }

    fn input(&self, binding: &SessionBinding) -> Result<SessionInput, RuntimeError> {
        let mut messages = self
            .messages
            .get(..binding.base_message_count)
            .ok_or_else(|| RuntimeError::Storage("session input prefix is missing".into()))?
            .to_vec();
        messages.push(Message::user(&binding.input));
        Ok(SessionInput {
            turn_id: binding.turn_id.clone(),
            run_id: binding.run_id.clone(),
            messages,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInput {
    pub turn_id: String,
    pub run_id: String,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRun {
    pub session: Session,
    pub run: RunState,
}

/// `save` is an atomic compare-and-swap. None creates revision 1; Some(n)
/// replaces exactly revision n with n+1. Never silently overwrite a conflict.
pub trait SessionStore: Send + Sync {
    fn load<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<Session>, RuntimeError>>;
    fn save<'a>(
        &'a self,
        session: &'a Session,
        expected_revision: Option<u64>,
    ) -> BoxFuture<'a, Result<(), RuntimeError>>;
}

#[derive(Debug, Clone)]
pub struct FileSessionStore {
    root: PathBuf,
    root_error: Option<String>,
    max_bytes: u64,
}
impl FileSessionStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let (root, root_error) = absolute_root(root.into());
        Self {
            root,
            root_error,
            max_bytes: 64 * 1024 * 1024,
        }
    }
    pub fn with_max_bytes(mut self, bytes: u64) -> Self {
        self.max_bytes = bytes;
        self
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl SessionStore for FileSessionStore {
    fn load<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<Session>, RuntimeError>> {
        Box::pin(async move {
            if let Some(error) = &self.root_error {
                return Err(storage(error));
            }
            let path = std::path::absolute(&self.root)
                .map_err(storage)?
                .join(format!("{}.session.json", session_key(id)?));
            let max = self.max_bytes;
            let id = id.to_string();
            blocking(move || read_session(&path, max, &id)).await
        })
    }
    fn save<'a>(
        &'a self,
        session: &'a Session,
        expected_revision: Option<u64>,
    ) -> BoxFuture<'a, Result<(), RuntimeError>> {
        Box::pin(async move {
            if let Some(error) = &self.root_error {
                return Err(storage(error));
            }
            session.validate()?;
            let root = std::path::absolute(&self.root).map_err(storage)?;
            let session = session.clone();
            let max = self.max_bytes;
            blocking(move || {
                std::fs::create_dir_all(&root).map_err(storage)?;
                let key = session_key(&session.id)?;
                let file = OpenOptions::new()
                    .create(true)
                    .truncate(false)
                    .read(true)
                    .write(true)
                    .open(root.join(format!("{key}.session.lock")))
                    .map_err(storage)?;
                fs2::FileExt::try_lock_exclusive(&file).map_err(|error| {
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || matches!(error.raw_os_error(), Some(11 | 33))
                    {
                        RuntimeError::Conflict("session metadata is owned by another writer".into())
                    } else {
                        storage(error)
                    }
                })?;
                let _lock = SessionLock(file);
                let path = root.join(format!("{key}.session.json"));
                let previous = read_session(&path, max, &session.id)?;
                let expected_next = expected_revision.map_or(Some(1), |n| n.checked_add(1));
                if previous.as_ref().map(|s| s.revision) != expected_revision
                    || Some(session.revision) != expected_next
                {
                    return Err(RuntimeError::Conflict("session revision changed".into()));
                }
                if let Some(previous) = previous {
                    if session.system_message_count != previous.system_message_count
                        || !session.messages.starts_with(&previous.messages)
                        || !session.turns.starts_with(&previous.turns)
                    {
                        return Err(RuntimeError::Conflict(
                            "session history is append-only".into(),
                        ));
                    }
                }
                atomic_write(&path, &session, max)
            })
            .await
        })
    }
}

struct SessionLock(File);
impl Drop for SessionLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

/// Session lifecycle operations perform no model/tool calls until the active
/// turn binding is committed. A durable run id is namespaced by session+turn.
pub struct SessionManager<'a> {
    store: &'a dyn SessionStore,
}
impl<'a> SessionManager<'a> {
    pub fn new(store: &'a dyn SessionStore) -> Self {
        Self { store }
    }

    pub async fn create(&self, id: &str, system: Vec<String>) -> Result<Session, RuntimeError> {
        if id.trim().is_empty() {
            return Err(RuntimeError::Configuration(
                "session id must be nonempty".into(),
            ));
        }
        let session = Session {
            version: SESSION_VERSION,
            revision: 1,
            id: id.into(),
            system_message_count: system.len(),
            messages: system.into_iter().map(Message::system).collect(),
            turns: Vec::new(),
            active: None,
        };
        self.store.save(&session, None).await?;
        Ok(session)
    }

    pub async fn load(&self, id: &str) -> Result<Session, RuntimeError> {
        let session = self
            .store
            .load(id)
            .await?
            .ok_or_else(|| RuntimeError::Conflict("session does not exist".into()))?;
        session.validate()?;
        if session.id != id {
            return Err(RuntimeError::Storage(
                "store returned a different session".into(),
            ));
        }
        Ok(session)
    }

    pub async fn begin_turn(
        &self,
        id: &str,
        turn_id: &str,
        input: &str,
    ) -> Result<SessionInput, RuntimeError> {
        if turn_id.trim().is_empty() {
            return Err(RuntimeError::Configuration(
                "turn id must be nonempty".into(),
            ));
        }
        for _ in 0..8 {
            let mut session = self.load(id).await?;
            if let Some(closed) = session
                .turns
                .iter()
                .find(|turn| turn.binding.turn_id == turn_id)
            {
                if closed.binding.input != input {
                    return Err(RuntimeError::Conflict(
                        "turn id is already bound to different input".into(),
                    ));
                }
                return session.input(&closed.binding);
            }
            if let Some(active) = &session.active {
                if active.turn_id != turn_id || active.input != input {
                    return Err(RuntimeError::Conflict(format!(
                        "session already has an active turn: {}",
                        active.turn_id
                    )));
                }
                return session.input(active);
            }
            let binding = SessionBinding {
                turn_id: turn_id.into(),
                run_id: run_id(id, turn_id)?,
                input: input.into(),
                base_message_count: session.messages.len(),
            };
            let prepared = session.input(&binding)?;
            let revision = session.revision;
            session.revision = revision
                .checked_add(1)
                .ok_or_else(|| storage("session revision overflow"))?;
            session.active = Some(binding);
            match self.store.save(&session, Some(revision)).await {
                Ok(()) => return Ok(prepared),
                Err(RuntimeError::Conflict(_)) => tokio::task::yield_now().await,
                Err(error) => return Err(error),
            }
        }
        Err(RuntimeError::Conflict(
            "session metadata is busy; retry the operation".into(),
        ))
    }

    pub async fn commit_turn(
        &self,
        id: &str,
        turn_id: &str,
        state: &RunState,
    ) -> Result<Session, RuntimeError> {
        if state.status != RunStatus::Finished || state.report.stop_reason != StopReason::Completed
        {
            return Err(RuntimeError::Conflict(
                "only a completed run can be committed as a successful session turn".into(),
            ));
        }
        self.close(id, turn_id, state, None).await
    }

    /// Close a failed terminal run only after uncertain operations are
    /// reconciled. Never-dispatched calls become explicit host error results;
    /// known observations remain in history for the following turn.
    pub async fn abandon_turn(
        &self,
        id: &str,
        turn_id: &str,
        state: &RunState,
        actor: &str,
        note: &str,
    ) -> Result<Session, RuntimeError> {
        if state.status != RunStatus::Finished
            || state.report.stop_reason == StopReason::Completed
            || actor.trim().is_empty()
            || actor.len() > 256
            || note.len() > 4096
        {
            return Err(RuntimeError::Conflict(
                "abandon requires a failed terminal run and a bounded operator acknowledgement"
                    .into(),
            ));
        }
        self.close(id, turn_id, state, Some((actor.into(), note.into())))
            .await
    }

    pub async fn run_turn(
        &self,
        durable: &DurableAgent<'_, '_>,
        id: &str,
        turn_id: &str,
        input: &str,
    ) -> Result<SessionRun, RuntimeError> {
        let prepared = self.begin_turn(id, turn_id, input).await?;
        self.drive(durable, id, prepared).await
    }

    pub async fn resume_turn(
        &self,
        durable: &DurableAgent<'_, '_>,
        id: &str,
        turn_id: &str,
    ) -> Result<SessionRun, RuntimeError> {
        let session = self.load(id).await?;
        let binding = session
            .active
            .as_ref()
            .filter(|binding| binding.turn_id == turn_id)
            .or_else(|| {
                session
                    .turns
                    .iter()
                    .find(|turn| turn.binding.turn_id == turn_id)
                    .map(|turn| &turn.binding)
            })
            .ok_or_else(|| RuntimeError::Conflict("turn is not bound to this session".into()))?;
        self.drive(durable, id, session.input(binding)?).await
    }

    async fn drive(
        &self,
        durable: &DurableAgent<'_, '_>,
        id: &str,
        input: SessionInput,
    ) -> Result<SessionRun, RuntimeError> {
        let run = durable
            .start_or_resume(&input.run_id, input.messages)
            .await?;
        let session = if run.status == RunStatus::Finished
            && run.report.stop_reason == StopReason::Completed
        {
            self.commit_turn(id, &input.turn_id, &run).await?
        } else {
            self.load(id).await?
        };
        Ok(SessionRun { session, run })
    }

    async fn close(
        &self,
        id: &str,
        turn_id: &str,
        state: &RunState,
        human: Option<(String, String)>,
    ) -> Result<Session, RuntimeError> {
        state.validate()?;
        let report_hash = fingerprint(&state.report)?;
        for _ in 0..8 {
            let mut session = self.load(id).await?;
            if let Some(closed) = session
                .turns
                .iter()
                .find(|turn| turn.binding.turn_id == turn_id)
            {
                if closed.report_fingerprint == report_hash
                    && closed.outcome == state.report.stop_reason
                    && closed.actor.as_ref() == human.as_ref().map(|pair| &pair.0)
                    && closed.note.as_ref() == human.as_ref().map(|pair| &pair.1)
                {
                    return Ok(session);
                }
                return Err(RuntimeError::Conflict(
                    "turn was already closed with different data".into(),
                ));
            }
            let binding = session
                .active
                .clone()
                .filter(|binding| binding.turn_id == turn_id)
                .ok_or_else(|| RuntimeError::Conflict("turn is not active".into()))?;
            let expected = session.input(&binding)?;
            if state.report.run_id != binding.run_id
                || state.initial_message_count != expected.messages.len()
                || !state.report.messages.starts_with(&expected.messages)
            {
                return Err(RuntimeError::Conflict(
                    "run input does not match the session turn".into(),
                ));
            }
            let mut messages = state.report.messages.clone();
            if let Some((actor, note)) = &human {
                if state.tool_steps.iter().any(|step| {
                    matches!(
                        step.progress,
                        ToolProgress::Uncertain { .. } | ToolProgress::InFlight { .. }
                    )
                }) {
                    return Err(RuntimeError::Conflict(
                        "reconcile uncertain tools before closing the session turn".into(),
                    ));
                }
                let pending = pending_calls(&messages)?;
                let calls = messages
                    .iter()
                    .filter_map(|m| match m {
                        Message::Assistant { turn } => Some(turn.tool_calls.iter()),
                        _ => None,
                    })
                    .flatten()
                    .filter(|call| pending.contains(&call.id))
                    .cloned()
                    .collect::<Vec<_>>();
                for call in calls {
                    if state.report.events.iter().any(|event|matches!(&event.kind,RunEventKind::ToolStarted {call_id,..} if call_id==&call.id)) {
                        return Err(RuntimeError::Conflict("pending tool may have been dispatched".into()));
                    }
                    messages.push(Message::Tool {result:ToolResult {call_id:call.id,output:json!({"host_error":"not executed: run ended before dispatch","stop_reason":state.report.stop_reason}),is_error:true}});
                }
                messages.push(Message::user(format!(
                    "[Host record: prior run ended with {:?}. Operator: {actor}. Note: {note}]",
                    state.report.stop_reason
                )));
            }
            validate_history(&messages)?;
            let revision = session.revision;
            session.revision = revision
                .checked_add(1)
                .ok_or_else(|| storage("session revision overflow"))?;
            session.active = None;
            session.messages = messages;
            session.turns.push(SessionTurn {
                binding,
                message_end_count: session.messages.len(),
                report_fingerprint: report_hash.clone(),
                outcome: state.report.stop_reason,
                actor: human.as_ref().map(|pair| pair.0.clone()),
                note: human.as_ref().map(|pair| pair.1.clone()),
            });
            match self.store.save(&session, Some(revision)).await {
                Ok(()) => return Ok(session),
                Err(RuntimeError::Conflict(_)) => tokio::task::yield_now().await,
                Err(error) => return Err(error),
            }
        }
        Err(RuntimeError::Conflict(
            "session metadata is busy; retry the operation".into(),
        ))
    }
}

fn validate_binding(id: &str, binding: &SessionBinding, base: usize) -> Result<(), RuntimeError> {
    if binding.turn_id.trim().is_empty()
        || binding.base_message_count != base
        || binding.run_id != run_id(id, &binding.turn_id)?
    {
        return Err(storage("invalid session turn binding"));
    }
    Ok(())
}

fn run_id(id: &str, turn: &str) -> Result<String, RuntimeError> {
    Ok(format!("session-{}", fingerprint(&(id, turn))?))
}
fn session_key(id: &str) -> Result<String, RuntimeError> {
    if id.trim().is_empty() {
        return Err(RuntimeError::Configuration(
            "session id must be nonempty".into(),
        ));
    }
    Ok(hex::encode(Sha256::digest(id.as_bytes())))
}
fn read_session(path: &Path, max: u64, id: &str) -> Result<Option<Session>, RuntimeError> {
    let session = read_json::<Session>(path, max)?;
    if let Some(session) = &session {
        session.validate()?;
        if session.id != id {
            return Err(storage("stored session identity changed"));
        }
    }
    Ok(session)
}
