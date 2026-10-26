//! sb-cdp (B-3, D-600). I-8: the [`Transport`] trait and its backends, plus the CDP client
//! [`Cdp`]: one writer task, one reader task, reply map by id, flattened sessions, pipelined
//! sends, events fanned out per session, 10 s command timeout (B-4 #11). Backends implement
//! only bytes in / bytes out.

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::mpsc::Receiver;

mod client;
mod pipe;
mod ws;

pub use client::{Cdp, CdpError, CdpSender, Event, Observer, COMMAND_TIMEOUT};
pub use pipe::{process_running, Pipe, DROP_KILL_CAP};
pub use ws::{devtools_active_port, parse_devtools_active_port, WebSocket};

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

/// Stub until needed (B-3).
pub struct ElectronNapi {
    _private: (),
}

/// Stub until needed (B-3).
pub struct ExtensionRelay {
    _private: (),
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
