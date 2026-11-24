//! One encoded-byte budget shared by mutation failures and validation replies.
use crate::query::paging::JsonBytes;
use quartz_domain::rules as domain;
use quartz_protocol::{self as wire, ErrorKind, ProtocolError};

pub(super) struct Diagnostics;
enum DiagnosticLocations<'keys> {
    Text,
    Submitted(&'keys [domain::RuleSetKey]),
}
impl Diagnostics {
    pub(super) fn error(diagnostics: &[domain::Diagnostic]) -> ProtocolError {
        Self::bounded(diagnostics, DiagnosticLocations::Text)
    }
    pub(super) fn tree(
        diagnostics: &[domain::Diagnostic],
        keys: &[domain::RuleSetKey],
    ) -> ProtocolError {
        Self::bounded(diagnostics, DiagnosticLocations::Submitted(keys))
    }
    fn bounded(
        diagnostics: &[domain::Diagnostic],
        locations: DiagnosticLocations<'_>,
    ) -> ProtocolError {
        let empty = ProtocolError::new(ErrorKind::RuleValidationFailed {
            diagnostics: Vec::new(),
            truncated: false,
        });
        let mut remaining = wire::QUERY_PAGE_BYTES
            - JsonBytes::measure(&empty).expect("validation envelope is JSON-safe");
        let mut items = Vec::new();
        for diagnostic in diagnostics.iter().take(wire::MAX_RULE_DIAGNOSTICS) {
            let item = wire::RuleDiagnostic {
                code: diagnostic.code.as_str().into(),
                path: diagnostic.path.clone(),
                location: (diagnostic.location.has_source_span()
                    && match locations {
                        DiagnosticLocations::Text => true,
                        DiagnosticLocations::Submitted(keys) => {
                            keys.iter().any(|key| key.as_str() == diagnostic.set_key)
                        }
                    })
                .then_some(wire::RuleDiagnosticLocation {
                    line: diagnostic.location.line,
                    column: diagnostic.location.column,
                }),
                params: Default::default(),
            };
            let separator_bytes = usize::from(!items.is_empty());
            let item_bytes =
                JsonBytes::measure(&item).expect("rule diagnostic is JSON-safe") + separator_bytes;
            if item_bytes > remaining {
                break;
            }
            remaining -= item_bytes;
            items.push(item);
        }
        let truncated = items.len() < diagnostics.len();
        ProtocolError::new(ErrorKind::RuleValidationFailed {
            diagnostics: items,
            truncated,
        })
    }
}
