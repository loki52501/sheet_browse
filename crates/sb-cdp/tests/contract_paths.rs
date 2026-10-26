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

/// B-604 (additive, pending in interface_changes): the paths it names resolve.
#[test]
fn b604_additive_paths_resolve() {
    let _ = sb_cdp::Cdp::new;
    let _ = sb_cdp::Cdp::with_observer;
    let _ = |s: &sb_cdp::CdpSender| {
        s.start_then(None, "Page.getFrameTree", serde_json::json!({}), |_| {})
    };
    let _ = sb_cdp::CdpSender::pending;
    let _ = sb_cdp::Pipe::launch;
    let _ = sb_cdp::Pipe::pids;
    let _ = sb_cdp::Pipe::kill;
    let _ = sb_cdp::process_running;
    let _ = sb_cdp::parse_devtools_active_port;
    let _: Option<sb_cdp::Observer> = None;
    let _: Option<sb_cdp::Event> = None;
    assert_eq!(sb_cdp::COMMAND_TIMEOUT.as_secs(), 10);
    assert!(sb_cdp::DROP_KILL_CAP.as_secs() > 0);
}
