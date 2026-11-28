//! Native Messaging host registration status operations (05 §2.1).

use serde::{Deserialize, Serialize};

use crate::BrowserKind;

/// Native Messaging host registration state for a browser family (05 §2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserHostRegistration {
    /// The registry key and manifest exist and point to the current `quartz-host.exe`.
    Registered,
    /// The registry key or manifest is missing, unreadable, or points to another path.
    NeedsRepair,
    /// The `quartz-host.exe` executable is missing beside the running core executable.
    HostMissing,
}

/// Whether at least one extension profile for a browser family is currently connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "bool", into = "bool")]
pub enum BrowserHostConnection {
    /// At least one live browser stream for this browser is connected to core.
    Connected,
    /// No live browser stream for this browser is currently connected.
    Disconnected,
}

impl From<bool> for BrowserHostConnection {
    fn from(value: bool) -> Self {
        if value {
            Self::Connected
        } else {
            Self::Disconnected
        }
    }
}

impl From<BrowserHostConnection> for bool {
    fn from(value: BrowserHostConnection) -> Self {
        value == BrowserHostConnection::Connected
    }
}

/// Registration and live connection state for one supported browser family (05 §2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserHostEntry {
    /// Browser family (`chrome`, `edge`, or `firefox`).
    pub browser: BrowserKind,
    /// Current Native Messaging registry and manifest status.
    pub registration: BrowserHostRegistration,
    /// Whether any profile of this browser is currently connected.
    pub connected: BrowserHostConnection,
}

/// Empty `browser_hosts.status` request body.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserHostsStatusRequest {}

/// Response body returned by `browser_hosts.status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserHostsStatusResponse {
    /// Registration and connection state for Chrome, Edge, and Firefox in that fixed order.
    pub hosts: Vec<BrowserHostEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CoreRequest, Op};

    fn snapshot<T: serde::de::DeserializeOwned + Serialize>(value: &serde_json::Value) {
        let decoded: T =
            serde_json::from_value(value.clone()).expect("typed browser_hosts wire value");
        assert_eq!(
            serde_json::to_value(decoded).expect("browser_hosts JSON"),
            *value
        );
    }

    #[test]
    fn browser_hosts_wire_snapshots_cover_status_request_and_response() {
        let wire: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/browser_hosts.json"))
                .expect("browser_hosts wire snapshots");
        snapshot::<BrowserHostsStatusRequest>(&wire["status_request"]);
        snapshot::<BrowserHostsStatusResponse>(&wire["status_response"]);

        assert_eq!(BrowserHostsStatusRequest::OP, Op::BrowserHostsStatus);

        let decoded: BrowserHostsStatusResponse =
            BrowserHostsStatusRequest::response(wire["status_response"].clone())
                .expect("status response");
        assert_eq!(
            decoded.hosts,
            vec![
                BrowserHostEntry {
                    browser: BrowserKind::Chrome,
                    registration: BrowserHostRegistration::Registered,
                    connected: BrowserHostConnection::Connected,
                },
                BrowserHostEntry {
                    browser: BrowserKind::Edge,
                    registration: BrowserHostRegistration::NeedsRepair,
                    connected: BrowserHostConnection::Disconnected,
                },
                BrowserHostEntry {
                    browser: BrowserKind::Firefox,
                    registration: BrowserHostRegistration::HostMissing,
                    connected: BrowserHostConnection::Disconnected,
                },
            ]
        );

        assert!(
            serde_json::from_value::<BrowserHostsStatusRequest>(serde_json::json!({"extra": true}))
                .is_err()
        );
    }
}
