//! Attach to a running Chrome over `ws://` (`--remote-debugging-port`, B-3).

use crate::{Transport, TransportError};
use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

const OUTBOUND_QUEUE: usize = 256;
const INBOUND_QUEUE: usize = 1024;
/// How often [`devtools_active_port`] re-checks for the file while waiting for it.
const PORT_FILE_POLL: Duration = Duration::from_millis(20);

/// A CDP connection to `ws://host:port/devtools/browser/<id>`.
pub struct WebSocket {
    tx: mpsc::Sender<Bytes>,
    inbound: Mutex<Option<mpsc::Receiver<Bytes>>>,
    tasks: [JoinHandle<()>; 2],
}

fn io(e: impl std::fmt::Display) -> TransportError {
    TransportError::Io(std::io::Error::other(e.to_string()))
}

impl WebSocket {
    /// Connect to a browser (or page) debugger URL. Must run inside a tokio runtime.
    pub async fn connect(url: &str) -> Result<WebSocket, TransportError> {
        let (stream, _) = tokio_tungstenite::connect_async(url).await.map_err(io)?;
        let (mut sink, mut source) = stream.split();
        let (tx, mut out_rx) = mpsc::channel::<Bytes>(OUTBOUND_QUEUE);
        let (in_tx, in_rx) = mpsc::channel::<Bytes>(INBOUND_QUEUE);
        let writer = tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                // CDP frames are JSON text; a non-UTF-8 message cannot be valid CDP.
                let Ok(text) = String::from_utf8(msg.to_vec()) else {
                    continue;
                };
                if sink.send(Message::text(text)).await.is_err() {
                    return;
                }
            }
            let _ = sink.close().await;
        });
        let reader = tokio::spawn(async move {
            while let Some(frame) = source.next().await {
                let bytes = match frame {
                    Ok(Message::Text(t)) => Bytes::from(t),
                    Ok(Message::Binary(b)) => b,
                    Ok(Message::Close(_)) | Err(_) => return,
                    Ok(_) => continue,
                };
                if in_tx.send(bytes).await.is_err() {
                    return;
                }
            }
        });
        Ok(WebSocket {
            tx,
            inbound: Mutex::new(Some(in_rx)),
            tasks: [writer, reader],
        })
    }
}

impl Drop for WebSocket {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

#[async_trait]
impl Transport for WebSocket {
    async fn send(&self, msg: Bytes) -> Result<(), TransportError> {
        self.tx.send(msg).await.map_err(|_| TransportError::Closed)
    }

    fn recv(&self) -> mpsc::Receiver<Bytes> {
        self.inbound
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .unwrap_or_else(|| mpsc::channel(1).1)
    }
}

/// Parse a `DevToolsActivePort` file: the port on line 1, the browser target path on line 2.
pub fn parse_devtools_active_port(text: &str) -> Option<String> {
    let mut lines = text.lines();
    let port: u16 = lines.next()?.trim().parse().ok()?;
    let path = lines.next()?.trim();
    if port == 0 || !path.starts_with("/devtools/browser/") {
        return None;
    }
    Some(format!("ws://127.0.0.1:{port}{path}"))
}

/// Wait, at most `cap`, until Chrome started with `--remote-debugging-port=0` has written
/// a complete `<user_data_dir>/DevToolsActivePort`, and return its browser `ws://` URL.
/// The condition is the file's content; Chrome offers no event for it, so it is re-read on
/// a short interval under the cap.
pub async fn devtools_active_port(
    user_data_dir: &Path,
    cap: Duration,
) -> Result<String, TransportError> {
    let file = user_data_dir.join("DevToolsActivePort");
    let wait = async {
        let mut tick = tokio::time::interval(PORT_FILE_POLL);
        loop {
            tick.tick().await;
            if let Ok(text) = tokio::fs::read_to_string(&file).await {
                if let Some(url) = parse_devtools_active_port(&text) {
                    return url;
                }
            }
        }
    };
    tokio::time::timeout(cap, wait).await.map_err(|_| {
        io(format!(
            "{} not written within {} ms",
            file.display(),
            cap.as_millis()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_port_file() {
        assert_eq!(
            parse_devtools_active_port("9222\n/devtools/browser/abc-1\n").as_deref(),
            Some("ws://127.0.0.1:9222/devtools/browser/abc-1")
        );
        assert_eq!(parse_devtools_active_port("9222\n"), None);
        assert_eq!(parse_devtools_active_port("x\n/devtools/browser/a"), None);
        assert_eq!(parse_devtools_active_port("0\n/devtools/browser/a"), None);
    }

    #[tokio::test(start_paused = true)]
    async fn port_file_wait_is_capped() {
        let dir = std::env::temp_dir().join(format!("sb-cdp-noport-{}", std::process::id()));
        let r = devtools_active_port(&dir, Duration::from_secs(2)).await;
        assert!(r.unwrap_err().to_string().contains("DevToolsActivePort"));
    }
}
