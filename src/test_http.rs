//! A one-shot local HTTP server for provider tests.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serves one request: answers `status` with `body` (as server-sent events
/// when `sse`), sending the body in `chunks` with `pause` between them.
/// Returns the base URL and a handle yielding the raw request.
pub(crate) async fn serve_once(
    status: u16,
    sse: bool,
    chunks: Vec<String>,
    pause: Duration,
) -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            let read = socket.read(&mut buffer).await.unwrap();
            request.extend_from_slice(&buffer[..read]);
            let text = String::from_utf8_lossy(&request);
            if let Some(head_end) = text.find("\r\n\r\n") {
                let length = text[..head_end]
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|value| value.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                if request.len() >= head_end + 4 + length {
                    break;
                }
            }
            if read == 0 {
                break;
            }
        }
        let content_type = if sse {
            "text/event-stream"
        } else {
            "application/json"
        };
        let head = format!(
            "HTTP/1.1 {status} OK\r\ncontent-type: {content_type}\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n"
        );
        socket.write_all(head.as_bytes()).await.unwrap();
        for (index, chunk) in chunks.iter().enumerate() {
            if index > 0 {
                tokio::time::sleep(pause).await;
            }
            let framed = format!("{:x}\r\n{chunk}\r\n", chunk.len());
            if socket.write_all(framed.as_bytes()).await.is_err() {
                break;
            }
        }
        let _ = socket.write_all(b"0\r\n\r\n").await;
        String::from_utf8_lossy(&request).into_owned()
    });
    (format!("http://{address}"), handle)
}
