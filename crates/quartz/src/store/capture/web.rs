//! Sanitized linked web interval row writes.

use super::{Capture, CaptureColumn, DbRowId, ExecuteCaptureBatchError, LogicalKey};
use crate::store::{
    enums,
    rows::WebIntervalsRow,
    schema::{domains, web_intervals},
};
use diesel::NullableExpressionMethods;
use diesel::prelude::{ExpressionMethods, QueryDsl, RunQueryDsl, SelectableHelper};
use quartz_domain::capture::{
    BrowserAudible, OpenWebInterval, PrivacyExplanation, PrivacyLabel, RowId, RunId, Timestamp,
    UtcInstant, WebEndReason,
};

/// `web_intervals.link_status` code for `linked` (schema v1 `enum_labels`).
const LINK_STATUS_LINKED: i32 = 1;
/// `web_intervals.audible` code when the linked tab is silent.
const AUDIBLE_SILENT: i32 = 0;
/// `web_intervals.audible` code when the linked tab is producing audio.
const AUDIBLE_ACTIVE: i32 = 1;

impl Capture<'_> {
    pub(super) fn open_web_interval(
        &mut self,
        run: RunId,
        open: &OpenWebInterval,
    ) -> Result<(), ExecuteCaptureBatchError> {
        let interval = self.interval(run, open.interval)?;
        let domain_id = open
            .domain
            .as_deref()
            .map(|host| self.domain(host).map(DbRowId::get))
            .transpose()?;
        let tab_title_id = open
            .tab_title
            .as_deref()
            .map(|text| self.title(text).map(DbRowId::get))
            .transpose()?;
        let row = WebIntervalsRow::opened(interval, open, domain_id, tab_title_id)?;
        let replacement = DbRowId::new(
            diesel::insert_into(web_intervals::table)
                .values(&row)
                .returning(web_intervals::id.assume_not_null())
                .get_result(self.connection)?,
        );
        if let Some(existing) = self
            .run_keys(run)
            .web_intervals
            .insert(open.id, replacement)
        {
            return Err(ExecuteCaptureBatchError::DuplicateKey {
                run,
                key: LogicalKey::WebInterval(open.id),
                existing: existing.get(),
                replacement: replacement.get(),
            });
        }
        self.bump_utc_days(enums::DataSource::Web, open.start.utc, open.start.utc)
    }

    fn domain(&mut self, host: &str) -> Result<DbRowId, ExecuteCaptureBatchError> {
        diesel::insert_into(domains::table)
            .values(domains::host.eq(host))
            .on_conflict(domains::host)
            .do_update()
            .set(domains::host.eq(host))
            .returning(domains::id.assume_not_null())
            .get_result(self.connection)
            .map(DbRowId::new)
            .map_err(ExecuteCaptureBatchError::from)
    }

    pub(super) fn close_web_interval(
        &mut self,
        run: RunId,
        logical: RowId,
        at: Timestamp,
        reason: WebEndReason,
    ) -> Result<(), ExecuteCaptureBatchError> {
        let id = self.web_interval(run, logical)?;
        self.close_web_row(id, at.utc, reason)?;
        self.run_keys(run).web_intervals.remove(&logical);
        Ok(())
    }

    pub(super) fn close_web_row(
        &mut self,
        id: DbRowId,
        end: UtcInstant,
        reason: WebEndReason,
    ) -> Result<(), ExecuteCaptureBatchError> {
        let row = web_intervals::table
            .filter(web_intervals::id.eq(id.get()))
            .filter(web_intervals::end_utc_ms.is_null())
            .select(WebIntervalsRow::as_select())
            .first(self.connection)?;
        let context = id.context();
        let start = context.utc(row.start_utc_ms, web_intervals::start_utc_ms)?;
        context.confirmation_utc(start, end, web_intervals::end_utc_ms)?;
        let end_utc_ms = context.field(web_intervals::end_utc_ms).utc(end)?;
        let count = diesel::update(
            web_intervals::table
                .filter(web_intervals::id.eq(id.get()))
                .filter(web_intervals::end_utc_ms.is_null()),
        )
        .set((
            web_intervals::end_utc_ms.eq(Some(end_utc_ms)),
            web_intervals::confirmed_utc_ms.eq(None::<i64>),
            web_intervals::end_reason.eq(Some(reason.code())),
        ))
        .execute(self.connection)?;
        id.require_open(web_intervals::table, count)?;
        self.bump_utc_days(enums::DataSource::Web, start, end)
    }
}

impl WebIntervalsRow {
    fn opened(
        interval: DbRowId,
        open: &OpenWebInterval,
        domain_id: Option<i64>,
        tab_title_id: Option<i64>,
    ) -> Result<Self, ExecuteCaptureBatchError> {
        let start_utc_ms = CaptureColumn::new(web_intervals::start_utc_ms).utc(open.start.utc)?;
        let audible = match open.audible {
            BrowserAudible::Silent => AUDIBLE_SILENT,
            BrowserAudible::Audible => AUDIBLE_ACTIVE,
        };
        let mut row = Self {
            id: None,
            stream_id: open.stream.get(),
            interval_id: interval.get(),
            start_utc_ms,
            end_utc_ms: None,
            confirmed_utc_ms: Some(start_utc_ms),
            end_reason: None,
            domain_id,
            tab_title_id,
            tab_title_status: open.tab_title_status.into(),
            tab_id: Some(open.tab_id),
            browser_window_id: open.browser_window_id,
            navigation_id: None,
            audible,
            link_status: LINK_STATUS_LINKED,
            web_level: open.web_level.code(),
            privacy_rule_id: None,
            privacy_label_key: None,
            privacy_label: None,
        };
        row.apply_privacy(open.privacy.as_ref());
        Ok(row)
    }

    fn apply_privacy(&mut self, privacy: Option<&PrivacyExplanation>) {
        if let Some(privacy) = privacy {
            self.privacy_rule_id = Some(privacy.rule_id.clone());
            match &privacy.label {
                Some(PrivacyLabel::Key(key)) => self.privacy_label_key = Some(key.clone()),
                Some(PrivacyLabel::Text(text)) => self.privacy_label = Some(text.clone()),
                None => {}
            }
        }
    }
}
