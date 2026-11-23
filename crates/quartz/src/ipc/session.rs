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
        matches!(
            (self, role),
            (Self::Quartz, Role::Ui | Role::Cli) | (Self::Host, Role::Host)
        )
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
    Host,
}
impl ReadyRole {
    fn role(self) -> Role {
        match self {
            Self::Ui { .. } => Role::Ui,
            Self::Cli => Role::Cli,
            Self::Host => Role::Host,
        }
    }
    fn peer(self, pid: ProcessId) -> ConnectionPeer {
        match self {
            Self::Ui { state } => ConnectionPeer::Ui(UiPeer { pid, state }),
            Self::Cli => ConnectionPeer::Cli,
            Self::Host => ConnectionPeer::Host,
        }
    }
}
impl From<HelloRole> for ReadyRole {
    fn from(role: HelloRole) -> Self {
        match role {
            HelloRole::Ui { ui } => Self::Ui { state: ui.state },
            HelloRole::Cli => Self::Cli,
            HelloRole::Host { .. } => Self::Host,
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
                // TODO(M5): Authenticate registered host browser streams and enable host runtime.
                if !self.allowed.permits(role) || role == Role::Host {
                    return Action::error(Some(id), ErrorKind::Forbidden);
                }
                self.state = State::Ready(hello.role.into());
                Action::success(
                    id,
                    HelloResponse {
                        proto,
                        instance_id: status.instance_id.clone(),
                        core_version: status.core_version.clone(),
                        capabilities: vec![
                            quartz_protocol::Capability::CategoriesV1,
                            quartz_protocol::Capability::QueryDocumentsV1,
                            quartz_protocol::Capability::QueryUnclassifiedV1,
                            quartz_protocol::Capability::QueryRangeV1,
                            quartz_protocol::Capability::QueryAnnotationsV1,
                        ],
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
                    Op::SettingsGet
                    | Op::SettingsSet
                    | Op::AnnotationsCreate
                    | Op::AnnotationsUpdate
                    | Op::AnnotationsDelete
                    | Op::CategoriesList
                    | Op::CategoriesCreate
                    | Op::CategoriesUpdate
                    | Op::CategoriesArchive
                    | Op::CategoriesReorder => {
                        match crate::persistence::dispatch::WriteRequest::decode(
                            request.op,
                            request.body,
                        ) {
                            Ok(request) => Action::Write { id, request },
                            Err(_) => Action::error(Some(id), ErrorKind::BadRequest),
                        }
                    }
                    Op::Hello
                    | Op::RuleSetsTree
                    | Op::RuleSetsGet
                    | Op::RuleSetsVersions
                    | Op::RuleSetsSetEnabled
                    | Op::RuleSetsReorder
                    | Op::RuleSetsDelete
                    | Op::RuleSetsRevert
                    | Op::RuleSetsEdit
                    | Op::RulesPreview
                    | Op::RulesExplain
                    | Op::RulesTextValidate
                    | Op::RulesTextPreview
                    | Op::RulesTextDiff
                    | Op::RulesTextImport
                    | Op::RulesTextExport
                    | Op::DataDelete
                    | Op::BrowserHello
                    | Op::BrowserState
                    | Op::BrowserGap
                    | Op::BrowserPurgeAck
                    | Op::BrowserQuerySiteToday
                    | Op::AppsIcon
                    | Op::BrowserHostsStatus
                    | Op::BrowserHostsEnable
                    | Op::BrowserHostsDisable
                    | Op::MaintenanceBackup
                    | Op::MaintenanceCompact
                    | Op::Unknown => Action::error(Some(id), ErrorKind::NotImplemented),
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
    #[test]
    fn category_operations_are_ui_only_and_reject_unknown_fields() {
        let status =
            crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId("categories".into()));
        let status = status.subscribe();
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../quartz-protocol/tests/fixtures/categories.json"
        ))
        .expect("category fixture");
        struct Case {
            op: Op,
            body: serde_json::Value,
        }
        let cases = [
            Case {
                op: Op::CategoriesList,
                body: fixture["list"].clone(),
            },
            Case {
                op: Op::CategoriesCreate,
                body: fixture["create"].clone(),
            },
            Case {
                op: Op::CategoriesUpdate,
                body: fixture["update"].clone(),
            },
            Case {
                op: Op::CategoriesReorder,
                body: fixture["reorder"].clone(),
            },
            Case {
                op: Op::CategoriesArchive,
                body: fixture["archive"].clone(),
            },
        ];
        for case in cases {
            let mut ui = ConnectionSession {
                state: State::Ready(ReadyRole::Ui {
                    state: UiState::Visible,
                }),
                allowed: AllowedRoles::Quartz,
            };
            assert!(matches!(
                ui.receive(
                    request(2, case.op, case.body.clone()),
                    &status.borrow().status
                ),
                Action::Write { .. }
            ));
            for role in [ReadyRole::Cli, ReadyRole::Host] {
                let mut session = ConnectionSession {
                    state: State::Ready(role),
                    allowed: AllowedRoles::Quartz,
                };
                assert_eq!(
                    session
                        .receive(
                            request(2, case.op, case.body.clone()),
                            &status.borrow().status
                        )
                        .reply()
                        .outcome,
                    ResponseOutcome::Err {
                        error: ProtocolError::new(ErrorKind::Forbidden)
                    }
                );
            }
            let mut invalid = case.body;
            invalid["extra"] = serde_json::json!(true);
            assert_eq!(
                ui.receive(request(2, case.op, invalid), &status.borrow().status)
                    .reply()
                    .outcome,
                ResponseOutcome::Err {
                    error: ProtocolError::new(ErrorKind::BadRequest)
                }
            );
        }
    }
    #[test]
    fn detail_queries_reach_dispatch_for_ui_and_cli() {
        let publisher =
            crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId("details".into()));
        let receiver = publisher.subscribe();
        for role in [
            ReadyRole::Ui {
                state: UiState::Visible,
            },
            ReadyRole::Cli,
        ] {
            let mut session = ConnectionSession {
                state: State::Ready(role),
                allowed: AllowedRoles::Quartz,
            };
            assert!(matches!(
                session.receive(
                    request(
                        7,
                        Op::QueryAnnotations,
                        serde_json::json!({"start_utc_ms":0,"end_utc_ms":1000})
                    ),
                    &receiver.borrow().status
                ),
                Action::Query {
                    request: crate::query::QueryRequest::Annotations(_),
                    ..
                }
            ));
            for body in [
                serde_json::json!({"start_utc_ms":0,"end_utc_ms":1000,"label":"unexpected"}),
                serde_json::json!({"start_utc_ms":0}),
                serde_json::json!({"start_utc_ms":0,"end_utc_ms":1000,"cursor":"invalid"}),
            ] {
                assert_eq!(
                    session
                        .receive(
                            request(7, Op::QueryAnnotations, body),
                            &receiver.borrow().status
                        )
                        .reply()
                        .outcome,
                    ResponseOutcome::Err {
                        error: ProtocolError::new(ErrorKind::BadRequest)
                    }
                );
            }
            let range = session.receive(request(4, Op::QueryRange, serde_json::json!({"start_date":"2025-01-02","end_date":"2025-01-09","bucket":"week","dimensions":["category","app","document"]})), &receiver.borrow().status);
            assert!(matches!(
                range,
                Action::Query {
                    request: crate::query::QueryRequest::Range(_),
                    ..
                }
            ));
            for body in [
                serde_json::json!({"start_date":"invalid","end_date":"2025-01-09","bucket":"week","dimensions":["category"]}),
                serde_json::json!({"start_date":"2025-01-02","end_date":"2025-01-09","bucket":"month","dimensions":["category"]}),
                serde_json::json!({"start_date":"2025-01-02","end_date":"2025-01-09","bucket":"week","dimensions":["category"],"time_zone":"UTC"}),
            ] {
                assert_eq!(
                    session
                        .receive(request(4, Op::QueryRange, body), &receiver.borrow().status)
                        .reply()
                        .outcome,
                    ResponseOutcome::Err {
                        error: ProtocolError::new(ErrorKind::BadRequest)
                    }
                );
            }
            let unclassified = session.receive(request(5, Op::QueryUnclassified, serde_json::json!({"start_date":"2025-01-02","end_date":"2025-01-03","group_by":"app"})), &receiver.borrow().status);
            assert!(matches!(
                unclassified,
                Action::Query {
                    request: crate::query::QueryRequest::Unclassified(_),
                    ..
                }
            ));
            for body in [
                serde_json::json!({"start_date":"2025-01-02","end_date":"2025-01-03","group_by":"domain"}),
                serde_json::json!({"start_date":"bad","end_date":"2025-01-03","group_by":"app"}),
                serde_json::json!({"start_date":"2025-01-02","end_date":"2025-01-03","group_by":"app","time_zone":"UTC"}),
            ] {
                let response = session.receive(
                    request(6, Op::QueryUnclassified, body),
                    &receiver.borrow().status,
                );
                assert_eq!(
                    response.reply().outcome,
                    ResponseOutcome::Err {
                        error: ProtocolError::new(ErrorKind::BadRequest)
                    }
                );
            }
            let segment = session.receive(
                request(
                    2,
                    Op::QuerySegmentDetail,
                    serde_json::json!({"start_utc_ms":0,"end_utc_ms":1000}),
                ),
                &receiver.borrow().status,
            );
            assert!(matches!(
                segment,
                Action::Query {
                    request: crate::query::QueryRequest::SegmentDetail(_),
                    ..
                }
            ));
            let entity = session.receive(
                request(
                    3,
                    Op::QueryEntityDetail,
                    serde_json::json!({"entity":{"kind":"app","id":1},"local_date":"2026-11-21"}),
                ),
                &receiver.borrow().status,
            );
            assert!(matches!(
                entity,
                Action::Query {
                    request: crate::query::QueryRequest::EntityDetail(_),
                    ..
                }
            ));
            let document = session.receive(request(4, Op::QueryEntityDetail, serde_json::json!({"entity":{"kind":"document","document_ref":"doc:opaque"},"local_date":"2026-11-21","reference_range":{"start_utc_ms":0,"end_utc_ms":604800000}})), &receiver.borrow().status);
            assert!(matches!(
                document,
                Action::Query {
                    request: crate::query::QueryRequest::EntityDetail(_),
                    ..
                }
            ));
            for kind in ["document", "domain"] {
                let response = session.receive(request(4, Op::QueryEntityDetail, serde_json::json!({"entity":{"kind":kind,"id":1},"local_date":"2026-11-21"})), &receiver.borrow().status);
                assert_eq!(
                    response.reply().outcome,
                    ResponseOutcome::Err {
                        error: ProtocolError::new(ErrorKind::BadRequest)
                    }
                );
            }
        }
    }
    #[test]
    fn cli_cannot_dispatch_annotation_writes() {
        let publisher =
            crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId("annotations".into()));
        let receiver = publisher.subscribe();
        let mut session = ConnectionSession {
            state: State::Ready(ReadyRole::Cli),
            allowed: AllowedRoles::Quartz,
        };
        for op in [
            Op::AnnotationsCreate,
            Op::AnnotationsUpdate,
            Op::AnnotationsDelete,
        ] {
            assert_eq!(
                session
                    .receive(
                        request(1, op, serde_json::json!({})),
                        &receiver.borrow().status
                    )
                    .reply()
                    .outcome,
                ResponseOutcome::Err {
                    error: ProtocolError::new(ErrorKind::Forbidden)
                }
            );
        }
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
        assert_eq!(
            hello.capabilities,
            vec![
                quartz_protocol::Capability::CategoriesV1,
                quartz_protocol::Capability::QueryDocumentsV1,
                quartz_protocol::Capability::QueryUnclassifiedV1,
                quartz_protocol::Capability::QueryRangeV1,
                quartz_protocol::Capability::QueryAnnotationsV1,
            ]
        );
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
    fn host_runtime_is_disabled_even_for_the_host_executable() {
        let publisher =
            crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId("host-test".into()));
        let receiver = publisher.subscribe();
        let mut session = ConnectionSession::new(AllowedRoles::Host);
        let hello = serde_json::json!({"proto":{"min":1,"max":1},"client_version":"test","role":"host","browser":{
            "profile_instance_id":"12345678-1234-1234-1234-123456789abc","browser_session_id":"12345678-1234-1234-1234-123456789abc",
            "epoch":1,"proto":{"min":1,"max":1},"browser":"edge","extension_id":"test"}});
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
        for role in [Role::Ui, Role::Cli, Role::Host] {
            assert_eq!(AllowedRoles::Quartz.permits(role), role != Role::Host);
            assert_eq!(AllowedRoles::Host.permits(role), role == Role::Host);
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
    #[test]
    fn settings_permissions_and_invalid_bodies_are_decided_before_database_admission() {
        let publisher = crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId(
            "settings-session".into(),
        ));
        let receiver = publisher.subscribe();
        let status = receiver.borrow();
        for role in [ReadyRole::Host] {
            let mut session = ConnectionSession {
                state: State::Ready(role),
                allowed: AllowedRoles::Quartz,
            };
            for op in [Op::SettingsGet, Op::SettingsSet] {
                let response = session
                    .receive(request(1, op, serde_json::json!({})), &status.status)
                    .reply();
                assert_eq!(
                    response.outcome,
                    ResponseOutcome::Err {
                        error: ProtocolError::new(ErrorKind::Forbidden)
                    }
                );
            }
        }
        let mut ui = ConnectionSession {
            state: State::Ready(ReadyRole::Ui {
                state: UiState::Starting,
            }),
            allowed: AllowedRoles::Quartz,
        };
        for body in [
            serde_json::json!({"language":"unknown"}),
            serde_json::json!({"hour_cycle":"25"}),
            serde_json::json!({"first_day_of_week":null}),
        ] {
            assert_eq!(
                ui.receive(request(1, Op::SettingsSet, body), &status.status)
                    .reply()
                    .outcome,
                ResponseOutcome::Err {
                    error: ProtocolError::new(ErrorKind::BadRequest)
                }
            );
        }
        assert!(matches!(
            ui.receive(
                request(2, Op::SettingsGet, serde_json::json!({})),
                &status.status
            ),
            Action::Write {
                request: crate::persistence::dispatch::WriteRequest::SettingsGet,
                ..
            }
        ));
    }
    #[test]
    fn timed_controls_require_ui_and_decode_required_fields_before_admission() {
        let status =
            crate::ipc::StatusPublisher::new(quartz_protocol::InstanceId("timed-session".into()));
        let receiver = status.subscribe();
        let status = &receiver.borrow().status;
        let ops = [
            Op::TrackingPause,
            Op::TrackingResume,
            Op::TrackingKeepAlive,
            Op::PrivacyModeSet,
        ];
        for role in [ReadyRole::Cli, ReadyRole::Host] {
            let mut session = ConnectionSession {
                state: State::Ready(role),
                allowed: AllowedRoles::Quartz,
            };
            for op in ops {
                assert_eq!(
                    session
                        .receive(request(1, op, serde_json::json!({})), status)
                        .reply()
                        .outcome,
                    ResponseOutcome::Err {
                        error: ProtocolError::new(ErrorKind::Forbidden)
                    }
                );
            }
        }
        let mut ui = ConnectionSession {
            state: State::Ready(ReadyRole::Ui {
                state: UiState::Starting,
            }),
            allowed: AllowedRoles::Quartz,
        };
        for op in [Op::TrackingPause, Op::TrackingKeepAlive, Op::PrivacyModeSet] {
            assert_eq!(
                ui.receive(request(2, op, serde_json::json!({})), status)
                    .reply()
                    .outcome,
                ResponseOutcome::Err {
                    error: ProtocolError::new(ErrorKind::BadRequest)
                }
            );
        }
        for body in [
            serde_json::json!({"enabled":true}),
            serde_json::json!({"enabled":true,"until_utc_ms":null}),
            serde_json::json!({"enabled":"true","until_utc_ms":100}),
        ] {
            assert_eq!(
                ui.receive(request(3, Op::PrivacyModeSet, body), status)
                    .reply()
                    .outcome,
                ResponseOutcome::Err {
                    error: ProtocolError::new(ErrorKind::BadRequest)
                }
            );
        }
        for op in [Op::TrackingPause, Op::TrackingKeepAlive] {
            assert!(matches!(
                ui.receive(
                    request(4, op, serde_json::json!({"until_utc_ms":100})),
                    status
                ),
                Action::Timed { .. }
            ));
        }
        assert!(matches!(
            ui.receive(
                request(5, Op::TrackingResume, serde_json::json!({})),
                status
            ),
            Action::Timed {
                command: super::super::TimedCommand::Resume,
                ..
            }
        ));
        assert!(matches!(
            ui.receive(
                request(6, Op::PrivacyModeSet, serde_json::json!({"enabled":false})),
                status
            ),
            Action::Timed {
                command: super::super::TimedCommand::DisablePrivacy,
                ..
            }
        ));
        assert!(matches!(
            ui.receive(
                request(
                    7,
                    Op::PrivacyModeSet,
                    serde_json::json!({"enabled":true,"until_utc_ms":100})
                ),
                status
            ),
            Action::Timed {
                command: super::super::TimedCommand::EnablePrivacy(_),
                ..
            }
        ));
    }
}
