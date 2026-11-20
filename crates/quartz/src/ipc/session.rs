//! Pure connection state and request decisions, independent of named pipes.

use quartz_protocol::{
    CoreStatusRequest, CoreStatusResponse, Envelope, ErrorKind, Hello, HelloResponse, Op,
    PROTOCOL_VERSION, ProtoRange, ProtocolError, RequestId, Response, ResponseOutcome, Role,
    Subscribe, UiFirstFrameRequest, UiOpenRequest,
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
    FirstFrame,
}
#[derive(Debug)]
enum State {
    HelloPending,
    Ready(Role),
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
    Ui {
        id: RequestId,
        command: UiCommand,
    },
    Subscribe {
        id: RequestId,
        subscription: Subscribe,
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
    pub(crate) fn role(&self) -> Option<Role> {
        match self.state {
            State::Ready(role) => Some(role),
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
                    Err(error) => return Action::error(Some(id), error.into()),
                };
                let role = hello.role.role();
                // TODO(M5): Authenticate registered host browser streams and enable host runtime.
                if !self.allowed.permits(role) || role == Role::Host {
                    return Action::error(Some(id), ErrorKind::Forbidden);
                }
                self.state = State::Ready(role);
                Action::success(
                    id,
                    HelloResponse {
                        proto,
                        instance_id: status.instance_id.clone(),
                        core_version: status.core_version.clone(),
                        capabilities: Vec::new(),
                    },
                )
            }
            State::Ready(role) => {
                if request.op == Op::Unknown {
                    return Action::error(Some(id), ErrorKind::UnknownOp);
                }
                if role.authorize(request.op).is_err() {
                    return Action::error(Some(id), ErrorKind::Forbidden);
                }
                match request.op {
                    Op::CoreStatus => {
                        if serde_json::from_value::<CoreStatusRequest>(request.body).is_err() {
                            return Action::error(Some(id), ErrorKind::BadRequest);
                        }
                        Action::success(id, status)
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
                            UiCommand::FirstFrame
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
                    Op::QueryDay | Op::QueryTimeline => Action::from_query(
                        id,
                        crate::query::QueryRequest::decode(request.op, request.body),
                    ),
                    Op::Hello
                    | Op::QueryRange
                    | Op::QuerySegmentDetail
                    | Op::QueryEntityDetail
                    | Op::QueryUnclassified
                    | Op::QueryPendingAway
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
                    | Op::CategoriesList
                    | Op::CategoriesCreate
                    | Op::CategoriesUpdate
                    | Op::CategoriesDelete
                    | Op::CategoriesReorder
                    | Op::AnnotationsCreate
                    | Op::AnnotationsUpdate
                    | Op::AnnotationsDelete
                    | Op::SettingsGet
                    | Op::SettingsSet
                    | Op::DataDelete
                    | Op::TrackingPause
                    | Op::TrackingResume
                    | Op::TrackingKeepAlive
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
                    | Op::PrivacyModeSet
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
        assert!(matches!(
            response.reply().outcome,
            ResponseOutcome::Ok { .. }
        ));
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
}
