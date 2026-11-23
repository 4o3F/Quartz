//! Version negotiation and role-specific handshake bodies (04 §4 and 05 §1.2).

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use snafu::Snafu;

/// Current IPC protocol version, independent of the database schema version (04 §4).
pub const PROTOCOL_VERSION: ProtoVersion = ProtoVersion(1);

/// An IPC protocol version number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProtoVersion(pub u32);

/// Inclusive supported protocol versions, always ordered from minimum to maximum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ProtoRangeWire")]
pub struct ProtoRange {
    min: ProtoVersion,
    max: ProtoVersion,
}

#[derive(Deserialize)]
struct ProtoRangeWire {
    min: ProtoVersion,
    max: ProtoVersion,
}

impl TryFrom<ProtoRangeWire> for ProtoRange {
    type Error = ParseProtoRangeError;

    fn try_from(value: ProtoRangeWire) -> Result<Self, Self::Error> {
        Self::new(value.min, value.max)
    }
}

impl ProtoRange {
    /// Validates an inclusive protocol range.
    ///
    /// # Errors
    /// Rejects a minimum greater than the maximum.
    pub fn new(min: ProtoVersion, max: ProtoVersion) -> Result<Self, ParseProtoRangeError> {
        if min > max {
            return Err(ParseProtoRangeError { min, max });
        }
        Ok(Self { min, max })
    }

    /// Lowest supported version.
    #[must_use]
    pub const fn min(self) -> ProtoVersion {
        self.min
    }

    /// Highest supported version.
    #[must_use]
    pub const fn max(self) -> ProtoVersion {
        self.max
    }

    /// Selects the highest version common to the core and client ranges.
    ///
    /// # Errors
    /// Disjoint ranges return both inputs for diagnostic and response construction.
    pub fn negotiate(self, client: Self) -> Result<ProtoVersion, NegotiateProtoError> {
        let highest = self.max.min(client.max);
        if highest < self.min.max(client.min) {
            return Err(NegotiateProtoError {
                supported: self,
                client,
            });
        }
        Ok(highest)
    }
}

/// A protocol range has reversed endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]
#[snafu(display("protocol minimum {min:?} exceeds maximum {max:?}"))]
pub struct ParseProtoRangeError {
    /// Supplied minimum.
    pub min: ProtoVersion,
    /// Supplied maximum.
    pub max: ProtoVersion,
}

/// The core and client have no common protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]
#[snafu(display("disjoint protocol ranges: core {supported:?}, client {client:?}"))]
pub struct NegotiateProtoError {
    /// Core's supported range, sent in `proto_mismatch`.
    pub supported: ProtoRange,
    /// Client's requested range.
    pub client: ProtoRange,
}

/// Canonical UUID text in the 8-4-4-4-12 hexadecimal form.
///
/// Parsing accepts either hex case and stores lowercase for stable identity comparisons.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Uuid(String);

/// A UUID is not canonical hexadecimal text separated as 8-4-4-4-12.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]
#[snafu(display("expected a UUID in 8-4-4-4-12 hexadecimal form"))]
pub struct ParseUuidError;

/// The canonical UUID representation contains 32 hex digits and four hyphens.
const UUID_TEXT_LEN: usize = 36;
/// Hyphens separate the canonical UUID's five hexadecimal groups.
const UUID_HYPHENS: [usize; 4] = [8, 13, 18, 23];

impl FromStr for Uuid {
    type Err = ParseUuidError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != UUID_TEXT_LEN
            || !value.bytes().enumerate().all(|(index, byte)| {
                if UUID_HYPHENS.contains(&index) {
                    byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            })
        {
            return Err(ParseUuidError);
        }
        Ok(Self(value.to_ascii_lowercase()))
    }
}

impl TryFrom<String> for Uuid {
    type Error = ParseUuidError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<Uuid> for String {
    fn from(value: Uuid) -> Self {
        value.0
    }
}

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The client program's release version, independent of its protocol range.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClientVersion(pub String);

/// The core program's release version.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CoreVersion(pub String);

/// Core-start identity; the pipe server generates a new value on each start.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InstanceId(pub String);

/// Browser extension identity checked against the registered extensions by the server.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExtensionId(pub String);

/// Persisted browser-background start counter (05 §1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Epoch(pub u64);

/// Browser implementation self-reported by the extension (05 §1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserKind {
    /// Google Chrome.
    Chrome,
    /// Microsoft Edge.
    Edge,
    /// Mozilla Firefox, also covered by the browser design.
    Firefox,
    /// An implementation added by a later protocol; runtime policy decides support.
    #[serde(other, skip_serializing)]
    Unknown,
}

/// Browser extension's `browser.hello` body from 05 §1.2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserHello {
    /// Stable identity stored in this browser profile's local storage.
    pub profile_instance_id: Uuid,
    /// Identity generated for the current browser session.
    pub browser_session_id: Uuid,
    /// Incremented each time the extension background starts.
    pub epoch: Epoch,
    /// Extension's supported protocol versions.
    pub proto: ProtoRange,
    /// Self-reported browser implementation.
    pub browser: BrowserKind,
}

/// Host handshake adds the extension identity supplied by the browser launch arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostHello {
    /// Flattened extension handshake fields.
    #[serde(flatten)]
    pub browser: BrowserHello,
    /// Registered identity used for server-side authorization.
    pub extension_id: ExtensionId,
}

/// UI lifecycle state declared during the handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiState {
    /// The UI process is starting.
    Starting,
    /// Resources are being prepared before the window is shown.
    Prewarming,
    /// The window is visible.
    Visible,
}

/// UI-specific handshake payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiHello {
    /// Current UI lifecycle state.
    pub state: UiState,
}

/// A declared role and its required role-specific payload, serialized beside `role`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum HelloRole {
    /// UI clients must declare their current lifecycle state.
    Ui {
        /// Payload carried in the `ui` object.
        ui: UiHello,
    },
    /// Hosts must identify their browser stream and extension.
    Host {
        /// Payload carried in the `browser` object.
        browser: HostHello,
    },
    /// Command-line clients have no role-specific payload.
    Cli,
}

impl HelloRole {
    /// Returns the permission role declared by this handshake.
    #[must_use]
    pub const fn role(&self) -> crate::Role {
        match self {
            Self::Ui { .. } => crate::Role::Ui,
            Self::Host { .. } => crate::Role::Host,
            Self::Cli => crate::Role::Cli,
        }
    }
}

/// First core request body, with role data at the same level as protocol and version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// Client's supported core protocol range.
    pub proto: ProtoRange,
    /// Version of the connecting program.
    pub client_version: ClientVersion,
    /// Declared role and its associated payload.
    #[serde(flatten)]
    pub role: HelloRole,
}

/// Optional protocol features reported by the core.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Capability {
    /// Browser activity reporting version 1.
    #[serde(rename = "browser.v1")]
    BrowserV1,
    /// Annotation operations version 1.
    #[serde(rename = "annotations.v1")]
    AnnotationsV1,
    /// Visible document rankings, extraction, references and details version 1.
    #[serde(rename = "query.documents.v1")]
    QueryDocumentsV1,
    /// Grouped unclassified active work version 1.
    #[serde(rename = "query.unclassified.v1")]
    QueryUnclassifiedV1,
    /// A feature not understood by this client.
    #[serde(other, skip_serializing)]
    Unknown,
}

/// Successful core handshake body from 04 §4.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloResponse {
    /// Highest common negotiated protocol version.
    pub proto: ProtoVersion,
    /// Core program version.
    pub core_version: CoreVersion,
    /// Identity generated for this core start.
    pub instance_id: InstanceId,
    /// Optional protocol features supported by this core.
    pub capabilities: Vec<Capability>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_ui_and_core_hello_examples_parse() {
        let hello: Hello = serde_json::from_value(serde_json::json!({
            "proto":{"min":1,"max":1}, "role":"ui", "client_version":"0.1.0",
            "ui":{"state":"prewarming", "future":true}, "future":true
        }))
        .expect("04 UI hello");
        assert_eq!(hello.role.role(), crate::Role::Ui);
        assert_eq!(hello.proto.min(), PROTOCOL_VERSION);
        assert_eq!(
            serde_json::to_value(&hello).expect("serialize")["ui"]["state"],
            "prewarming"
        );
        let response: HelloResponse = serde_json::from_value(serde_json::json!({
            "proto":1,"core_version":"0.1.0","instance_id":"8c1e...",
            "capabilities":["browser.v1","annotations.v1","future.v2"],"future":true
        }))
        .expect("04 response");
        assert_eq!(
            response.capabilities,
            [
                Capability::BrowserV1,
                Capability::AnnotationsV1,
                Capability::Unknown
            ]
        );
        assert!(
            serde_json::to_value(&response).is_err(),
            "unknown capabilities are receive-only"
        );
        assert!(
            serde_json::from_str::<Hello>(
                r#"{"proto":{"min":1,"max":1},"client_version":"1","role":"ui"}"#
            )
            .is_err()
        );
    }

    #[test]
    fn browser_and_host_hello_follow_05_fields() {
        let browser = serde_json::json!({
            "profile_instance_id":"a286737e-3855-43cb-98ee-c366344372d0",
            "browser_session_id":"be5a458b-e2b2-417a-b236-ab5491e2ec4d",
            "epoch":12,"proto":{"min":1,"max":1},"browser":"edge"
        });
        let parsed: BrowserHello = serde_json::from_value(browser.clone()).expect("05 example");
        assert_eq!(parsed.browser, BrowserKind::Edge);
        let mut host_browser = browser;
        host_browser["extension_id"] = serde_json::json!("extension-id");
        host_browser["future"] = serde_json::json!(42);
        let host = serde_json::json!({"proto":{"min":1,"max":1},"client_version":"0.1.0", "role":"host","browser":host_browser});
        let parsed: Hello =
            serde_json::from_value(host.clone()).expect("host hello with extension identity");
        assert_eq!(parsed.role.role(), crate::Role::Host);
        let encoded = serde_json::to_value(&parsed).expect("serialize host hello");
        assert_eq!(encoded["browser"]["browser"], "edge");
        assert_eq!(encoded["browser"]["epoch"], 12);
        assert_eq!(encoded["browser"]["extension_id"], "extension-id");
        let mut missing_id = host;
        missing_id["browser"]
            .as_object_mut()
            .expect("object")
            .remove("extension_id");
        assert!(serde_json::from_value::<Hello>(missing_id).is_err());
        assert_eq!(
            serde_json::from_str::<BrowserKind>("\"firefox\"").expect("Firefox in 05"),
            BrowserKind::Firefox
        );
        assert_eq!(
            serde_json::from_str::<BrowserKind>("\"future\"").expect("future browser"),
            BrowserKind::Unknown
        );
    }

    #[test]
    fn uuid_shape_is_validated_and_case_normalized() {
        for bad in [
            "",
            "8c1e...",
            "a286737e385543cb98eec366344372d0",
            "a286737e_3855-43cb-98ee-c366344372d0",
            "g286737e-3855-43cb-98ee-c366344372d0",
            "a286737e-3855-43cb-98ee-c366344372d00",
        ] {
            assert!(bad.parse::<Uuid>().is_err(), "{bad}");
            assert!(
                serde_json::from_value::<Uuid>(serde_json::json!(bad)).is_err(),
                "{bad}"
            );
        }
        let uuid: Uuid = "A286737E-3855-43CB-98EE-C366344372D0"
            .parse()
            .expect("canonical UUID");
        assert_eq!(uuid.to_string(), "a286737e-3855-43cb-98ee-c366344372d0");
        assert_eq!(
            serde_json::from_value::<Uuid>(serde_json::to_value(&uuid).expect("serialize"))
                .expect("deserialize"),
            uuid
        );
    }

    #[test]
    fn ranges_validate_and_negotiate_highest_common_version() {
        let core = ProtoRange::new(ProtoVersion(2), ProtoVersion(5)).expect("core range");
        let client = ProtoRange::new(ProtoVersion(3), ProtoVersion(6)).expect("client range");
        assert_eq!(core.negotiate(client), Ok(ProtoVersion(5)));
        assert_eq!(client.negotiate(core), Ok(ProtoVersion(5)));
        let touching = ProtoRange::new(ProtoVersion(5), ProtoVersion(7)).expect("touching range");
        assert_eq!(core.negotiate(touching), Ok(ProtoVersion(5)));
        let disjoint = ProtoRange::new(ProtoVersion(6), ProtoVersion(7)).expect("disjoint range");
        assert_eq!(
            core.negotiate(disjoint),
            Err(NegotiateProtoError {
                supported: core,
                client: disjoint
            })
        );
        assert_eq!(
            disjoint.negotiate(core),
            Err(NegotiateProtoError {
                supported: disjoint,
                client: core
            })
        );
        assert!(ProtoRange::new(ProtoVersion(2), ProtoVersion(1)).is_err());
        assert!(serde_json::from_str::<ProtoRange>(r#"{"min":2,"max":1}"#).is_err());
        assert!(serde_json::from_str::<ProtoRange>(r#"{"min":1,"max":1,"future":true}"#).is_ok());
    }
}
