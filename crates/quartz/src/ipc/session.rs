//! Pure connection state and request decisions, independent of named pipes.

use super::{
    ProcessId,
    registry::{ConnectionPeer, UiPeer},
};

use quartz_protocol::{
    CoreStatusRequest, CoreStatusResponse, Envelope, ErrorKind, Hello, HelloResponse, HelloRole,
    Op, PROTOCOL_VERSION, ProtoRange, ProtocolError, RequestId, Response, ResponseOutcome, Role,
    Subscribe, UiFirstFrameRequest, UiHiddenRequest, UiOpenRequest, UiState,
};

/// Roles permitted by the authenticated executable path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AllowedRoles {
    Quartz,
    Host,
}
impl AllowedRoles {
    pub(crate) fn permits(self, role: Role) -> bool {
        matches!((self, role), (Self::Quartz, Role::Ui | Role::Cli))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UiCommand {
    Open,
    Hidden {
        connection: super::registry::ConnectionId,
    },
    FirstFrame {
        connection: super::registry::ConnectionId,
    },
    Registered {
        connection: super::registry::ConnectionId,
        pid: ProcessId,
        state: quartz_protocol::UiState,
        registered_at: std::time::Instant,
    },
    Unregistered {
        connection: super::registry::ConnectionId,
    },
    SettingsChanged(quartz_protocol::EffectiveUiSettings),
}
#[derive(Debug, Clone, Copy)]
enum ReadyRole {
    Ui { state: UiState },
    Cli,
}
impl ReadyRole {
    fn role(self) -> Role {
        match self {
            Self::Ui { .. } => Role::Ui,
            Self::Cli => Role::Cli,
        }
    }
    fn peer(self, pid: ProcessId) -> ConnectionPeer {
        match self {
            Self::Ui { state } => ConnectionPeer::Ui(UiPeer { pid, state }),
            Self::Cli => ConnectionPeer::Cli,
        }
    }
}
impl From<HelloRole> for ReadyRole {
    fn from(role: HelloRole) -> Self {
        match role {
            HelloRole::Ui { ui } => Self::Ui { state: ui.state },
            HelloRole::Cli => Self::Cli,
        }
    }
}
#[derive(Debug)]
enum State {
    HelloPending,
    Ready(ReadyRole),
    Closing,
}
#[derive(Debug)]
pub(crate) struct ConnectionSession {
    state: State,
    allowed: AllowedRoles,
}
#[derive(Debug)]
pub(crate) enum Action {
    Reply(Response),
    Hidden {
        id: RequestId,
    },
    FirstFrame {
        id: RequestId,
    },
    Timed {
        id: RequestId,
        command: super::TimedCommand,
    },
    Ui {
        id: RequestId,
        command: UiCommand,
    },
    Subscribe {
        id: RequestId,
        subscription: Subscribe,
    },
    Unsubscribe {
        id: RequestId,
        subscription: quartz_protocol::Unsubscribe,
    },
    Write {
        id: RequestId,
        request: crate::persistence::dispatch::WriteRequest,
    },
    Rules {
        id: RequestId,
        request: crate::rules::read::ReadRequest,
    },
    Preview {
        id: RequestId,
        request: crate::query::preview::PreviewRequest,
    },
    Query {
        id: RequestId,
        request: crate::query::QueryRequest,
    },
}
impl Action {
    fn from_query(
        id: RequestId,
        query: Result<crate::query::QueryRequest, serde_json::Error>,
    ) -> Self {
        match query {
            Ok(request) => Self::Query { id, request },
            Err(_) => Self::error(Some(id), ErrorKind::BadRequest),
        }
    }

    fn error(id: Option<RequestId>, kind: ErrorKind) -> Self {
        Self::Reply(Self::error_response(id, ProtocolError::new(kind)))
    }
    pub(super) fn error_response(id: Option<RequestId>, error: ProtocolError) -> Response {
        Response {
            id,
            outcome: ResponseOutcome::Err { error },
        }
    }
    fn success(id: RequestId, body: impl serde::Serialize) -> Self {
        Self::Reply(Self::success_response(id, body))
    }
    pub(super) fn success_response(id: RequestId, body: impl serde::Serialize) -> Response {
        match serde_json::to_value(body) {
            Ok(body) => Response {
                id: Some(id),
                outcome: ResponseOutcome::Ok { body },
            },
            Err(error) => {
                tracing::error!(%error, "could not encode IPC response body");
                Self::error_response(Some(id), ProtocolError::new(ErrorKind::Internal))
            }
        }
    }
}
impl ConnectionSession {
    pub(crate) fn new(allowed: AllowedRoles) -> Self {
        Self {
            state: State::HelloPending,
            allowed,
        }
    }
    pub(crate) fn hello_pending(&self) -> bool {
        matches!(self.state, State::HelloPending)
    }
    pub(crate) fn peer(&self, pid: ProcessId) -> Option<ConnectionPeer> {
        match self.state {
            State::Ready(role) => Some(role.peer(pid)),
            State::HelloPending | State::Closing => None,
        }
    }
    pub(crate) fn role(&self) -> Option<Role> {
        match self.state {
            State::Ready(role) => Some(role.role()),
            State::HelloPending | State::Closing => None,
        }
    }
    pub(crate) fn closing(&self) -> bool {
        matches!(self.state, State::Closing)
    }
    pub(crate) fn fail(&mut self, kind: ErrorKind) -> Action {
        self.state = State::Closing;
        Action::error(None, kind)
    }
    pub(crate) fn receive(&mut self, envelope: Envelope, status: &CoreStatusResponse) -> Action {
        let Envelope::Request(request) = envelope else {
            return self.fail(ErrorKind::BadRequest);
        };
        let id = request.id;
        match self.state {
            State::HelloPending => {
                self.state = State::Closing;
                if request.op != Op::Hello {
                    return Action::error(Some(id), ErrorKind::HandshakeRequired);
                }
                let Ok(hello) = serde_json::from_value::<Hello>(request.body) else {
                    return Action::error(Some(id), ErrorKind::BadRequest);
                };
                let supported = ProtoRange::new(PROTOCOL_VERSION, PROTOCOL_VERSION)
                    .expect("single protocol version is valid");
                let proto = match supported.negotiate(hello.proto) {
                    Ok(proto) => proto,
                    Err(error) => {
                        return Action::error(
                            Some(id),
                            ErrorKind::ProtoMismatch {
                                supported: error.supported,
                                core_version: status.core_version.clone(),
                            },
                        );
                    }
                };
                let role = hello.role.role();
                if !self.allowed.permits(role) {
                    return Action::error(Some(id), ErrorKind::Forbidden);
                }
                self.state = State::Ready(hello.role.into());
                Action::success(
                    id,
                    HelloResponse {
                        proto,
                        instance_id: status.instance_id.clone(),
                        core_version: status.core_version.clone(),
                    },
                )
            }
            State::Ready(role) => {
                let role = role.role();
                if request.op == Op::Unknown {
                    return Action::error(Some(id), ErrorKind::UnknownOp);
                }
                if role.authorize(request.op).is_err() {
                    return Action::error(Some(id), ErrorKind::Forbidden);
                }
                match request.op {
                    Op::TrackingPause
                    | Op::TrackingResume
                    | Op::TrackingKeepAlive
                    | Op::PrivacyModeSet => {
                        match super::TimedCommand::decode(request.op, request.body) {
                            Ok(command) => Action::Timed { id, command },
                            Err(error) => Action::Reply(Action::error_response(Some(id), error)),
                        }
                    }
                    Op::CoreStatus => {
                        if serde_json::from_value::<CoreStatusRequest>(request.body).is_err() {
                            return Action::error(Some(id), ErrorKind::BadRequest);
                        }
                        Action::success(id, status)
                    }
                    Op::UiHidden => {
                        if serde_json::from_value::<UiHiddenRequest>(request.body).is_err() {
                            return Action::error(Some(id), ErrorKind::BadRequest);
                        }
                        Action::Hidden { id }
                    }
                    Op::UiOpen | Op::UiFirstFrame => {
                        let command = if request.op == Op::UiOpen {
                            if serde_json::from_value::<UiOpenRequest>(request.body).is_err() {
                                return Action::error(Some(id), ErrorKind::BadRequest);
                            }
                            UiCommand::Open
                        } else {
                            if serde_json::from_value::<UiFirstFrameRequest>(request.body).is_err()
                            {
                                return Action::error(Some(id), ErrorKind::BadRequest);
                            }
                            return Action::FirstFrame { id };
                        };
                        Action::Ui { id, command }
                    }
                    Op::Subscribe => {
                        let Ok(subscription) = serde_json::from_value::<Subscribe>(request.body)
                        else {
                            return Action::error(Some(id), ErrorKind::BadRequest);
                        };
                        Action::Subscribe { id, subscription }
                    }
                    Op::Unsubscribe => {
                        let Ok(subscription) =
                            serde_json::from_value::<quartz_protocol::Unsubscribe>(request.body)
                        else {
                            return Action::error(Some(id), ErrorKind::BadRequest);
                        };
                        Action::Unsubscribe { id, subscription }
                    }
                    Op::RuleSetsTree | Op::RuleSetsGet | Op::RulesTextExport => {
                        match crate::rules::read::ReadRequest::decode(request.op, request.body) {
                            Ok(request) => Action::Rules { id, request },
                            Err(_) => Action::error(Some(id), ErrorKind::BadRequest),
                        }
                    }
                    Op::RulesPreview => {
                        match crate::query::preview::PreviewRequest::decode(request.body) {
                            Ok(request) => Action::Preview { id, request },
                            Err(error) => Action::Reply(Action::error_response(Some(id), error)),
                        }
                    }
                    Op::QueryFacts
                    | Op::QueryDay
                    | Op::QueryTimeline
                    | Op::QueryAnnotations
                    | Op::QueryPendingAway
                    | Op::QuerySegmentDetail
                    | Op::QueryUnclassified
                    | Op::QueryRange
                    | Op::QueryEntityDetail => Action::from_query(
                        id,
                        crate::query::QueryRequest::decode(request.op, request.body),
                    ),
                    Op::RulesTextValidate
                    | Op::RulesTextImport
                    | Op::RuleSetsEdit
                    | Op::RuleSetsSetEnabled
                    | Op::RuleSetsDelete
                    | Op::SettingsGet
                    | Op::SettingsSet
                    | Op::AnnotationsCreate
                    | Op::AnnotationsUpdate
                    | Op::AnnotationsDelete
                    | Op::CategoriesList
                    | Op::CategoriesCreate
                    | Op::CategoriesUpdate
                    | Op::CategoriesArchive
                    | Op::CategoriesReorder
                    | Op::DataInfo
                    | Op::DataBackup
                    | Op::DataCompact
                    | Op::DataDeleteRange => {
                        match crate::persistence::dispatch::WriteRequest::decode(
                            request.op,
                            request.body,
                        ) {
                            Ok(request) => Action::Write { id, request },
                            Err(_) => Action::error(Some(id), ErrorKind::BadRequest),
                        }
                    }
                    Op::Hello => Action::error(Some(id), ErrorKind::BadRequest),
                    Op::Unknown => Action::error(Some(id), ErrorKind::UnknownOp),
                }
            }
            State::Closing => Action::error(Some(id), ErrorKind::BadRequest),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quartz_protocol::Topic;
    impl Action {
        fn reply(self) -> Response {
            let Self::Reply(response) = self else {
                panic!("expected reply action")
            };
            response
        }
    }
    fn request(id: u64, op: Op, body: serde_json::Value) -> Envelope {
        Envelope::Request(quartz_protocol::Request {
            id: RequestId(id),
            op,
            body,
        })
    }
    fn ready(role: ReadyRole) -> ConnectionSession {
        ConnectionSession {
            state: State::Ready(role),
            allowed: AllowedRoles::Quartz,
        }
    }
    #[test]
    fn role_authorization_and_dispatch_routing() {
        let status = crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId("roles".into()));
        let status = status.subscribe();
        let snapshot = &status.borrow().status;
        let ui_only = [
            (
                Op::RuleSetsEdit,
                serde_json::json!({"key":"user.a","base_version":0,"text":""}),
            ),
            (
                Op::RuleSetsSetEnabled,
                serde_json::json!({"key":"user.a","base_version":1,"enabled":false}),
            ),
            (
                Op::RuleSetsDelete,
                serde_json::json!({"key":"user.a","base_version":1}),
            ),
            (Op::CategoriesList, serde_json::json!({})),
            (
                Op::CategoriesArchive,
                serde_json::json!({"uuid":"00000000-0000-4000-8000-000000000123","archived":true}),
            ),
            (Op::AnnotationsCreate, serde_json::json!({"entities":[]})),
            (Op::AnnotationsUpdate, serde_json::json!({"entities":[]})),
            (Op::AnnotationsDelete, serde_json::json!({"uuids":[]})),
            (Op::TrackingPause, serde_json::json!({"until_utc_ms":100})),
            (Op::TrackingResume, serde_json::json!({})),
            (
                Op::TrackingKeepAlive,
                serde_json::json!({"until_utc_ms":100}),
            ),
            (Op::PrivacyModeSet, serde_json::json!({"enabled":false})),
            (
                Op::RulesPreview,
                serde_json::json!({"mutation":{"op":"rule_sets.edit","body":{"key":"user.a","base_version":0,"text":""}},"start_date":"2025-01-02","end_date":"2025-01-03"}),
            ),
            (Op::DataInfo, serde_json::json!({})),
            (Op::DataBackup, serde_json::json!({})),
            (Op::DataCompact, serde_json::json!({})),
            (
                Op::DataDeleteRange,
                serde_json::json!({"start_date":"2025-01-02","end_date":"2025-01-03"}),
            ),
        ];
        for (op, body) in ui_only {
            assert_eq!(
                ready(ReadyRole::Cli)
                    .receive(request(1, op, body.clone()), snapshot)
                    .reply()
                    .outcome,
                ResponseOutcome::Err {
                    error: ProtocolError::new(ErrorKind::Forbidden)
                },
                "{op:?} forbidden for CLI"
            );
            assert!(
                !matches!(
                    ready(ReadyRole::Ui {
                        state: UiState::Visible
                    })
                    .receive(request(1, op, body), snapshot),
                    Action::Reply(_)
                ),
                "{op:?} dispatches for UI"
            );
        }
        let shared = [
            (Op::RuleSetsTree, serde_json::json!({"kind":"privacy"})),
            (
                Op::RuleSetsGet,
                serde_json::json!({"key":"builtin.privacy","view":"effective"}),
            ),
            (
                Op::RulesTextExport,
                serde_json::json!({"key":"builtin.privacy"}),
            ),
            (
                Op::RulesTextValidate,
                serde_json::json!({"key":"user.a","base_version":0,"text":""}),
            ),
            (Op::SettingsGet, serde_json::json!({})),
            (
                Op::QueryAnnotations,
                serde_json::json!({"start_utc_ms":0,"end_utc_ms":1000}),
            ),
            (
                Op::QueryRange,
                serde_json::json!({"start_date":"2025-01-02","end_date":"2025-01-09","bucket":"week","dimensions":["category"]}),
            ),
            (
                Op::QueryUnclassified,
                serde_json::json!({"start_date":"2025-01-02","end_date":"2025-01-03","group_by":"app"}),
            ),
            (
                Op::QuerySegmentDetail,
                serde_json::json!({"start_utc_ms":0,"end_utc_ms":1000}),
            ),
            (
                Op::QueryEntityDetail,
                serde_json::json!({"entity":{"kind":"app","id":1},"local_date":"2026-11-21"}),
            ),
        ];
        for role in [
            ReadyRole::Ui {
                state: UiState::Visible,
            },
            ReadyRole::Cli,
        ] {
            for (op, body) in &shared {
                assert!(
                    !matches!(
                        ready(role).receive(request(2, *op, body.clone()), snapshot),
                        Action::Reply(_)
                    ),
                    "{op:?} dispatches for {role:?}"
                );
            }
        }
        assert_eq!(
            ready(ReadyRole::Ui {
                state: UiState::Visible
            })
            .receive(
                request(
                    3,
                    Op::RuleSetsTree,
                    serde_json::json!({"kind":"privacy","extra":true})
                ),
                snapshot
            )
            .reply()
            .outcome,
            ResponseOutcome::Err {
                error: ProtocolError::new(ErrorKind::BadRequest)
            }
        );
    }
    #[test]
    fn duplicate_hello_rejects_request_and_preserves_negotiated_role() {
        let publisher =
            crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId("duplicate-hello".into()));
        let receiver = publisher.subscribe();
        let mut session = ConnectionSession {
            state: State::Ready(ReadyRole::Cli),
            allowed: AllowedRoles::Quartz,
        };
        let action = session.receive(request(2, Op::Hello, serde_json::json!({"proto":{"min":1,"max":1},"role":"ui","ui":{"state":"visible"},"client_version":"test"})), &receiver.borrow().status);
        assert_eq!(
            action.reply().outcome,
            ResponseOutcome::Err {
                error: ProtocolError::new(ErrorKind::BadRequest)
            }
        );
        assert!(matches!(
            session.peer(ProcessId::new(1)),
            Some(ConnectionPeer::Cli)
        ));
        assert!(!session.closing());
    }
    #[test]
    fn pure_session_requires_hello_and_authorizes_without_io() {
        let publisher =
            crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId("session".into()));
        let receiver = publisher.subscribe();
        let status = receiver.borrow();
        let mut pending = ConnectionSession::new(AllowedRoles::Quartz);
        assert_eq!(
            pending
                .receive(
                    request(7, Op::CoreStatus, serde_json::json!({})),
                    &status.status
                )
                .reply()
                .outcome,
            ResponseOutcome::Err {
                error: ProtocolError::new(ErrorKind::HandshakeRequired)
            }
        );
        assert!(pending.closing());
        let mut session = ConnectionSession::new(AllowedRoles::Quartz);
        let hello =
            serde_json::json!({"proto":{"min":1,"max":1},"role":"cli","client_version":"test"});
        let response = session.receive(request(1, Op::Hello, hello), &status.status);
        assert!(!session.closing());
        let ResponseOutcome::Ok { body } = response.reply().outcome else {
            panic!("hello response")
        };
        let hello: HelloResponse = serde_json::from_value(body).expect("hello body");
        assert_eq!(hello.proto, PROTOCOL_VERSION);
        assert_eq!(hello.instance_id, status.status.instance_id);
        assert_eq!(hello.core_version, status.status.core_version);
        assert!(matches!(
            session.receive(
                request(2, Op::UiOpen, serde_json::json!({})),
                &status.status
            ),
            Action::Ui {
                id: RequestId(2),
                command: UiCommand::Open
            }
        ));
        assert_eq!(
            session
                .receive(
                    request(3, Op::UiFirstFrame, serde_json::json!({})),
                    &status.status
                )
                .reply()
                .outcome,
            ResponseOutcome::Err {
                error: ProtocolError::new(ErrorKind::Forbidden)
            }
        );
        assert_eq!(
            session
                .receive(
                    request(4, Op::CoreStatus, serde_json::json!(null)),
                    &status.status
                )
                .reply()
                .outcome,
            ResponseOutcome::Err {
                error: ProtocolError::new(ErrorKind::BadRequest)
            }
        );
        assert!(!session.closing());
    }
    #[test]
    fn host_executable_cannot_authenticate_any_role() {
        let publisher =
            crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId("host-test".into()));
        let receiver = publisher.subscribe();
        let mut session = ConnectionSession::new(AllowedRoles::Host);
        let hello =
            serde_json::json!({"proto":{"min":1,"max":1},"client_version":"test","role":"cli"});
        let decision = session.receive(request(1, Op::Hello, hello), &receiver.borrow().status);
        assert_eq!(
            decision.reply().outcome,
            ResponseOutcome::Err {
                error: ProtocolError::new(ErrorKind::Forbidden)
            }
        );
        assert!(session.closing());
    }

    #[test]
    fn executable_roles_cannot_be_promoted() {
        for role in [Role::Ui, Role::Cli] {
            assert!(AllowedRoles::Quartz.permits(role));
            assert!(!AllowedRoles::Host.permits(role));
        }
    }
    #[test]
    fn subscribe_validation_is_atomic_and_role_checked_without_io() {
        let publisher =
            crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId("subscribe".into()));
        let receiver = publisher.subscribe();
        let status = &receiver.borrow().status;
        let mut session = ConnectionSession::new(AllowedRoles::Quartz);
        session.receive(request(1, Op::Hello, serde_json::json!({"proto":{"min":1,"max":1},"role":"ui","client_version":"test","ui":{"state":"starting"}})), status);
        for body in [
            serde_json::json!({"topics":["weather"]}),
            serde_json::json!({"topics":["day:2026-02-30"]}),
            serde_json::json!({"topics":null}),
        ] {
            let decision = session.receive(request(2, Op::Subscribe, body), status);
            assert!(matches!(
                decision.reply().outcome,
                ResponseOutcome::Err {
                    error: ProtocolError {
                        kind: ErrorKind::BadRequest,
                        ..
                    }
                }
            ));
        }
        let rejected = session.receive(
            request(
                3,
                Op::Subscribe,
                serde_json::json!({"topics":["state","day:2026-11-20"]}),
            ),
            status,
        );
        assert!(matches!(
            rejected,
            Action::Subscribe {
                id: RequestId(3),
                ..
            }
        ));
        let accepted = session.receive(
            request(
                4,
                Op::Subscribe,
                serde_json::json!({"topics":["state","control"]}),
            ),
            status,
        );
        let Action::Subscribe { id, subscription } = accepted else {
            panic!("expected subscription action")
        };
        assert_eq!(id, RequestId(4));
        assert_eq!(subscription.topics, vec![Topic::State, Topic::Control]);
    }
}
