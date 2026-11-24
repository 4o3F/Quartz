//! Rule read projection and byte-bounded paging over one immutable registry publication.

use crate::query::paging::{PageBudget, PagePosition};
use quartz_domain::rules::{
    self as domain,
    registry::{RuleSetDeletion, RuleSnapshot},
};
use quartz_protocol::{
    self as wire, ErrorKind, ProtocolError, RuleKind, RuleSetCounts, RuleSetEntry, RuleSetNode,
    RuleSetRevision, RuleSetSource, RuleSetWarning, RuleSetsGetRequest, RuleSetsGetResponse,
    RuleSetsTreeRequest, RuleSetsTreeResponse, RuleSetsVersionsRequest, RuleSetsVersionsResponse,
    RuleView, RuleWarningCode,
};
use std::{collections::BTreeSet, sync::Arc};

/// Historical selection contains one chain, so every node has the same sibling position.
const HISTORICAL_SIBLING_ORDER: i64 = 0;

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
    Versions(RuleSetsVersionsRequest),
}
/// Typed response bodies for the three rule read operations.
#[derive(Debug, serde::Serialize)]
#[serde(untagged)]
pub(crate) enum ReadResponse {
    Tree(RuleSetsTreeResponse),
    Get(RuleSetsGetResponse),
    Versions(RuleSetsVersionsResponse),
}
impl ReadRequest {
    pub(crate) fn decode(op: wire::Op, body: serde_json::Value) -> Result<Self, serde_json::Error> {
        let request = match op {
            wire::Op::RuleSetsTree => serde_json::from_value(body).map(Self::Tree),
            wire::Op::RuleSetsGet => serde_json::from_value(body).map(Self::Get),
            wire::Op::RuleSetsVersions => serde_json::from_value(body).map(Self::Versions),
            _ => Err(<serde_json::Error as serde::de::Error>::custom(
                "operation has no rule read decoder",
            )),
        }?;
        match &request {
            Self::Tree(body) => {
                if let Some(cursor) = &body.cursor {
                    cursor.validate_query(&wire::CursorQuery::RuleSetsTree {
                        rule_kind: body.kind,
                    })?;
                }
            }
            Self::Get(body) => {
                if let Some(cursor) = &body.cursor {
                    cursor.validate_query(&wire::CursorQuery::RuleSetsGet {
                        key: body.key.clone(),
                        version: body.version,
                        view: body.view,
                    })?;
                }
            }
            Self::Versions(body) => {
                if let Some(cursor) = &body.cursor {
                    cursor.validate_query(&wire::CursorQuery::RuleSetsVersions {
                        key: body.key.clone(),
                    })?;
                }
            }
        }
        Ok(request)
    }
    pub(crate) fn execute(self, inputs: ReadInputs) -> Result<ReadResponse, ProtocolError> {
        let versions = inputs.versions();
        match self {
            Self::Tree(request) => inputs.tree(request, versions).map(ReadResponse::Tree),
            Self::Get(request) => inputs.contents(request, versions).map(ReadResponse::Get),
            Self::Versions(request) => inputs
                .revisions(request, versions)
                .map(ReadResponse::Versions),
        }
    }
}
impl ReadInputs {
    fn versions(&self) -> wire::QueryVersions {
        let sets: Vec<_> = self.snapshot.state().sets.iter().map(|item| {
            let revisions: Vec<_> = item.1.versions.values().map(|revision| serde_json::json!({
                "version": revision.version, "content": revision.content,
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
        let query = wire::CursorQuery::RuleSetsTree {
            rule_kind: request.kind,
        };
        let resolution = self
            .snapshot
            .resolution(Self::kind(request.kind))
            .ok_or_else(Self::internal)?;
        let mut response = RuleSetsTreeResponse {
            kind: request.kind,
            effective_version: resolution.effective.version_hash.to_string(),
            nodes: Vec::new(),
            versions,
            next_cursor: None,
        };
        let page = Self::budget(&response, &response.versions, &query)?
            .page_mapped(
                resolution.nodes.iter().collect(),
                Self::position(request.cursor.as_ref(), &response.versions, &query)?,
                |node| {
                    let key = node
                        .key
                        .parse()
                        .map_err(|_| crate::query::RunQueryError::InvalidRuleEntry)?;
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
                    for change in self
                        .snapshot
                        .upstream_changes()
                        .iter()
                        .filter(|change| change.set_key().as_str() == node.key)
                    {
                        let warning = RuleSetWarning {
                            code: match change.state() {
                                domain::registry::UpstreamState::TargetDeleted => {
                                    RuleWarningCode::TargetDeleted
                                }
                                domain::registry::UpstreamState::UpstreamChanged => {
                                    RuleWarningCode::UpstreamChanged
                                }
                            },
                            target: change.target().to_string(),
                        };
                        if !warnings.contains(&warning) {
                            warnings.push(warning);
                        }
                    }
                    warnings.sort_by(|left, right| {
                        left.target
                            .cmp(&right.target)
                            .then_with(|| left.code.cmp(&right.code))
                    });
                    warnings.dedup();
                    Ok(RuleSetNode {
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
                    })
                },
            )
            .map_err(Self::paging_error)?;
        response.nodes = page.items;
        response.next_cursor = page
            .next
            .map(|next| wire::QueryCursor::new(response.versions.clone(), query, next));
        Ok(response)
    }
    fn contents(
        &self,
        request: RuleSetsGetRequest,
        versions: wire::QueryVersions,
    ) -> Result<RuleSetsGetResponse, ProtocolError> {
        let query = wire::CursorQuery::RuleSetsGet {
            key: request.key.clone(),
            version: request.version,
            view: request.view,
        };
        let position = Self::position(request.cursor.as_ref(), &versions, &query)?;
        let key: domain::RuleSetKey = request
            .key
            .parse()
            .map_err(|_| ProtocolError::new(ErrorKind::BadRequest))?;
        let missing = || {
            ProtocolError::new(ErrorKind::RuleSetNotFound {
                key: request.key.clone(),
                version: request.version.map(u32::from),
            })
        };
        let set = self.snapshot.state().sets.get(&key).ok_or_else(missing)?;
        if request.version.is_none() && set.deletion == RuleSetDeletion::Deleted {
            return Err(missing());
        }
        let version = request.version.map_or(set.current.version, u32::from);
        let revision = set.versions.get(&version).ok_or_else(missing)?;
        let kind = set.current.set.header.kind;
        let historical;
        let historical_sets = request
            .version
            .map(|_| self.saved_chain(&key, version))
            .transpose()?;
        let resolution = if request.version.is_some() {
            historical = domain::Resolution::resolve(
                historical_sets.as_deref().ok_or_else(Self::internal)?,
                kind,
            );
            &historical
        } else {
            self.snapshot.resolution(kind).ok_or_else(Self::internal)?
        };
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
        let mut response = RuleSetsGetResponse {
            key: request.key.clone(),
            version,
            parent_version: revision.parent_version,
            entries: Vec::new(),
            versions,
            next_cursor: None,
        };
        let page = Self::budget(&response, &response.versions, &query)?
            .page_mapped(entries, position, |entry| {
                self.entry(entry, historical_sets.as_deref())
                    .map_err(|_| crate::query::RunQueryError::InvalidRuleEntry)
            })
            .map_err(Self::paging_error)?;
        response.entries = page.items;
        response.next_cursor = page
            .next
            .map(|next| wire::QueryCursor::new(response.versions.clone(), query, next));
        Ok(response)
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
    fn saved_chain(
        &self,
        key: &domain::RuleSetKey,
        version: u32,
    ) -> Result<Vec<domain::InstalledSet>, ProtocolError> {
        let mut sets = Vec::new();
        let mut key = key.clone();
        let mut version = version;
        let mut visited = BTreeSet::new();
        loop {
            if !visited.insert(key.clone()) {
                return Err(Self::internal());
            }
            let entry = self
                .snapshot
                .state()
                .sets
                .get(&key)
                .ok_or_else(Self::internal)?;
            let revision = entry.versions.get(&version).ok_or_else(Self::internal)?;
            let mut installed = entry.current.clone();
            installed.version = version;
            installed.set.content = revision.content.clone();
            // Historical content is resolved independently of current subtree enablement and ordering.
            installed.enabled = domain::Enablement::Enabled;
            installed.sibling_order = HISTORICAL_SIBLING_ORDER;
            sets.push(installed);
            match (&entry.current.set.header.parent, revision.parent_version) {
                (None, None) => return Ok(sets),
                (Some(parent), Some(parent_version)) => {
                    key = parent.parse().map_err(|_| Self::internal())?;
                    version = parent_version;
                }
                _ => return Err(Self::internal()),
            }
        }
    }
    fn entry(
        &self,
        entry: &domain::ResolvedEntry,
        historical: Option<&[domain::InstalledSet]>,
    ) -> Result<RuleSetEntry, ProtocolError> {
        let content = if let Some(sets) = historical {
            &sets
                .iter()
                .find(|set| set.set.header.key == entry.key)
                .ok_or_else(Self::internal)?
                .set
                .content
        } else {
            let key = entry.key.parse().map_err(|_| Self::internal())?;
            &self
                .snapshot
                .state()
                .sets
                .get(&key)
                .ok_or_else(Self::internal)?
                .current
                .set
                .content
        };
        let encoded = entry.format_entry(content);
        if encoded.to_string().len() > domain::MAX_ENTRY_BYTES {
            return Err(Self::internal());
        }
        let result = serde_json::from_value(encoded).map_err(|_| Self::internal())?;
        Ok(result)
    }
    fn revisions(
        &self,
        request: RuleSetsVersionsRequest,
        versions: wire::QueryVersions,
    ) -> Result<RuleSetsVersionsResponse, ProtocolError> {
        let query = wire::CursorQuery::RuleSetsVersions {
            key: request.key.clone(),
        };
        let position = Self::position(request.cursor.as_ref(), &versions, &query)?;
        let key = request
            .key
            .parse()
            .map_err(|_| ProtocolError::new(ErrorKind::BadRequest))?;
        let set = self.snapshot.state().sets.get(&key).ok_or_else(|| {
            ProtocolError::new(ErrorKind::RuleSetNotFound {
                key: request.key.clone(),
                version: None,
            })
        })?;
        let mut response = RuleSetsVersionsResponse {
            revisions: Vec::new(),
            versions,
            next_cursor: None,
        };
        let page = Self::budget(&response, &response.versions, &query)?
            .page_mapped(
                set.versions.values().rev().collect(),
                position,
                |revision| {
                    Ok(RuleSetRevision {
                        version: revision.version,
                        parent_version: revision.parent_version,
                        created_utc_ms: revision
                            .created_utc_ms
                            .ok_or(crate::query::RunQueryError::InvalidRuleEntry)?,
                        note: revision.note.clone(),
                    })
                },
            )
            .map_err(Self::paging_error)?;
        response.revisions = page.items;
        response.next_cursor = page
            .next
            .map(|next| wire::QueryCursor::new(response.versions.clone(), query, next));
        Ok(response)
    }
    fn budget(
        response: &impl serde::Serialize,
        versions: &wire::QueryVersions,
        query: &wire::CursorQuery,
    ) -> Result<PageBudget, ProtocolError> {
        PageBudget::new(response)
            .and_then(|budget| {
                budget.with_cursor(&wire::QueryCursor::new(
                    versions.clone(),
                    query.clone(),
                    usize::MAX,
                ))
            })
            .map_err(Self::paging_error)
    }
    fn position(
        cursor: Option<&wire::QueryCursor>,
        versions: &wire::QueryVersions,
        query: &wire::CursorQuery,
    ) -> Result<PagePosition, ProtocolError> {
        PagePosition::from_cursor(cursor, versions, query).map_err(Self::paging_error)
    }
    fn paging_error(error: crate::query::RunQueryError) -> ProtocolError {
        ProtocolError::new(error.kind())
    }
    fn internal() -> ProtocolError {
        ProtocolError::new(ErrorKind::Internal)
    }
}

#[cfg(test)]
mod tests;
