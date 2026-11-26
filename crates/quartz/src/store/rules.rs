//! Rule-row conversion and execution of domain writes inside the caller's transaction.

use super::{
    enums::{RuleSetKind, RuleSetSource},
    rows::{RuleSetVersionsRow, RuleSetsRow},
    schema::{categories, rule_set_versions, rule_sets},
};
use diesel::{
    SqliteConnection, SqliteExpressionMethods,
    prelude::{
        Connection, ExpressionMethods, OptionalExtension, QueryDsl, RunQueryDsl, SelectableHelper,
    },
};
use quartz_domain::rules::registry::{
    ExpectedRuleVersion, RegistryState, RuleSetDeletion, RuleSetState, RuleSetVersion, RuleSetWrite,
};
use quartz_domain::rules::{
    self, Content, ContentHash, Enablement, InstalledSet, ReferenceCatalog, RuleKind, RuleOrigin,
    RuleSet, RuleSetKey, SetHeader, SourceLocation,
};
use serde::Serialize;
use snafu::{ResultExt, Snafu};
use std::collections::BTreeMap;

#[derive(Debug, Snafu)]
pub(crate) enum LoadRulesError {
    #[snafu(display("could not read persisted rule rows"))]
    Query { source: diesel::result::Error },
    #[snafu(display("could not decode stored rule content"))]
    Decode { source: serde_json::Error },
    #[snafu(display("stored rule set is missing its row identifier"))]
    MissingId,
    #[snafu(display("stored rule set refers to a missing parent row"))]
    MissingParent,
    #[snafu(display("stored rule set {set} is missing its version history"))]
    MissingVersionHistory { set: RuleSetKey },
    #[snafu(display("stored current revision is missing"))]
    MissingRevision,
    #[snafu(display("stored rule enablement is unsupported"))]
    InvalidEnablement { value: i64 },
    #[snafu(display("stored rule revision exceeds the supported range"))]
    InvalidVersion { source: std::num::TryFromIntError },
    #[snafu(display("stored contents do not match their SHA-256"))]
    HashMismatch,
    #[snafu(display("stored SHA-256 encoding is invalid"))]
    InvalidHash {
        source: rules::ParseContentHashError,
    },
    #[snafu(display("stored rule-set key is invalid"))]
    Identity { source: rules::ParseRuleSetKeyError },
}
impl From<diesel::result::Error> for LoadRulesError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Query { source }
    }
}

#[derive(Debug, Snafu)]
#[snafu(module(execute_error))]
pub(crate) enum ExecuteRuleWritesError {
    #[snafu(display("could not execute rule writes"))]
    Database { source: diesel::result::Error },
    #[snafu(display("persisted rule row has no identifier"))]
    MissingId,
    #[snafu(display(
        "persisted identity or revision no longer matches the write precondition for {key}"
    ))]
    PreconditionMismatch { key: RuleSetKey },
}
impl From<diesel::result::Error> for ExecuteRuleWritesError {
    fn from(source: diesel::result::Error) -> Self {
        Self::Database { source }
    }
}

/// SQLite stores rule enablement as exactly zero or one (03 §2.6).
#[derive(Debug, Clone, Copy)]
enum PersistedEnablement {
    Disabled,
    Enabled,
}
impl PersistedEnablement {
    fn decode(value: i64) -> Result<Self, LoadRulesError> {
        match value {
            0 => Ok(Self::Disabled),
            1 => Ok(Self::Enabled),
            value => Err(LoadRulesError::InvalidEnablement { value }),
        }
    }
    fn domain(self) -> Enablement {
        match self {
            Self::Disabled => Enablement::Disabled,
            Self::Enabled => Enablement::Enabled,
        }
    }
}

/// Read operations borrowing either a writer or a read-only connection.
pub(crate) struct RuleSets<'connection> {
    connection: &'connection mut SqliteConnection,
}
impl std::fmt::Debug for RuleSets<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuleSets").finish_non_exhaustive()
    }
}
#[derive(diesel::Queryable, diesel::Selectable)]
#[diesel(table_name = categories)]
struct CategoryReferenceRow {
    uuid: String,
    name: Option<String>,
    builtin_key: Option<String>,
}

/// Read the SQLite integer before narrowing can turn a malformed value into zero or one.
#[derive(diesel::Queryable, diesel::Selectable)]
#[diesel(table_name = rule_sets)]
struct StoredRuleSet {
    #[diesel(embed)]
    row: RuleSetsRow,
    #[diesel(select_expression = diesel::dsl::sql::<diesel::sql_types::BigInt>("rule_sets.enabled"))]
    #[diesel(select_expression_type = diesel::expression::SqlLiteral<diesel::sql_types::BigInt>)]
    enablement: i64,
}

impl<'connection> RuleSets<'connection> {
    pub(super) fn new(connection: &'connection mut SqliteConnection) -> Self {
        Self { connection }
    }
    pub(crate) fn load(&mut self) -> Result<RegistryState, LoadRulesError> {
        self.connection
            .transaction(|connection| RuleSets::new(connection).load_rows())
    }
    pub(crate) fn category_references(
        &mut self,
        archived: quartz_domain::categories::IncludeArchivedCategories,
    ) -> Result<ReferenceCatalog, LoadRulesError> {
        let mut query = categories::table.into_boxed();
        if archived == quartz_domain::categories::IncludeArchivedCategories::Exclude {
            query = query.filter(categories::archived.eq(0));
        }
        let rows = query
            .order(categories::uuid)
            .select(CategoryReferenceRow::as_select())
            .load::<CategoryReferenceRow>(self.connection)?;
        let mut references = ReferenceCatalog::default();
        for row in rows {
            references.insert_category(&row.uuid, row.builtin_key.as_deref(), row.name.as_deref());
        }
        Ok(references)
    }
    fn load_rows(&mut self) -> Result<RegistryState, LoadRulesError> {
        let rows = rule_sets::table
            .select(StoredRuleSet::as_select())
            .order(rule_sets::id)
            .load::<StoredRuleSet>(self.connection)
            .context(QuerySnafu)?;
        let keys: BTreeMap<_, _> = rows
            .iter()
            .map(|stored| {
                let row = &stored.row;
                Ok((
                    row.id.ok_or(LoadRulesError::MissingId)?,
                    row.key.parse::<RuleSetKey>().context(IdentitySnafu)?,
                ))
            })
            .collect::<Result<_, LoadRulesError>>()?;
        let mut versions: BTreeMap<i64, BTreeMap<u32, RuleSetVersion>> = BTreeMap::new();
        for row in rule_set_versions::table
            .select(RuleSetVersionsRow::as_select())
            .load::<RuleSetVersionsRow>(self.connection)
            .context(QuerySnafu)?
        {
            let saved = row.decode_content()?;
            let version = u32::try_from(row.version).context(InvalidVersionSnafu)?;
            versions.entry(row.set_id).or_default().insert(
                version,
                RuleSetVersion {
                    version,
                    content: saved.content,
                    header: saved.set,
                    parent_version: row
                        .parent_version
                        .map(u32::try_from)
                        .transpose()
                        .context(InvalidVersionSnafu)?,
                    note: row.note,
                    created_utc_ms: Some(row.created_utc_ms),
                },
            );
        }
        let references = self
            .category_references(quartz_domain::categories::IncludeArchivedCategories::Exclude)?;
        let mut sets = BTreeMap::new();
        for stored in rows {
            let row = stored.row;
            let enablement = PersistedEnablement::decode(stored.enablement)?;
            let id = row.id.ok_or(LoadRulesError::MissingId)?;
            let key = keys.get(&id).ok_or(LoadRulesError::MissingId)?.clone();
            let parent = row
                .parent_id
                .map(|id| {
                    keys.get(&id)
                        .map(ToString::to_string)
                        .ok_or(LoadRulesError::MissingParent)
                })
                .transpose()?;
            let version = u32::try_from(row.current_version).context(InvalidVersionSnafu)?;
            let history = versions
                .remove(&id)
                .ok_or_else(|| LoadRulesError::MissingVersionHistory { set: key.clone() })?;
            let content = history
                .get(&version)
                .ok_or(LoadRulesError::MissingRevision)?
                .content
                .clone();
            let legacy_header = SetHeader {
                key: row.key,
                kind: row.kind.domain_kind(),
                name: row.name,
                parent,
                description: None,
                chain: None,
            };
            // Legacy rows have no header snapshot; only the row header remains available.
            let header = history[&version].header.clone().unwrap_or(legacy_header);
            let set = RuleSet {
                header,
                content,
                locations: BTreeMap::from([(
                    String::new(),
                    SourceLocation::database(&key, version),
                )]),
            };
            sets.insert(
                key,
                RuleSetState {
                    current: InstalledSet {
                        set,
                        version,
                        enabled: enablement.domain(),
                        sibling_order: row.sibling_order,
                    },
                    origin: match row.source {
                        RuleSetSource::Builtin => RuleOrigin::Builtin,
                        RuleSetSource::User => RuleOrigin::User,
                    },
                    builtin_revision: row
                        .builtin_revision
                        .map(u32::try_from)
                        .transpose()
                        .context(InvalidVersionSnafu)?,
                    deletion: if row.deleted_utc_ms.is_some() {
                        RuleSetDeletion::Deleted
                    } else {
                        RuleSetDeletion::Active
                    },
                    versions: history,
                },
            );
        }
        Ok(RegistryState { sets, references })
    }
}

/// Created only by Store::transaction; execution never starts or commits its own transaction.
pub(crate) struct RuleSetTransaction<'connection> {
    connection: &'connection mut SqliteConnection,
}
impl std::fmt::Debug for RuleSetTransaction<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuleSetTransaction").finish_non_exhaustive()
    }
}
impl<'connection> RuleSetTransaction<'connection> {
    pub(super) fn new(connection: &'connection mut SqliteConnection) -> Self {
        Self { connection }
    }
    pub(crate) fn execute(
        &mut self,
        writes: &[RuleSetWrite],
        committed_at_ms: i64,
    ) -> Result<(), ExecuteRuleWritesError> {
        for write in writes {
            let update = write.update();
            let parent_id = update
                .set
                .header
                .parent
                .as_ref()
                .map(|key| {
                    rule_sets::table
                        .filter(rule_sets::key.eq(key))
                        .select(rule_sets::id)
                        .first::<Option<i64>>(self.connection)
                        .context(execute_error::DatabaseSnafu)?
                        .ok_or(ExecuteRuleWritesError::MissingId)
                })
                .transpose()?;
            let kind = RuleSetKind::from_domain(update.set.header.kind);
            let source = match update.origin {
                RuleOrigin::Builtin => RuleSetSource::Builtin,
                RuleOrigin::User => RuleSetSource::User,
            };
            let builtin_revision = update.builtin_revision.map(i64::from);
            let enabled = match update.enabled {
                Enablement::Enabled => 1,
                Enablement::Disabled => 0,
            };
            let deleted_utc_ms = match update.deletion {
                RuleSetDeletion::Active => None,
                RuleSetDeletion::Deleted => Some(committed_at_ms),
            };
            let current_version = i64::from(update.revision.version);
            let id = match update.expected {
                ExpectedRuleVersion::Absent => diesel::insert_into(rule_sets::table)
                    .values(&RuleSetsRow {
                        id: None,
                        key: update.key.to_string(),
                        kind,
                        parent_id,
                        source,
                        builtin_revision,
                        name: update.set.header.name.clone(),
                        enabled,
                        deleted_utc_ms,
                        sibling_order: update.sibling_order,
                        current_version,
                        chain_id: None,
                        created_utc_ms: committed_at_ms,
                    })
                    .returning(rule_sets::id)
                    .get_result::<Option<i64>>(self.connection)
                    .context(execute_error::DatabaseSnafu)?
                    .ok_or(ExecuteRuleWritesError::MissingId)?,
                ExpectedRuleVersion::Version(expected) => diesel::update(
                    rule_sets::table
                        .filter(rule_sets::key.eq(update.key.as_str()))
                        .filter(rule_sets::current_version.eq(i64::from(expected)))
                        .filter(rule_sets::kind.eq(kind))
                        .filter(rule_sets::parent_id.is(parent_id))
                        .filter(rule_sets::source.eq(source)),
                )
                .set((
                    rule_sets::builtin_revision.eq(builtin_revision),
                    rule_sets::name.eq(&update.set.header.name),
                    rule_sets::enabled.eq(enabled),
                    rule_sets::deleted_utc_ms.eq(deleted_utc_ms),
                    rule_sets::sibling_order.eq(update.sibling_order),
                    rule_sets::current_version.eq(current_version),
                ))
                .returning(rule_sets::id)
                .get_result::<Option<i64>>(self.connection)
                .optional()
                .context(execute_error::DatabaseSnafu)?
                .flatten()
                .ok_or_else(|| ExecuteRuleWritesError::PreconditionMismatch {
                    key: update.key.clone(),
                })?,
            };
            let saved = StoredRevisionContent {
                content: update.revision.content.clone(),
                set: update.revision.header.clone(),
            };
            let content = serde_json::to_string(&saved).expect("stored rule revision is JSON-safe");
            let content_hash = ContentHash::of(&content).to_string();
            diesel::insert_into(rule_set_versions::table)
                .values(&RuleSetVersionsRow {
                    set_id: id,
                    version: current_version,
                    content,
                    content_hash,
                    parent_version: update.revision.parent_version.map(i64::from),
                    note: update.revision.note.clone(),
                    created_utc_ms: committed_at_ms,
                })
                .execute(self.connection)
                .context(execute_error::DatabaseSnafu)?;
        }
        Ok(())
    }
}
impl RuleSetKind {
    fn domain_kind(self) -> RuleKind {
        match self {
            Self::Classification => RuleKind::Classification,
            Self::Extraction => RuleKind::Extraction,
            Self::Privacy => RuleKind::Privacy,
            Self::Precedent => RuleKind::Precedent,
            Self::PolicyCheck => RuleKind::PolicyCheck,
        }
    }
    fn from_domain(kind: RuleKind) -> Self {
        match kind {
            RuleKind::Classification => Self::Classification,
            RuleKind::Extraction => Self::Extraction,
            RuleKind::Privacy => Self::Privacy,
            RuleKind::Precedent => Self::Precedent,
            RuleKind::PolicyCheck => Self::PolicyCheck,
        }
    }
}
#[derive(Serialize)]
struct StoredRevisionContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    set: Option<SetHeader>,
    #[serde(flatten)]
    content: Content,
}
impl StoredRevisionContent {
    fn decode(text: &str) -> Result<Self, serde_json::Error> {
        let mut fields: serde_json::Map<String, serde_json::Value> = serde_json::from_str(text)?;
        let set = match fields.remove("set") {
            Some(value) => serde_json::from_value(value)?,
            None => None,
        };
        // Decode the remaining object directly so Content rejects unknown fields.
        let content = serde_json::from_value(serde_json::Value::Object(fields))?;
        Ok(Self { set, content })
    }
}
impl RuleSetVersionsRow {
    fn decode_content(&self) -> Result<StoredRevisionContent, LoadRulesError> {
        let stored = self
            .content_hash
            .parse::<ContentHash>()
            .context(InvalidHashSnafu)?;
        snafu::ensure!(stored == ContentHash::of(&self.content), HashMismatchSnafu);
        StoredRevisionContent::decode(&self.content).context(DecodeSnafu)
    }
}

#[cfg(test)]
mod tests;
