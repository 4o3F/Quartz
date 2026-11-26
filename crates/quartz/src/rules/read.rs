//! Rule read projection and single-frame output over one immutable registry publication.

use crate::query::{JsonBytes, QUERY_MAX_RESPONSE_BYTES, fit_single_list};
use quartz_domain::rules::{
    self as domain,
    registry::{RuleSetDeletion, RuleSnapshot},
};
use quartz_protocol::{
    self as wire, ErrorKind, ProtocolError, RuleKind, RuleSetCounts, RuleSetEntry, RuleSetNode,
    RuleSetSource, RuleSetWarning, RuleSetsGetRequest, RuleSetsGetResponse, RuleSetsTreeRequest,
    RuleSetsTreeResponse, RuleView, RuleWarningCode,
};
use std::{collections::BTreeSet, sync::Arc};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum EntryOrder {
    Effective { index: usize },
    Inactive,
}

/// Captured together by the database actor after pending capture writes commit.
#[derive(Debug)]
pub(crate) struct ReadInputs {
    pub snapshot: Arc<RuleSnapshot>,
    pub metadata_version: i64,
}
/// Read work decoded before admission to the database channel.
#[derive(Debug, Clone)]
pub(crate) enum ReadRequest {
    Tree(RuleSetsTreeRequest),
    Get(RuleSetsGetRequest),
    Export(wire::RulesTextExportRequest),
}
/// Typed response bodies for rule read operations.
#[derive(Debug, serde::Serialize)]
#[serde(untagged)]
pub(crate) enum ReadResponse {
    Tree(RuleSetsTreeResponse),
    Get(RuleSetsGetResponse),
    Export(wire::RulesTextExportResponse),
}
impl ReadRequest {
    pub(crate) fn decode(op: wire::Op, body: serde_json::Value) -> Result<Self, serde_json::Error> {
        match op {
            wire::Op::RulesTextExport => serde_json::from_value(body).map(Self::Export),
            wire::Op::RuleSetsTree => serde_json::from_value(body).map(Self::Tree),
            wire::Op::RuleSetsGet => serde_json::from_value(body).map(Self::Get),
            _ => Err(<serde_json::Error as serde::de::Error>::custom(
                "operation has no rule read decoder",
            )),
        }
    }
    pub(crate) fn execute(self, inputs: ReadInputs) -> Result<ReadResponse, ProtocolError> {
        let versions = inputs.versions();
        match self {
            Self::Export(request) => inputs.export(request, versions).map(ReadResponse::Export),
            Self::Tree(request) => inputs.tree(request, versions).map(ReadResponse::Tree),
            Self::Get(request) => inputs.contents(request, versions).map(ReadResponse::Get),
        }
    }
}
impl ReadInputs {
    pub(crate) fn versions(&self) -> wire::QueryVersions {
        let sets: Vec<_> = self.snapshot.state().sets.iter().map(|item| {
            let revisions: Vec<_> = item.1.versions.values().map(|revision| serde_json::json!({
                "version": revision.version, "content": revision.content, "header": revision.header,
                "parent_version": revision.parent_version, "note": revision.note,
                "created_utc_ms": revision.created_utc_ms,
            })).collect();
            serde_json::json!({
                "key": item.0.as_str(), "header": item.1.current.set.header,
                "version": item.1.current.version,
                "enabled": item.1.current.enabled == domain::Enablement::Enabled,
                "deleted": item.1.deletion == RuleSetDeletion::Deleted,
                "sibling_order": item.1.current.sibling_order,
                "builtin_revision": item.1.builtin_revision,
                "source": match item.1.origin { domain::RuleOrigin::Builtin => RuleSetSource::Builtin, domain::RuleOrigin::User => RuleSetSource::User },
                "revisions": revisions,
            })
        }).collect();
        let encoded = serde_json::json!({"semantics": "rule-reads.v1", "metadata": self.metadata_version, "sets": sets});
        wire::QueryVersions(domain::ContentHash::of(&encoded.to_string()).to_string())
    }
    fn export(
        &self,
        request: wire::RulesTextExportRequest,
        versions: wire::QueryVersions,
    ) -> Result<wire::RulesTextExportResponse, ProtocolError> {
        let key: domain::RuleSetKey = request
            .key
            .parse()
            .map_err(|_| ProtocolError::new(ErrorKind::BadRequest))?;
        let missing = || {
            ProtocolError::new(ErrorKind::RuleSetNotFound {
                key: request.key.clone(),
            })
        };
        let set = self.snapshot.state().sets.get(&key).ok_or_else(missing)?;
        if set.deletion == RuleSetDeletion::Deleted {
            return Err(missing());
        }
        let text = set.current.set.canonical_text();
        if text.len() > wire::MAX_RULE_TEXT_BYTES {
            return Err(ProtocolError::new(ErrorKind::BadRequest));
        }
        let response = wire::RulesTextExportResponse { text, versions };
        if JsonBytes::measure(&response).map_err(|_| Self::internal())? > QUERY_MAX_RESPONSE_BYTES {
            return Err(Self::internal());
        }
        Ok(response)
    }
    fn kind(kind: RuleKind) -> domain::RuleKind {
        match kind {
            RuleKind::Classification => domain::RuleKind::Classification,
            RuleKind::Extraction => domain::RuleKind::Extraction,
            RuleKind::Privacy => domain::RuleKind::Privacy,
        }
    }
    fn tree(
        &self,
        request: RuleSetsTreeRequest,
        versions: wire::QueryVersions,
    ) -> Result<RuleSetsTreeResponse, ProtocolError> {
        let resolution = self
            .snapshot
            .resolution(Self::kind(request.kind))
            .ok_or_else(Self::internal)?;
        let truncated = resolution.nodes.len() > wire::RULE_SETS_TREE_MAX_NODES;
        let mut nodes =
            Vec::with_capacity(resolution.nodes.len().min(wire::RULE_SETS_TREE_MAX_NODES));
        for node in resolution.nodes.iter().take(wire::RULE_SETS_TREE_MAX_NODES) {
            let key = node.key.parse().map_err(|_| Self::internal())?;
            let set = &self.snapshot.state().sets[&key];
            let content = &set.current.set.content;
            let mut warnings: Vec<_> = resolution
                .overlay_warnings
                .iter()
                .filter(|warning| warning.key == node.key)
                .map(|warning| RuleSetWarning {
                    code: if warning.code == domain::DiagnosticCode::Conflict {
                        RuleWarningCode::Conflict
                    } else {
                        RuleWarningCode::TargetDeleted
                    },
                    target: warning.target.clone(),
                })
                .collect();
            warnings.sort_by(|left, right| {
                left.target
                    .cmp(&right.target)
                    .then_with(|| left.code.cmp(&right.code))
            });
            warnings.dedup();
            nodes.push(RuleSetNode {
                key: node.key.clone(),
                parent: set.current.set.header.parent.clone(),
                version: set.current.version,
                enabled: node.enabled == domain::Enablement::Enabled,
                sibling_order: set.current.sibling_order,
                source: match set.origin {
                    domain::RuleOrigin::Builtin => RuleSetSource::Builtin,
                    domain::RuleOrigin::User => RuleSetSource::User,
                },
                counts: RuleSetCounts {
                    added: content.rule.len(),
                    overridden: content.r#override.len(),
                    disabled: content.disable.len(),
                },
                warnings,
            });
        }
        let response = RuleSetsTreeResponse {
            kind: request.kind,
            effective_version: resolution.effective.version_hash.to_string(),
            nodes,
            versions,
            truncated,
        };
        fit_single_list(
            response,
            QUERY_MAX_RESPONSE_BYTES,
            |r| &mut r.nodes,
            |r| r.truncated = true,
        )
        .map_err(|_| Self::internal())
    }
    fn contents(
        &self,
        request: RuleSetsGetRequest,
        versions: wire::QueryVersions,
    ) -> Result<RuleSetsGetResponse, ProtocolError> {
        let key: domain::RuleSetKey = request
            .key
            .parse()
            .map_err(|_| ProtocolError::new(ErrorKind::BadRequest))?;
        let missing = || {
            ProtocolError::new(ErrorKind::RuleSetNotFound {
                key: request.key.clone(),
            })
        };
        let set = self.snapshot.state().sets.get(&key).ok_or_else(missing)?;
        if set.deletion == RuleSetDeletion::Deleted {
            return Err(missing());
        }
        let version = set.current.version;
        let revision = set.versions.get(&version).ok_or_else(missing)?;
        let kind = set.current.set.header.kind;
        let resolution = self.snapshot.resolution(kind).ok_or_else(Self::internal)?;
        let root = self.root(&request.key)?;
        let mut entries = Vec::new();
        for entry in &resolution.entries {
            let included = match request.view {
                RuleView::Own => entry.key == request.key,
                RuleView::Effective => self.root(&entry.key)? == root,
            };
            if included {
                entries.push(entry);
            }
        }
        if request.view == RuleView::Effective {
            entries.sort_by_key(|entry| {
                entry
                    .effective_order
                    .map_or(EntryOrder::Inactive, |index| EntryOrder::Effective {
                        index,
                    })
            });
        }
        let truncated = entries.len() > wire::RULE_SETS_GET_MAX_ENTRIES;
        let entries = entries
            .into_iter()
            .take(wire::RULE_SETS_GET_MAX_ENTRIES)
            .map(|entry| self.entry(entry))
            .collect::<Result<Vec<_>, _>>()?;
        let response = RuleSetsGetResponse {
            key: request.key,
            version,
            parent_version: revision.parent_version,
            entries,
            versions,
            truncated,
        };
        fit_single_list(
            response,
            QUERY_MAX_RESPONSE_BYTES,
            |r| &mut r.entries,
            |r| r.truncated = true,
        )
        .map_err(|_| Self::internal())
    }
    fn root(&self, key: &str) -> Result<String, ProtocolError> {
        let mut key: domain::RuleSetKey = key.parse().map_err(|_| Self::internal())?;
        let mut visited = BTreeSet::new();
        loop {
            if !visited.insert(key.clone()) {
                return Err(Self::internal());
            }
            let set = self
                .snapshot
                .state()
                .sets
                .get(&key)
                .ok_or_else(Self::internal)?;
            match &set.current.set.header.parent {
                Some(parent) => key = parent.parse().map_err(|_| Self::internal())?,
                None => return Ok(key.to_string()),
            }
        }
    }
    fn entry(&self, entry: &domain::ResolvedEntry) -> Result<RuleSetEntry, ProtocolError> {
        let key = entry.key.parse().map_err(|_| Self::internal())?;
        let content = &self
            .snapshot
            .state()
            .sets
            .get(&key)
            .ok_or_else(Self::internal)?
            .current
            .set
            .content;
        let encoded = entry.format_entry(content);
        if encoded.to_string().len() > domain::MAX_ENTRY_BYTES {
            return Err(Self::internal());
        }
        let result = serde_json::from_value(encoded).map_err(|_| Self::internal())?;
        Ok(result)
    }
    fn internal() -> ProtocolError {
        ProtocolError::new(ErrorKind::Internal)
    }
}

#[cfg(test)]
mod tests;
