//! sb-cdp (B-3). DRAFT signature for I-8, owned by area driver (T-18).
//! One writer task, one reader task, reply map by id, flattened sessions, pipelined sends,
//! events fanned out per session. Backends implement only bytes in / bytes out.

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::mpsc::Receiver;

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("transport closed")]
    Closed,
    #[error("transport io: {0}")]
    Io(#[from] std::io::Error),
}

/// I-8. `send` writes one CDP message; `recv` hands out the inbound stream (call once: a
/// second call returns an already-closed receiver).
#[async_trait]
pub trait Transport: Send + Sync {
    async fn send(&self, msg: Bytes) -> Result<(), TransportError>;
    fn recv(&self) -> Receiver<Bytes>;
}

/// `--remote-debugging-pipe` (default). On Windows Chromium reads the pipe from inherited
/// CRT fds 3/4; std::process cannot pass those, so T-18 needs the lpReserved2 handle
/// inheritance libuv uses, or `--remote-debugging-io-pipes` (verify against the D-34 build).
pub struct Pipe {
    _private: (),
}

/// Attach to a running Chrome over `ws://` (`--remote-debugging-port`).
pub struct WebSocket {
    _private: (),
}

/// Stub until needed (B-3).
pub struct ElectronNapi {
    _private: (),
}

/// Stub until needed (B-3).
pub struct ExtensionRelay {
    _private: (),
}

#[async_trait]
impl Transport for Pipe {
    async fn send(&self, msg: Bytes) -> Result<(), TransportError> {
        let _ = msg;
        todo!("T-18: B-3 pipe")
    }
    fn recv(&self) -> Receiver<Bytes> {
        todo!("T-18: B-3 pipe")
    }
}

#[async_trait]
impl Transport for WebSocket {
    async fn send(&self, msg: Bytes) -> Result<(), TransportError> {
        let _ = msg;
        todo!("T-18: B-3 websocket")
    }
    fn recv(&self) -> Receiver<Bytes> {
        todo!("T-18: B-3 websocket")
    }
}

#[async_trait]
impl Transport for ElectronNapi {
    async fn send(&self, msg: Bytes) -> Result<(), TransportError> {
        let _ = msg;
        todo!("B-3: stub until needed")
    }
    fn recv(&self) -> Receiver<Bytes> {
        todo!("B-3: stub until needed")
    }
}

#[async_trait]
impl Transport for ExtensionRelay {
    async fn send(&self, msg: Bytes) -> Result<(), TransportError> {
        let _ = msg;
        todo!("B-3: stub until needed")
    }
    fn recv(&self) -> Receiver<Bytes> {
        todo!("B-3: stub until needed")
    }
}
