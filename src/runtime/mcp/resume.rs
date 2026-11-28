//! Explicit process-local SSE cursor recovery; never replays an MCP POST.
use super::{error, RuntimeError};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpSsePolicy {
    pub max_pending: usize,
    pub max_resumptions: usize,
    pub max_event_id_bytes: usize,
    pub max_events_per_request: usize,
}
impl Default for McpSsePolicy {
    fn default() -> Self {
        Self {
            max_pending: 16,
            max_resumptions: 1,
            max_event_id_bytes: 256,
            max_events_per_request: 256,
        }
    }
}
impl McpSsePolicy {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if !(1..=64).contains(&self.max_pending)
            || !(1..=8).contains(&self.max_resumptions)
            || !(1..=1024).contains(&self.max_event_id_bytes)
            || !(1..=1024).contains(&self.max_events_per_request)
        {
            return Err(error("invalid SSE resumption limits"));
        }
        Ok(())
    }
}
/// Host-only process-local handle; no Deserialize/Debug or stored request body.
/// None means no complete resumable cursor was observed; GET is then refused.
#[derive(Clone)]
pub struct McpSseResume {
    connection: String,
    request: u64,
    binding: String,
    event: Option<String>,
}
impl McpSseResume {
    pub fn request_id(&self) -> u64 {
        self.request
    }
    pub fn request_binding(&self) -> &str {
        &self.binding
    }
    pub fn last_event_id(&self) -> Option<&str> {
        self.event.as_deref()
    }
}
struct Pending {
    binding: String,
    event: Option<String>,
    seen: BTreeSet<String>,
    active: bool,
    attempts: usize,
}
struct State {
    closed: bool,
    pending: BTreeMap<u64, Pending>,
}
#[derive(Clone)]
pub(super) struct Registry {
    identity: String,
    policy: McpSsePolicy,
    state: Arc<Mutex<State>>,
}
impl Registry {
    pub(super) fn new(policy: McpSsePolicy) -> Result<Self, RuntimeError> {
        policy.validate()?;
        Ok(Self {
            identity: uuid::Uuid::new_v4().simple().to_string(),
            policy,
            state: Arc::new(Mutex::new(State {
                closed: false,
                pending: BTreeMap::new(),
            })),
        })
    }
    pub(super) fn start(&self, request: u64, binding: String) -> Result<Lease, RuntimeError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| error("SSE checkpoint lock poisoned"))?;
        if state.closed
            || state.pending.len() >= self.policy.max_pending
            || state.pending.contains_key(&request)
        {
            return Err(error(
                "SSE pending request limit/session state refused before POST",
            ));
        }
        state.pending.insert(
            request,
            Pending {
                binding,
                event: None,
                seen: BTreeSet::new(),
                active: true,
                attempts: 0,
            },
        );
        Ok(Lease {
            registry: self.clone(),
            request,
        })
    }
    pub(super) fn list(&self) -> Result<Vec<McpSseResume>, RuntimeError> {
        let state = self
            .state
            .lock()
            .map_err(|_| error("SSE checkpoint lock poisoned"))?;
        Ok(state
            .pending
            .iter()
            .filter(|(_, v)| !v.active)
            .map(|(id, v)| McpSseResume {
                connection: self.identity.clone(),
                request: *id,
                binding: v.binding.clone(),
                event: v.event.clone(),
            })
            .collect())
    }
    pub(super) fn claim(&self, handle: &McpSseResume) -> Result<Lease, RuntimeError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| error("SSE checkpoint lock poisoned"))?;
        if state.closed || handle.connection != self.identity {
            return Err(error("SSE resume session identity mismatch"));
        }
        let entry = state
            .pending
            .get_mut(&handle.request)
            .ok_or_else(|| error("SSE checkpoint no longer pending"))?;
        if entry.active
            || entry.binding != handle.binding
            || entry.event != handle.event
            || entry.event.is_none()
            || entry.attempts >= self.policy.max_resumptions
        {
            return Err(error(
                "SSE cursor unavailable/stale/in use or GET attempt limit exceeded",
            ));
        }
        // Reserve the explicit GET attempt before network dispatch. Cancelled
        // or uncertain attempts are never automatically refunded/retried.
        entry.attempts += 1;
        entry.active = true;
        Ok(Lease {
            registry: self.clone(),
            request: handle.request,
        })
    }
    pub(super) fn discard(&self, handle: &McpSseResume) -> Result<(), RuntimeError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| error("SSE checkpoint lock poisoned"))?;
        if handle.connection != self.identity {
            return Err(error("SSE checkpoint source mismatch"));
        }
        let entry = state
            .pending
            .get(&handle.request)
            .ok_or_else(|| error("SSE checkpoint missing"))?;
        if entry.active || entry.binding != handle.binding || entry.event != handle.event {
            return Err(error("cannot discard active/stale SSE checkpoint"));
        }
        state.pending.remove(&handle.request);
        Ok(())
    }
    pub(super) fn close(&self) -> Result<(), RuntimeError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| error("SSE checkpoint lock poisoned"))?;
        state.closed = true;
        state.pending.clear();
        Ok(())
    }
}
pub(super) struct Lease {
    registry: Registry,
    request: u64,
}
impl Lease {
    /// Record only after the complete event has been validated/processed. A
    /// callback interrupted before that point cannot silently skip its event.
    pub(super) fn event(&self, id: Option<&str>) -> Result<(), RuntimeError> {
        let Some(id) = id else {
            return Ok(());
        };
        if id.is_empty()
            || id.len() > self.registry.policy.max_event_id_bytes
            || !id.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(error("invalid/unbounded SSE event ID"));
        }
        let mut state = self
            .registry
            .state
            .lock()
            .map_err(|_| error("SSE checkpoint lock poisoned"))?;
        let entry = state
            .pending
            .get_mut(&self.request)
            .ok_or_else(|| error("SSE checkpoint closed"))?;
        if entry.seen.len() >= self.registry.policy.max_events_per_request
            || !entry.seen.insert(id.to_owned())
        {
            return Err(error(
                "SSE event ID repeated or event count limit exceeded; no callback replay",
            ));
        }
        entry.event = Some(id.to_owned());
        Ok(())
    }
    pub(super) fn precheck(&self, id: Option<&str>) -> Result<(), RuntimeError> {
        let Some(id) = id else {
            return Ok(());
        };
        let state = self
            .registry
            .state
            .lock()
            .map_err(|_| error("SSE checkpoint lock poisoned"))?;
        let entry = state
            .pending
            .get(&self.request)
            .ok_or_else(|| error("SSE checkpoint closed"))?;
        if id.is_empty()
            || id.len() > self.registry.policy.max_event_id_bytes
            || !id.bytes().all(|b| b.is_ascii_graphic())
            || entry.seen.contains(id)
            || entry.seen.len() >= self.registry.policy.max_events_per_request
        {
            return Err(error(
                "invalid/replayed/overlimit SSE event ID; no callback replay",
            ));
        }
        Ok(())
    }
    pub(super) fn complete(&self) -> Result<(), RuntimeError> {
        self.registry
            .state
            .lock()
            .map_err(|_| error("SSE checkpoint lock poisoned"))?
            .pending
            .remove(&self.request);
        Ok(())
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.registry.state.lock() {
            if let Some(entry) = state.pending.get_mut(&self.request) {
                entry.active = false;
            }
        }
    }
}
