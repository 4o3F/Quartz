//! Typed UI failures and the shared error-screen presentation.

use crate::format::{Arg, Formatter, Msg};
use quartz_pipe::CallCoreError;
use quartz_protocol::{CoreError, CoreErrorCode, CoreVersion, ErrorKind, ProtoRange, cli};
use std::path::PathBuf;

/// Whether this window can resolve the failure through one of its actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Recoverable,
    Unrecoverable,
}

/// Every failure that the launch and connection windows can present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(test, derive(strum::VariantArray))]
pub enum FailureKind {
    CoreNotRunning,
    PipeBusy,
    ConnectTimeout,
    ConnectIo,
    CoreStartFailed,
    NewerDatabase,
    MigrationFailed,
    OtherSession,
    VersionMismatch,
    IdentityRejected,
    HandshakeRejected,
    InvalidResponse,
    InterfaceUnavailable,
}

/// An action with a fixed operation; both retry variants have the same localized label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureAction {
    StartCore,
    RetryConnection,
    RetryStartCore,
    CopyDetails,
    Exit,
}
impl FailureAction {
    pub(crate) fn label(self, formatter: Formatter) -> String {
        formatter.text(match self {
            Self::StartCore => Msg::LinkStartCore,
            Self::RetryConnection | Self::RetryStartCore => Msg::FailureRetry,
            Self::CopyDetails => Msg::FailureCopyDetails,
            Self::Exit => Msg::FailureExit,
        })
    }
    #[cfg(windows)]
    pub(crate) fn element_id(self) -> &'static str {
        match self {
            Self::StartCore => "start-core",
            Self::RetryConnection => "retry-connection",
            Self::RetryStartCore => "retry-start-core",
            Self::CopyDetails => "copy-failure-details",
            Self::Exit => "exit-failure",
        }
    }
}
impl FailureKind {
    /// Classifies every kind explicitly; a new kind requires a policy decision.
    pub const fn severity(self) -> Severity {
        match self {
            Self::CoreNotRunning
            | Self::PipeBusy
            | Self::ConnectTimeout
            | Self::ConnectIo
            | Self::CoreStartFailed => Severity::Recoverable,
            Self::NewerDatabase
            | Self::MigrationFailed
            | Self::OtherSession
            | Self::VersionMismatch
            | Self::IdentityRejected
            | Self::HandshakeRejected
            | Self::InvalidResponse
            | Self::InterfaceUnavailable => Severity::Unrecoverable,
        }
    }
    /// Operations available in this window, in display order (07 §2.5).
    pub const fn actions(self) -> &'static [FailureAction] {
        match self {
            Self::CoreNotRunning => &[FailureAction::StartCore, FailureAction::RetryConnection],
            Self::PipeBusy | Self::ConnectTimeout | Self::ConnectIo => {
                &[FailureAction::RetryConnection]
            }
            Self::CoreStartFailed => &[FailureAction::RetryStartCore, FailureAction::Exit],
            Self::NewerDatabase
            | Self::MigrationFailed
            | Self::OtherSession
            | Self::VersionMismatch
            | Self::IdentityRejected
            | Self::HandshakeRejected
            | Self::InvalidResponse
            | Self::InterfaceUnavailable => &[FailureAction::CopyDetails, FailureAction::Exit],
        }
    }
    /// Initial expander state from the approved error mockup (07 section 2.5).
    pub(crate) const fn details_expansion(self) -> DetailsExpansion {
        match self {
            Self::CoreStartFailed
            | Self::MigrationFailed
            | Self::NewerDatabase
            | Self::VersionMismatch => DetailsExpansion::Expanded,
            Self::CoreNotRunning
            | Self::PipeBusy
            | Self::ConnectTimeout
            | Self::ConnectIo
            | Self::OtherSession
            | Self::IdentityRejected
            | Self::HandshakeRejected
            | Self::InvalidResponse
            | Self::InterfaceUnavailable => DetailsExpansion::Collapsed,
        }
    }
    pub(crate) const fn copy_placement(self) -> CopyPlacement {
        match self.severity() {
            Severity::Recoverable => CopyPlacement::DetailsFooter,
            Severity::Unrecoverable => CopyPlacement::PrimaryAction,
        }
    }
    fn code(self) -> &'static str {
        match self {
            Self::CoreNotRunning => "core_not_running",
            Self::PipeBusy => "pipe_busy",
            Self::ConnectTimeout => "connect_timeout",
            Self::ConnectIo => "connect_io",
            Self::CoreStartFailed => "core_start_failed",
            Self::NewerDatabase => "newer_database",
            Self::MigrationFailed => "migration_failed",
            Self::OtherSession => "other_session",
            Self::VersionMismatch => "proto_mismatch",
            Self::IdentityRejected => "identity_rejected",
            Self::HandshakeRejected => "handshake_rejected",
            Self::InvalidResponse => "invalid_response",
            Self::InterfaceUnavailable => "interface_unavailable",
        }
    }
    fn messages(self) -> FailureMessages {
        match self {
            Self::CoreNotRunning => FailureMessages {
                title: Msg::LinkOffline,
                message: Msg::FailureOfflineMessage,
            },
            Self::PipeBusy => FailureMessages {
                title: Msg::LinkFailed,
                message: Msg::FailureBusyMessage,
            },
            Self::ConnectTimeout => FailureMessages {
                title: Msg::LinkFailed,
                message: Msg::FailureTimeoutMessage,
            },
            Self::ConnectIo => FailureMessages {
                title: Msg::LinkFailed,
                message: Msg::FailureIoMessage,
            },
            Self::CoreStartFailed => FailureMessages {
                title: Msg::CoreErrorStartFailedTitle,
                message: Msg::CoreErrorStartFailedMessage,
            },
            Self::NewerDatabase => FailureMessages {
                title: Msg::CoreErrorNewerDatabaseTitle,
                message: Msg::CoreErrorNewerDatabaseMessage,
            },
            Self::MigrationFailed => FailureMessages {
                title: Msg::CoreErrorMigrationFailedTitle,
                message: Msg::CoreErrorMigrationFailedMessage,
            },
            Self::OtherSession => FailureMessages {
                title: Msg::CoreErrorOtherSessionTitle,
                message: Msg::CoreErrorOtherSessionMessage,
            },
            Self::VersionMismatch => FailureMessages {
                title: Msg::LinkMismatch,
                message: Msg::FailureMismatchMessage,
            },
            Self::IdentityRejected => FailureMessages {
                title: Msg::FailureIdentityTitle,
                message: Msg::FailureIdentityMessage,
            },
            Self::HandshakeRejected => FailureMessages {
                title: Msg::LinkRejected,
                message: Msg::FailureRejectedMessage,
            },
            Self::InvalidResponse => FailureMessages {
                title: Msg::FailureInvalidTitle,
                message: Msg::FailureInvalidMessage,
            },
            Self::InterfaceUnavailable => FailureMessages {
                title: Msg::FailureInterfaceTitle,
                message: Msg::FailureInterfaceMessage,
            },
        }
    }
}
struct FailureMessages {
    title: Msg,
    message: Msg,
}

/// One typed failure shared by the startup and main windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    kind: FailureKind,
    backups: Vec<PathBuf>,
    protocol: Option<ProtocolDetails>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProtocolDetails {
    core_version: CoreVersion,
    supported: ProtoRange,
}
impl Failure {
    /// Creates a failure without additional protocol or backup details.
    pub fn new(kind: FailureKind) -> Self {
        Self {
            kind,
            backups: Vec::new(),
            protocol: None,
        }
    }
    pub fn kind(&self) -> FailureKind {
        self.kind
    }
    pub fn severity(&self) -> Severity {
        self.kind.severity()
    }
    pub fn actions(&self) -> &'static [FailureAction] {
        self.kind.actions()
    }
    pub fn from_startup(error: CoreError) -> Self {
        let kind = match error.code {
            CoreErrorCode::CoreStartFailed => FailureKind::CoreStartFailed,
            CoreErrorCode::NewerDatabase => FailureKind::NewerDatabase,
            CoreErrorCode::MigrationFailed => FailureKind::MigrationFailed,
            CoreErrorCode::OtherSession => FailureKind::OtherSession,
        };
        Self {
            kind,
            backups: error.backup_paths,
            protocol: None,
        }
    }
    /// Classifies typed connection causes without inspecting error text.
    pub fn from_connection(error: &CallCoreError) -> Self {
        let kind = match error {
            #[cfg(windows)]
            CallCoreError::Open { source } => match source {
                quartz_pipe::OpenCorePipeError::CoreNotRunning { .. } => {
                    FailureKind::CoreNotRunning
                }
                quartz_pipe::OpenCorePipeError::Busy { .. } => FailureKind::PipeBusy,
                quartz_pipe::OpenCorePipeError::Connect { .. } => FailureKind::ConnectIo,
                quartz_pipe::OpenCorePipeError::ServerIdentity { .. } => {
                    FailureKind::IdentityRejected
                }
            },
            #[cfg(windows)]
            CallCoreError::Wait { source } => match source {
                quartz_pipe::WaitCorePipeError::Deadline { .. } => FailureKind::ConnectTimeout,
                quartz_pipe::WaitCorePipeError::Windows { .. } => FailureKind::ConnectIo,
                quartz_pipe::WaitCorePipeError::Worker { .. }
                | quartz_pipe::WaitCorePipeError::UnexpectedStatus { .. }
                | quartz_pipe::WaitCorePipeError::Cancelled => FailureKind::InterfaceUnavailable,
            },
            CallCoreError::ResponseTimeout { .. } | CallCoreError::WriteTimeout { .. } => {
                FailureKind::ConnectTimeout
            }
            CallCoreError::Io { .. } => FailureKind::ConnectIo,
            CallCoreError::Protocol {
                kind:
                    ErrorKind::ProtoMismatch {
                        core_version,
                        supported,
                    },
            } => {
                return Self {
                    kind: FailureKind::VersionMismatch,
                    backups: Vec::new(),
                    protocol: Some(ProtocolDetails {
                        core_version: core_version.clone(),
                        supported: *supported,
                    }),
                };
            }
            CallCoreError::Protocol { .. } | CallCoreError::ConnectionProtocol { .. } => {
                FailureKind::HandshakeRejected
            }
            CallCoreError::HandshakeVersion {
                received,
                core_version,
            } => {
                return Self {
                    kind: FailureKind::VersionMismatch,
                    backups: Vec::new(),
                    protocol: Some(ProtocolDetails {
                        core_version: core_version.clone(),
                        supported: ProtoRange::new(*received, *received)
                            .expect("single protocol version is ordered"),
                    }),
                };
            }
            CallCoreError::Decode { .. }
            | CallCoreError::DecodeBody { .. }
            | CallCoreError::Frame { .. }
            | CallCoreError::UnexpectedEnvelope
            | CallCoreError::ResponseIdMismatch { .. }
            | CallCoreError::UnsolicitedResponse { .. }
            | CallCoreError::DiscardConnection => FailureKind::InvalidResponse,
            CallCoreError::Runtime { .. }
            | CallCoreError::Encode { .. }
            | CallCoreError::EncodeBody { .. }
            | CallCoreError::EventOverflow
            | CallCoreError::RequestIdExhausted => FailureKind::InterfaceUnavailable,
        };
        Self::new(kind)
    }
    #[cfg(windows)]
    pub(crate) fn from_request(error: &crate::link::RequestCoreError) -> Self {
        match error {
            crate::link::RequestCoreError::Closed => Self::new(FailureKind::InterfaceUnavailable),
            crate::link::RequestCoreError::Disconnected => Self::new(FailureKind::CoreNotRunning),
            crate::link::RequestCoreError::Timeout => Self::new(FailureKind::ConnectTimeout),
            crate::link::RequestCoreError::Busy => Self::new(FailureKind::PipeBusy),
            crate::link::RequestCoreError::Call { source } => Self::from_connection(source),
        }
    }
    #[cfg(windows)]
    pub(crate) fn from_start(error: &crate::link::StartCoreError) -> Self {
        match error {
            crate::link::StartCoreError::Closed => Self::new(FailureKind::InterfaceUnavailable),
            crate::link::StartCoreError::Launch { .. } => Self::new(FailureKind::CoreStartFailed),
            crate::link::StartCoreError::Connect { source } => Self::from_connection(source),
        }
    }
}

/// Whether details are initially visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DetailsExpansion {
    Collapsed,
    Expanded,
}
impl DetailsExpansion {
    #[cfg(windows)]
    pub(crate) fn toggle(&mut self) {
        *self = match self {
            Self::Collapsed => Self::Expanded,
            Self::Expanded => Self::Collapsed,
        };
    }
}
/// Copy details appears once, either in the action bar or the footer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyPlacement {
    PrimaryAction,
    DetailsFooter,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DetailLine {
    Value { label: String, value: String },
    Command(String),
    Text(String),
}
/// Consecutive values share a column; commands and text occupy their own rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DetailGroup {
    Values(Vec<DetailValue>),
    Command(String),
    Text(String),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DetailValue {
    pub(crate) label: String,
    pub(crate) value: String,
}
impl DetailLine {
    pub(crate) fn grouped(lines: Vec<Self>) -> Vec<DetailGroup> {
        let mut groups = Vec::new();
        for line in lines {
            match line {
                Self::Value { label, value } => {
                    let value = DetailValue { label, value };
                    match groups.last_mut() {
                        Some(DetailGroup::Values(values)) => values.push(value),
                        _ => groups.push(DetailGroup::Values(vec![value])),
                    }
                }
                Self::Command(text) => groups.push(DetailGroup::Command(text)),
                Self::Text(text) => groups.push(DetailGroup::Text(text)),
            }
        }
        groups
    }

    fn text(&self) -> String {
        match self {
            Self::Value { label, value } => format!("{label}: {value}"),
            Self::Command(text) | Self::Text(text) => text.clone(),
        }
    }
}
#[derive(Debug)]
pub(crate) struct FailurePresentation {
    pub(crate) title: String,
    pub(crate) message: String,
    pub(crate) details: Vec<DetailLine>,
    pub(crate) actions: &'static [FailureAction],
}
impl FailurePresentation {
    pub(crate) fn new(failure: &Failure, formatter: Formatter) -> Self {
        let messages = failure.kind.messages();
        let mut details = vec![DetailLine::Value {
            label: formatter.text(Msg::FailureCode),
            value: failure.kind.code().into(),
        }];
        if let Some(protocol) = &failure.protocol {
            details.push(DetailLine::Value {
                label: formatter.text(Msg::FailureCoreVersion),
                value: protocol.core_version.0.clone(),
            });
            details.push(DetailLine::Value {
                label: formatter.text(Msg::FailureProtocol),
                value: format!(
                    "{}–{}",
                    protocol.supported.min().0,
                    protocol.supported.max().0
                ),
            });
        }
        if failure.kind == FailureKind::VersionMismatch {
            details.push(DetailLine::Value {
                label: formatter.text(Msg::FailureUiVersion),
                value: env!("CARGO_PKG_VERSION").into(),
            });
        }
        for path in &failure.backups {
            details.push(DetailLine::Value {
                label: formatter.text(Msg::CoreErrorBackups),
                value: path.display().to_string(),
            });
            if path
                .as_os_str()
                .to_str()
                .is_none_or(|text| text.contains(['"', '\r', '\n']))
            {
                details.push(DetailLine::Text(formatter.text_with(
                    Msg::CoreErrorRestoreManually,
                    &[Arg::Command.with(&format!("{} {}", cli::PROGRAM, cli::RESTORE_BACKUP))],
                )));
            } else {
                details.push(DetailLine::Command(format!(
                    "{} {} {} \"{}\"",
                    cli::PROGRAM,
                    cli::RESTORE_BACKUP,
                    cli::CONFIRM,
                    path.display()
                )));
            }
        }
        if !failure.backups.is_empty() {
            details.push(DetailLine::Text(
                formatter.text(Msg::CoreErrorRestoreWarning),
            ));
        }
        Self {
            title: formatter.text(messages.title),
            message: formatter.text(messages.message),
            details,
            actions: failure.actions(),
        }
    }
    /// Diagnostic content has no access to foreground window titles.
    pub(crate) fn copy_text(&self) -> String {
        std::iter::once(self.message.clone())
            .chain(self.details.iter().map(DetailLine::text))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests;
