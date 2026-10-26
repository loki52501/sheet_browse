//! I-8 contract: every path `agent-memory/interfaces.yaml` lists for sb-cdp resolves, and
//! every backend implements the trait. A wrong path fails compilation.

use sb_cdp::{ElectronNapi, ExtensionRelay, Pipe, Transport, TransportError, WebSocket};
use std::sync::Arc;

fn is_transport<T: Transport + 'static>() {}

#[test]
fn i8_paths_resolve() {
    is_transport::<Pipe>();
    is_transport::<WebSocket>();
    is_transport::<ElectronNapi>();
    is_transport::<ExtensionRelay>();
    let _: Option<Arc<dyn Transport>> = None;
    let e = TransportError::Closed;
    assert_eq!(e.to_string(), "transport closed");
    let io: TransportError = std::io::Error::other("x").into();
    assert!(matches!(io, TransportError::Io(_)));
}
