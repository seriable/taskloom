//! Server-sent events of a streaming HTTP response, with an idle timeout.

use std::time::Duration;

use futures_util::StreamExt;

use crate::gateway::ModelError;

/// One event: its `event:` name (if any) and its joined `data:` lines.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Reads `response` as server-sent events, calling `on_event` for each until
/// it returns `Ok(true)` (done) or the stream ends. With `idle`, no bytes for
/// that long fail as transient.
/// Returns whether the callback confirmed completion. EOF alone does not
/// confirm that a provider finished its response; the caller must check it.
pub(crate) async fn read_events(
    response: reqwest::Response,
    idle: Option<Duration>,
    mut on_event: impl FnMut(SseEvent) -> Result<bool, ModelError>,
) -> Result<bool, ModelError> {
    let mut stream = response.bytes_stream();
    let mut parser = Parser::default();
    loop {
        let next = match idle {
            Some(limit) => tokio::time::timeout(limit, stream.next())
                .await
                .map_err(|_| {
                    ModelError::transient(
                        None,
                        format!(
                            "the provider sent no output for {}s (idle timeout)",
                            limit.as_secs()
                        ),
                    )
                })?,
            None => stream.next().await,
        };
        let Some(chunk) = next else {
            break;
        };
        let bytes = chunk.map_err(|error| {
            ModelError::from_transport_error(
                &error,
                format!("read the answer stream failed: {error}"),
            )
        })?;
        for event in parser.push(&bytes) {
            if on_event(event)? {
                return Ok(true);
            }
        }
    }
    if let Some(event) = parser.finish() {
        return on_event(event);
    }
    Ok(false)
}

/// Splits bytes into events (`\n` or `\r\n` lines, a blank line ends one).
#[derive(Default)]
struct Parser {
    buffer: Vec<u8>,
    current: SseEvent,
    has_data: bool,
}

impl Parser {
    fn push(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(position) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let mut line = self.buffer.drain(..=position).collect::<Vec<_>>();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line).into_owned();
            if let Some(event) = self.line(&line) {
                events.push(event);
            }
        }
        events
    }

    fn line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            return self.take();
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "data" => {
                if self.has_data {
                    self.current.data.push('\n');
                }
                self.current.data.push_str(value);
                self.has_data = true;
            }
            "event" => self.current.event = Some(value.to_string()),
            _ => {}
        }
        None
    }

    fn take(&mut self) -> Option<SseEvent> {
        if !self.has_data && self.current.event.is_none() {
            return None;
        }
        self.has_data = false;
        Some(std::mem::take(&mut self.current))
    }

    fn finish(&mut self) -> Option<SseEvent> {
        if !self.buffer.is_empty() {
            let line = String::from_utf8_lossy(&std::mem::take(&mut self.buffer)).into_owned();
            let _ = self.line(line.trim_end_matches('\r'));
        }
        self.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_split_on_blank_lines_across_chunks() {
        let mut parser = Parser::default();
        let mut events = parser.push(b"event: delta\r\ndata: {\"a\"");
        assert!(events.is_empty());
        events.extend(parser.push(b":1}\r\n\r\n: comment\ndata: x\ndata: y\n\n"));
        assert_eq!(
            events,
            vec![
                SseEvent {
                    event: Some("delta".into()),
                    data: "{\"a\":1}".into()
                },
                SseEvent {
                    event: None,
                    data: "x\ny".into()
                },
            ]
        );
        assert_eq!(parser.push(b"data: [DONE]").len(), 0);
        assert_eq!(parser.finish().unwrap().data, "[DONE]");
    }
}
