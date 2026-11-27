//! Week page view model: effective week boundaries, clamped `query.range` requests, stacked
//! daily category columns, per-day focus/distraction shares, and coalesced day-event refresh.

use std::{collections::BTreeMap, time::Duration};

use jiff::{ToSpan as _, civil::Weekday};
use quartz_protocol::{self as wire, LocalDate};
use snafu::Snafu;

use crate::{
    link::LoadDataError,
    model::{
        BuiltinCategory, CategoryCatalog, CategoryColor, CategoryName, LoadProgress,
        ProgressUpdate, QueryProgress, Share, Tendency, live_details::CategoryDisplay,
    },
};

/// Seven calendar days in a Gregorian week.
pub(crate) const WEEK_DAY_COUNT: usize = 7;
/// Seven calendar days as a signed offset for weekday arithmetic.
const DAYS_PER_WEEK_I8: i8 = 7;
/// Seven calendar days as a signed span offset for `jiff` date arithmetic.
const DAYS_PER_WEEK_I64: i64 = 7;
/// Offset from week start to the inclusive seventh day (`start + 6 days`).
const LAST_WEEK_DAY_OFFSET: i64 = 6;
/// Seconds in one hour, used for rounding chart y-axis bounds.
const SECONDS_PER_HOUR_U64: u64 = 3_600;
/// Clean even-hour y-axis ceilings for daily stacked columns so the midpoint tick stays whole.
const CHART_SCALE_STEPS_HOURS: [u64; 8] = [1, 2, 4, 6, 8, 10, 12, 16];
/// Default y-axis ceiling when the week has no recorded activity.
const DEFAULT_EMPTY_SCALE_HOURS: u64 = 2;

/// Converts a protocol [`wire::Weekday`] into a `jiff` [`Weekday`].
#[must_use]
pub(crate) const fn weekday_from_protocol(weekday: wire::Weekday) -> Weekday {
    match weekday {
        wire::Weekday::Monday => Weekday::Monday,
        wire::Weekday::Tuesday => Weekday::Tuesday,
        wire::Weekday::Wednesday => Weekday::Wednesday,
        wire::Weekday::Thursday => Weekday::Thursday,
        wire::Weekday::Friday => Weekday::Friday,
        wire::Weekday::Saturday => Weekday::Saturday,
        wire::Weekday::Sunday => Weekday::Sunday,
    }
}

/// First logical date of the 7-day week containing `date` when weeks begin on `first_weekday`.
#[must_use]
pub(crate) fn week_start_for(date: LocalDate, first_weekday: Weekday) -> Option<LocalDate> {
    let offset = (date.weekday().to_monday_zero_offset() - first_weekday.to_monday_zero_offset())
        .rem_euclid(DAYS_PER_WEEK_I8);
    date.checked_sub(i64::from(offset).days()).ok()
}

/// Last inclusive logical date (`week_start + 6 days`) of the 7-day week starting at `week_start`.
#[must_use]
pub(crate) fn week_end_inclusive(week_start: LocalDate) -> Option<LocalDate> {
    week_start.checked_add(LAST_WEEK_DAY_OFFSET.days()).ok()
}

/// Whether `date` falls within the 7-day week `[week_start, week_start + 7 days)`.
#[must_use]
pub(crate) fn week_contains_date(week_start: LocalDate, date: LocalDate) -> bool {
    let Ok(week_end_exclusive) = week_start.checked_add(DAYS_PER_WEEK_I64.days()) else {
        return false;
    };
    date >= week_start && date < week_end_exclusive
}

/// Half-open `[start_date, end_date)` logical-date range for querying the week starting at
/// `week_start`, clamped so logical dates after `today` are never requested.
/// Returns `None` if the entire week lies after `today`.
#[must_use]
pub(crate) fn week_query_range(
    week_start: LocalDate,
    today: LocalDate,
) -> Option<(LocalDate, LocalDate)> {
    if week_start > today {
        return None;
    }
    let week_end_exclusive = week_start.checked_add(DAYS_PER_WEEK_I64.days()).ok()?;
    let today_exclusive = today.tomorrow().ok()?;
    let clamped_end = week_end_exclusive.min(today_exclusive);
    (week_start < clamped_end).then_some((week_start, clamped_end))
}

/// Builds a `query.range` request for the week starting at `week_start`, clamped to `today`.
#[must_use]
pub(crate) fn build_week_range_request(
    week_start: LocalDate,
    today: LocalDate,
) -> Option<wire::QueryRangeRequest> {
    let (start_date, end_date) = week_query_range(week_start, today)?;
    Some(wire::QueryRangeRequest {
        start_date,
        end_date,
        bucket: wire::RangeBucket::Day,
        dimensions: vec![wire::RangeDimension::Category],
    })
}

/// Clean y-axis ceiling for the 7-day stacked column chart.
#[must_use]
pub(crate) fn week_chart_scale(max_active: Duration) -> Duration {
    if max_active.is_zero() {
        return Duration::from_hours(DEFAULT_EMPTY_SCALE_HOURS);
    }
    for hours in CHART_SCALE_STEPS_HOURS {
        let candidate = Duration::from_hours(hours);
        if candidate >= max_active {
            return candidate;
        }
    }
    let whole_secs = max_active
        .as_secs()
        .saturating_add(u64::from(max_active.subsec_nanos() > 0));
    let ceil_hours = whole_secs.div_ceil(SECONDS_PER_HOUR_U64);
    let even_hours = ceil_hours.saturating_add(ceil_hours % 2);
    Duration::from_hours(even_hours.max(DEFAULT_EMPTY_SCALE_HOURS))
}

/// Failure when converting a `query.range` response into a [`WeekData`] view model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Snafu)]
pub(crate) enum BuildWeekError {
    #[snafu(display("could not compute the 7 logical dates of the week"))]
    InvalidWeekCalendar,
    #[snafu(display("range bucket reported a negative active duration"))]
    NegativeBucketDuration,
    #[snafu(display("category group reported a negative active duration"))]
    NegativeGroupDuration,
    #[snafu(display("range bucket date fell outside the requested week"))]
    UnexpectedBucketDate,
    #[snafu(display("category metadata in range response could not be mapped"))]
    InvalidCategoryDisplay,
}

/// Stable ordering key for categories observed across the week.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum WeekCategoryKey {
    Classified(wire::CategoryId),
    Unclassified,
}

/// One category appearing in the week's legend and column stacking order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WeekCategory {
    pub(crate) name: CategoryName,
    pub(crate) color: CategoryColor,
    pub(crate) total: Duration,
}

/// One non-empty category segment inside a day's stacked column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WeekDaySegment {
    /// Index into [`WeekData::categories`].
    pub(crate) category: usize,
    pub(crate) color: CategoryColor,
    pub(crate) duration: Duration,
}

/// Whether a day slot in the 7-day week is on/before today or in the future.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WeekDayKind {
    Observed,
    Future,
}

/// One day slot in the 7-day week view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WeekDay {
    pub(crate) date: LocalDate,
    pub(crate) kind: WeekDayKind,
    pub(crate) active: Duration,
    /// Stacked category segments in [`WeekData::categories`] order (bottom to top).
    pub(crate) segments: Vec<WeekDaySegment>,
    /// Focus share of active time; `None` for future days and zero-activity days.
    pub(crate) focus_share: Option<Share>,
    /// Distraction share of active time; `None` for future days and zero-activity days.
    pub(crate) distraction_share: Option<Share>,
}

/// Complete mapped data for one 7-day week.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WeekData {
    pub(crate) days: Vec<WeekDay>,
    /// Categories in legend and bottom-to-top stacking order (descending week total, tie-broken
    /// by catalog hierarchy preorder).
    pub(crate) categories: Vec<WeekCategory>,
    pub(crate) total_active: Duration,
    pub(crate) chart_scale: Duration,
    pub(crate) truncated: bool,
}

impl WeekData {
    /// Constructs an empty 7-day week with no recorded activity.
    ///
    /// # Errors
    /// Returns [`BuildWeekError::InvalidWeekCalendar`] if the 7 calendar dates overflow.
    pub(crate) fn empty(week_start: LocalDate, today: LocalDate) -> Result<Self, BuildWeekError> {
        let mut days = Vec::with_capacity(WEEK_DAY_COUNT);
        for offset in 0..DAYS_PER_WEEK_I64 {
            let date = week_start
                .checked_add(offset.days())
                .map_err(|_| BuildWeekError::InvalidWeekCalendar)?;
            let kind = if date > today {
                WeekDayKind::Future
            } else {
                WeekDayKind::Observed
            };
            days.push(WeekDay {
                date,
                kind,
                active: Duration::ZERO,
                segments: Vec::new(),
                focus_share: None,
                distraction_share: None,
            });
        }
        Ok(Self {
            days,
            categories: Vec::new(),
            total_active: Duration::ZERO,
            chart_scale: week_chart_scale(Duration::ZERO),
            truncated: false,
        })
    }

    /// Converts a `query.range` (`bucket = Day`, `dimensions = [Category]`) response and optional
    /// [`CategoryCatalog`] into the 7-day week view model.
    ///
    /// # Errors
    /// Returns [`BuildWeekError`] if durations are negative, bucket dates fall outside the week,
    /// or category display metadata is invalid.
    pub(crate) fn from_protocol(
        week_start: LocalDate,
        today: LocalDate,
        response: wire::QueryRangeResponse,
        catalog: Option<&CategoryCatalog>,
    ) -> Result<Self, BuildWeekError> {
        let week_end_exclusive = week_start
            .checked_add(DAYS_PER_WEEK_I64.days())
            .map_err(|_| BuildWeekError::InvalidWeekCalendar)?;

        let catalog_positions: BTreeMap<wire::Uuid, usize> = catalog
            .map(|catalog| {
                catalog
                    .items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| (item.uuid.clone(), index))
                    .collect()
            })
            .unwrap_or_default();

        struct ResolvedCatMeta {
            name: CategoryName,
            color: CategoryColor,
            tendency: Tendency,
            catalog_order: Option<usize>,
            total: Duration,
        }

        struct RawBucket {
            active: Duration,
            categories: BTreeMap<WeekCategoryKey, Duration>,
        }

        let mut category_meta: BTreeMap<WeekCategoryKey, ResolvedCatMeta> = BTreeMap::new();
        let mut buckets_by_date: BTreeMap<LocalDate, RawBucket> = BTreeMap::new();

        for bucket in response.buckets {
            if bucket.start_date < week_start || bucket.start_date >= week_end_exclusive {
                return Err(BuildWeekError::UnexpectedBucketDate);
            }
            let expected_end = bucket
                .start_date
                .tomorrow()
                .map_err(|_| BuildWeekError::InvalidWeekCalendar)?;
            if bucket.end_date != expected_end {
                return Err(BuildWeekError::UnexpectedBucketDate);
            }
            let active_ms = u64::try_from(bucket.active_ms)
                .map_err(|_| BuildWeekError::NegativeBucketDuration)?;
            // Days after `today` are always rendered empty even if a caller supplied buckets.
            if bucket.start_date > today {
                continue;
            }
            let active = Duration::from_millis(active_ms);
            let mut per_cat: BTreeMap<WeekCategoryKey, Duration> = BTreeMap::new();

            for group in bucket.groups {
                if group.dimension != wire::RangeDimension::Category {
                    continue;
                }
                let group_ms = u64::try_from(group.active_ms)
                    .map_err(|_| BuildWeekError::NegativeGroupDuration)?;
                let wire::RangeEntity::Category(wire::CategoryEntity::Category { category }) =
                    group.entity
                else {
                    continue;
                };
                if group_ms == 0 {
                    continue;
                }
                let duration = Duration::from_millis(group_ms);
                let (key, resolved) = resolve_category(&category, catalog, &catalog_positions)?;
                let entry = category_meta.entry(key).or_insert_with(|| ResolvedCatMeta {
                    name: resolved.name,
                    color: resolved.color,
                    tendency: resolved.tendency,
                    catalog_order: resolved.catalog_order,
                    total: Duration::ZERO,
                });
                entry.total = entry.total.saturating_add(duration);
                let day_entry = per_cat.entry(key).or_insert(Duration::ZERO);
                *day_entry = day_entry.saturating_add(duration);
            }

            buckets_by_date.insert(
                bucket.start_date,
                RawBucket {
                    active,
                    categories: per_cat,
                },
            );
        }

        let mut ordered_keys: Vec<(WeekCategoryKey, ResolvedCatMeta)> = category_meta
            .into_iter()
            .filter(|(_, meta)| !meta.total.is_zero())
            .collect();
        ordered_keys.sort_by(|(left_key, left_meta), (right_key, right_meta)| {
            right_meta
                .total
                .cmp(&left_meta.total)
                .then_with(|| {
                    (left_meta.catalog_order.is_none(), left_meta.catalog_order)
                        .cmp(&(right_meta.catalog_order.is_none(), right_meta.catalog_order))
                })
                .then_with(|| left_key.cmp(right_key))
        });

        let mut categories = Vec::with_capacity(ordered_keys.len());
        let mut key_order = Vec::with_capacity(ordered_keys.len());
        for (key, meta) in ordered_keys {
            key_order.push((key, meta.color, meta.tendency));
            categories.push(WeekCategory {
                name: meta.name,
                color: meta.color,
                total: meta.total,
            });
        }

        let mut days = Vec::with_capacity(WEEK_DAY_COUNT);
        let mut total_active = Duration::ZERO;
        let mut max_day_active = Duration::ZERO;

        for offset in 0..DAYS_PER_WEEK_I64 {
            let date = week_start
                .checked_add(offset.days())
                .map_err(|_| BuildWeekError::InvalidWeekCalendar)?;
            if date > today {
                days.push(WeekDay {
                    date,
                    kind: WeekDayKind::Future,
                    active: Duration::ZERO,
                    segments: Vec::new(),
                    focus_share: None,
                    distraction_share: None,
                });
                continue;
            }
            let Some(raw) = buckets_by_date.get(&date) else {
                days.push(WeekDay {
                    date,
                    kind: WeekDayKind::Observed,
                    active: Duration::ZERO,
                    segments: Vec::new(),
                    focus_share: None,
                    distraction_share: None,
                });
                continue;
            };
            total_active = total_active.saturating_add(raw.active);
            max_day_active = max_day_active.max(raw.active);

            if raw.active.is_zero() {
                days.push(WeekDay {
                    date,
                    kind: WeekDayKind::Observed,
                    active: Duration::ZERO,
                    segments: Vec::new(),
                    focus_share: None,
                    distraction_share: None,
                });
                continue;
            }

            let mut segments = Vec::new();
            let mut focus_total = Duration::ZERO;
            let mut distraction_total = Duration::ZERO;
            for (index, &(key, color, tendency)) in key_order.iter().enumerate() {
                let Some(&duration) = raw.categories.get(&key) else {
                    continue;
                };
                if duration.is_zero() {
                    continue;
                }
                match tendency {
                    Tendency::Focus => focus_total = focus_total.saturating_add(duration),
                    Tendency::Distraction => {
                        distraction_total = distraction_total.saturating_add(duration)
                    }
                    Tendency::Neutral => {}
                }
                segments.push(WeekDaySegment {
                    category: index,
                    color,
                    duration,
                });
            }

            let focus_share = Some(Share::of(focus_total.min(raw.active), raw.active));
            let distraction_share = Some(Share::of(distraction_total.min(raw.active), raw.active));

            days.push(WeekDay {
                date,
                kind: WeekDayKind::Observed,
                active: raw.active,
                segments,
                focus_share,
                distraction_share,
            });
        }

        Ok(Self {
            days,
            categories,
            total_active,
            chart_scale: week_chart_scale(max_day_active),
            truncated: response.truncated,
        })
    }

    /// Whether no day in this week recorded any active time.
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.total_active.is_zero()
    }
}

struct ResolvedCategoryInfo {
    name: CategoryName,
    color: CategoryColor,
    tendency: Tendency,
    catalog_order: Option<usize>,
}

fn resolve_category(
    category_ref: &wire::CategoryRef,
    catalog: Option<&CategoryCatalog>,
    catalog_positions: &BTreeMap<wire::Uuid, usize>,
) -> Result<(WeekCategoryKey, ResolvedCategoryInfo), BuildWeekError> {
    match category_ref {
        wire::CategoryRef::Unclassified => Ok((
            WeekCategoryKey::Unclassified,
            ResolvedCategoryInfo {
                name: CategoryName::Builtin(BuiltinCategory::Uncategorized),
                color: CategoryColor::Neutral,
                tendency: Tendency::Neutral,
                catalog_order: None,
            },
        )),
        wire::CategoryRef::Classified { category } => {
            let key = WeekCategoryKey::Classified(category.id);
            if let Ok(uuid) = category.uuid.0.parse::<wire::Uuid>()
                && let Some(item) = catalog.and_then(|catalog| catalog.find(&uuid))
            {
                return Ok((
                    key,
                    ResolvedCategoryInfo {
                        name: item.display_name.clone(),
                        color: item.color,
                        tendency: item.tendency,
                        catalog_order: catalog_positions.get(&uuid).copied(),
                    },
                ));
            }
            let display = CategoryDisplay::from_protocol(category_ref)
                .map_err(|_| BuildWeekError::InvalidCategoryDisplay)?;
            let tendency = match category.tendency {
                wire::CategoryTendency::Focus => Tendency::Focus,
                wire::CategoryTendency::Neutral => Tendency::Neutral,
                wire::CategoryTendency::Distraction => Tendency::Distraction,
            };
            Ok((
                key,
                ResolvedCategoryInfo {
                    name: display.name,
                    color: display.color,
                    tendency,
                    catalog_order: None,
                },
            ))
        }
    }
}

/// Monotonic generation counter for dropping late `query.range` responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub(crate) struct WeekGeneration(u64);

impl WeekGeneration {
    /// Returns the next monotonic generation.
    #[must_use]
    pub(crate) const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    /// Returns whether `incoming` matches the active generation.
    #[must_use]
    pub(crate) const fn accepts(self, incoming: Self) -> bool {
        self.0 == incoming.0
    }
}

/// Whether the Week page is currently visible on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageVisibility {
    Visible,
    Hidden,
}

/// Whether one or more invalidation events arrived while a week query was already in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum RefreshPending {
    #[default]
    Idle,
    Queued,
}

/// Whether starting a query should replace existing content with the loading skeleton or keep
/// already loaded week data visible during a background refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoadReset {
    ShowLoading,
    KeepReady,
}

/// Load state of the Week page.
#[derive(Debug)]
pub(crate) enum WeekLoadState {
    Loading,
    Ready(WeekData),
    Failed(LoadDataError),
}

/// Side effect emitted by [`WeekPage`] state transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WeekEffect {
    None,
    Query {
        request: wire::QueryRangeRequest,
        week_start: LocalDate,
        today: LocalDate,
        generation: WeekGeneration,
    },
}

/// Pure state machine for the Week page: week navigation, generation gating, progress tracking,
/// and coalesced refresh on `day` events without timers or polling.
#[derive(Debug)]
pub(crate) struct WeekPage {
    week_start: Option<LocalDate>,
    first_weekday: Option<Weekday>,
    state: WeekLoadState,
    progress: LoadProgress,
    generation: WeekGeneration,
    in_flight: Option<WeekGeneration>,
    pending_refresh: RefreshPending,
}

impl Default for WeekPage {
    fn default() -> Self {
        Self::new()
    }
}

impl WeekPage {
    /// Creates an unloaded Week page state machine.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            week_start: None,
            first_weekday: None,
            state: WeekLoadState::Loading,
            progress: LoadProgress::Waiting,
            generation: WeekGeneration::default(),
            in_flight: None,
            pending_refresh: RefreshPending::Idle,
        }
    }

    /// First logical date of the currently shown week, once locale metadata is available.
    #[must_use]
    pub(crate) const fn week_start(&self) -> Option<LocalDate> {
        self.week_start
    }

    /// Last inclusive logical date of the currently shown week.
    #[must_use]
    pub(crate) fn week_end_inclusive(&self) -> Option<LocalDate> {
        self.week_start.and_then(week_end_inclusive)
    }

    /// Whether the currently shown week contains `today` (or lies in the future / is unset).
    #[must_use]
    pub(crate) fn is_current_week(&self, today: LocalDate) -> bool {
        let Some(start) = self.week_start else {
            return true;
        };
        if week_contains_date(start, today) {
            return true;
        }
        start >= today
    }

    /// Current load state of the Week page.
    #[must_use]
    pub(crate) const fn state(&self) -> &WeekLoadState {
        &self.state
    }

    /// Per-day progress of the active `query.range` request.
    #[must_use]
    pub(crate) const fn progress(&self) -> LoadProgress {
        self.progress
    }

    /// Opens or re-opens the Week page and triggers a load for the shown week.
    #[must_use]
    pub(crate) fn open(&mut self, today: LocalDate, first_weekday: Weekday) -> WeekEffect {
        let previous_weekday = self.first_weekday;
        let was_current = self.week_start.is_none() || self.is_current_week(today);
        self.first_weekday = Some(first_weekday);
        let Some(current_start) = week_start_for(today, first_weekday) else {
            return WeekEffect::None;
        };
        let target_start = match self.week_start {
            None => current_start,
            Some(_) if was_current => current_start,
            Some(existing) if previous_weekday != Some(first_weekday) => {
                week_start_for(existing, first_weekday)
                    .map(|aligned| aligned.min(current_start))
                    .unwrap_or(current_start)
            }
            Some(existing) => existing.min(current_start),
        };
        let changed_week = self.week_start != Some(target_start);
        self.week_start = Some(target_start);
        let reset = if changed_week || !matches!(self.state, WeekLoadState::Ready(_)) {
            LoadReset::ShowLoading
        } else {
            LoadReset::KeepReady
        };
        self.issue_query(target_start, today, reset)
    }

    /// Retries or reloads the currently shown week.
    #[must_use]
    pub(crate) fn reload(&mut self, today: LocalDate) -> WeekEffect {
        let Some(start) = self.week_start else {
            return WeekEffect::None;
        };
        let reset = if matches!(self.state, WeekLoadState::Ready(_)) {
            LoadReset::KeepReady
        } else {
            LoadReset::ShowLoading
        };
        self.issue_query(start, today, reset)
    }

    /// Moves to the previous 7-day week and issues a fresh load.
    #[must_use]
    pub(crate) fn previous_week(&mut self, today: LocalDate) -> WeekEffect {
        let Some(start) = self.week_start else {
            return WeekEffect::None;
        };
        let Ok(previous) = start.checked_sub(DAYS_PER_WEEK_I64.days()) else {
            return WeekEffect::None;
        };
        self.week_start = Some(previous);
        self.issue_query(previous, today, LoadReset::ShowLoading)
    }

    /// Moves to the next 7-day week unless the shown week already contains `today`.
    #[must_use]
    pub(crate) fn next_week(&mut self, today: LocalDate) -> WeekEffect {
        if self.is_current_week(today) {
            return WeekEffect::None;
        }
        let Some(start) = self.week_start else {
            return WeekEffect::None;
        };
        let Some(first_weekday) = self.first_weekday else {
            return WeekEffect::None;
        };
        let Some(current_start) = week_start_for(today, first_weekday) else {
            return WeekEffect::None;
        };
        let Ok(next) = start.checked_add(DAYS_PER_WEEK_I64.days()) else {
            return WeekEffect::None;
        };
        let target = next.min(current_start);
        if target == start {
            return WeekEffect::None;
        }
        self.week_start = Some(target);
        self.issue_query(target, today, LoadReset::ShowLoading)
    }

    /// Jumps to the week containing `today` unless already viewing it.
    #[must_use]
    pub(crate) fn go_this_week(&mut self, today: LocalDate) -> WeekEffect {
        if self.is_current_week(today) {
            return WeekEffect::None;
        }
        let Some(first_weekday) = self.first_weekday else {
            return WeekEffect::None;
        };
        let Some(current_start) = week_start_for(today, first_weekday) else {
            return WeekEffect::None;
        };
        if self.week_start == Some(current_start) {
            return WeekEffect::None;
        }
        self.week_start = Some(current_start);
        self.issue_query(current_start, today, LoadReset::ShowLoading)
    }

    /// Reconciles shown week boundaries when `StateSnapshot` updates `logical_date` or locale.
    #[must_use]
    pub(crate) fn on_snapshot_changed(
        &mut self,
        previous_today: LocalDate,
        new_today: LocalDate,
        first_weekday: Option<Weekday>,
        visibility: PageVisibility,
    ) -> WeekEffect {
        let Some(first_weekday) = first_weekday else {
            self.first_weekday = None;
            self.week_start = None;
            self.in_flight = None;
            self.pending_refresh = RefreshPending::Idle;
            self.progress = LoadProgress::Waiting;
            self.generation = self.generation.next();
            self.state = WeekLoadState::Loading;
            return WeekEffect::None;
        };

        let locale_changed = self.first_weekday != Some(first_weekday);
        let date_changed = previous_today != new_today;
        if !locale_changed && !date_changed {
            return WeekEffect::None;
        }

        let was_on_previous_current = self
            .week_start
            .is_none_or(|start| week_contains_date(start, previous_today));
        self.first_weekday = Some(first_weekday);
        let Some(current_start) = week_start_for(new_today, first_weekday) else {
            return WeekEffect::None;
        };
        let new_start = match self.week_start {
            None => current_start,
            Some(_) if was_on_previous_current => current_start,
            Some(existing) if locale_changed => week_start_for(existing, first_weekday)
                .map(|aligned| aligned.min(current_start))
                .unwrap_or(current_start),
            Some(existing) => existing.min(current_start),
        };
        let week_moved = self.week_start != Some(new_start);
        self.week_start = Some(new_start);

        if visibility == PageVisibility::Hidden {
            if week_moved {
                self.state = WeekLoadState::Loading;
            }
            return WeekEffect::None;
        }
        if week_moved {
            self.issue_query(new_start, new_today, LoadReset::ShowLoading)
        } else {
            self.request_coalesced_refresh(new_start, new_today)
        }
    }

    /// Reacts to a `day` version event: ignores dates outside the shown week, ignores when
    /// hidden (the page reloads on open), and coalesces bursts during an in-flight load into a
    /// single follow-up query.
    #[must_use]
    pub(crate) fn on_day_changed(
        &mut self,
        changed_date: LocalDate,
        today: LocalDate,
        visibility: PageVisibility,
    ) -> WeekEffect {
        let Some(start) = self.week_start else {
            return WeekEffect::None;
        };
        if !week_contains_date(start, changed_date) {
            return WeekEffect::None;
        }
        if visibility == PageVisibility::Hidden {
            return WeekEffect::None;
        }
        self.request_coalesced_refresh(start, today)
    }

    /// Reacts to category or rule mutations that affect classification across days.
    #[must_use]
    pub(crate) fn on_rules_mutated(
        &mut self,
        today: LocalDate,
        visibility: PageVisibility,
    ) -> WeekEffect {
        let Some(start) = self.week_start else {
            return WeekEffect::None;
        };
        if visibility == PageVisibility::Hidden {
            return WeekEffect::None;
        }
        self.request_coalesced_refresh(start, today)
    }

    /// Records per-day progress for an in-flight `query.range` request, dropping stale updates.
    #[must_use]
    pub(crate) fn apply_progress(
        &mut self,
        generation: WeekGeneration,
        progress: QueryProgress,
    ) -> ProgressUpdate {
        if !self.generation.accepts(generation) || self.in_flight != Some(generation) {
            return ProgressUpdate::Ignored;
        }
        self.progress.record(progress);
        ProgressUpdate::Applied
    }

    /// Applies the outcome of `query.range`, dropping stale generations and dispatching any
    /// coalesced follow-up refresh that arrived while the query was in flight.
    #[must_use]
    pub(crate) fn apply_loaded(
        &mut self,
        generation: WeekGeneration,
        week_start: LocalDate,
        today: LocalDate,
        result: Result<wire::QueryRangeResponse, LoadDataError>,
        catalog: Option<&CategoryCatalog>,
    ) -> WeekEffect {
        if !self.generation.accepts(generation) {
            return WeekEffect::None;
        }
        self.in_flight = None;
        self.progress = LoadProgress::Waiting;
        match result {
            Ok(response) => match WeekData::from_protocol(week_start, today, response, catalog) {
                Ok(data) => {
                    self.state = WeekLoadState::Ready(data);
                }
                Err(_) => {
                    self.state = WeekLoadState::Failed(LoadDataError::InvalidData);
                }
            },
            Err(error) => {
                self.state = WeekLoadState::Failed(error);
            }
        }
        if self.pending_refresh == RefreshPending::Queued {
            self.pending_refresh = RefreshPending::Idle;
            if let Some(current_start) = self.week_start {
                return self.issue_query(current_start, today, LoadReset::KeepReady);
            }
        }
        WeekEffect::None
    }

    fn request_coalesced_refresh(&mut self, week_start: LocalDate, today: LocalDate) -> WeekEffect {
        if self.in_flight.is_some() {
            self.pending_refresh = RefreshPending::Queued;
            return WeekEffect::None;
        }
        let reset = if matches!(self.state, WeekLoadState::Ready(_)) {
            LoadReset::KeepReady
        } else {
            LoadReset::ShowLoading
        };
        self.issue_query(week_start, today, reset)
    }

    fn issue_query(
        &mut self,
        week_start: LocalDate,
        today: LocalDate,
        reset: LoadReset,
    ) -> WeekEffect {
        self.generation = self.generation.next();
        self.pending_refresh = RefreshPending::Idle;
        self.progress = LoadProgress::Waiting;
        let Some(request) = build_week_range_request(week_start, today) else {
            self.in_flight = None;
            match WeekData::empty(week_start, today) {
                Ok(empty) => {
                    self.state = WeekLoadState::Ready(empty);
                }
                Err(_) => {
                    self.state = WeekLoadState::Failed(LoadDataError::InvalidData);
                }
            }
            return WeekEffect::None;
        };
        if reset == LoadReset::ShowLoading {
            self.state = WeekLoadState::Loading;
        }
        self.in_flight = Some(self.generation);
        WeekEffect::Query {
            request,
            week_start,
            today,
            generation: self.generation,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PaletteSlot;

    const DEV_UUID: &str = "11111111-1111-4111-8111-111111111111";
    const LEARN_UUID: &str = "22222222-2222-4222-8222-222222222222";
    const ENTERTAIN_UUID: &str = "44444444-4444-4444-8444-444444444444";
    const ARCHIVED_UUID: &str = "77777777-7777-4777-8777-777777777777";

    fn date(text: &str) -> LocalDate {
        text.parse().expect("valid test date")
    }

    fn cat_ref(
        id: i64,
        uuid: &str,
        builtin: Option<wire::BuiltinCategory>,
        name: Option<&str>,
        color: &str,
        tendency: wire::CategoryTendency,
    ) -> wire::CategoryRef {
        wire::CategoryRef::Classified {
            category: wire::Category {
                id: wire::CategoryId(id),
                uuid: wire::CategoryUuid(uuid.to_owned()),
                builtin_key: builtin,
                name: name.map(str::to_owned),
                color: color.to_owned(),
                tendency,
            }
            .into(),
        }
    }

    fn cat_group(category: wire::CategoryRef, active_ms: i64) -> wire::RangeGroup {
        wire::RangeGroup {
            dimension: wire::RangeDimension::Category,
            entity: wire::RangeEntity::Category(wire::CategoryEntity::Category { category }),
            active_ms,
        }
    }

    fn day_bucket(
        start: LocalDate,
        active_ms: i64,
        groups: Vec<wire::RangeGroup>,
    ) -> wire::RangeSummaryBucket {
        wire::RangeSummaryBucket {
            start_date: start,
            end_date: start.tomorrow().expect("next day"),
            active_ms,
            groups,
        }
    }

    fn range_response(buckets: Vec<wire::RangeSummaryBucket>) -> wire::QueryRangeResponse {
        wire::QueryRangeResponse {
            time_zone: "Asia/Shanghai".to_owned(),
            day_start_offset_min: 240,
            reference_range: None,
            buckets,
            versions: wire::QueryVersions("v1".to_owned()),
            truncated: false,
        }
    }

    fn sample_catalog() -> CategoryCatalog {
        let entity = |uuid: &str,
                      builtin: Option<wire::BuiltinCategory>,
                      name: Option<&str>,
                      color: &str,
                      tendency: wire::CategoryTendency,
                      archived: bool,
                      sort_order: i64| wire::EditableCategory {
            uuid: uuid.parse().expect("uuid"),
            parent_uuid: None,
            builtin_key: builtin,
            name: name.map(str::to_owned),
            color: color.to_owned(),
            tendency,
            archived,
            sort_order,
        };
        CategoryCatalog::from_protocol(wire::CategoriesListResponse {
            entities: vec![
                entity(
                    LEARN_UUID,
                    Some(wire::BuiltinCategory::Learning),
                    None,
                    "cat.1",
                    wire::CategoryTendency::Focus,
                    false,
                    1,
                ),
                entity(
                    DEV_UUID,
                    Some(wire::BuiltinCategory::Development),
                    None,
                    "cat.8",
                    wire::CategoryTendency::Focus,
                    false,
                    2,
                ),
                entity(
                    ENTERTAIN_UUID,
                    Some(wire::BuiltinCategory::Entertainment),
                    None,
                    "cat.13",
                    wire::CategoryTendency::Distraction,
                    false,
                    3,
                ),
                entity(
                    ARCHIVED_UUID,
                    None,
                    Some("Legacy"),
                    "cat.5",
                    wire::CategoryTendency::Focus,
                    true,
                    4,
                ),
            ],
            versions: wire::QueryVersions("cats-v1".to_owned()),
            truncated: false,
        })
        .expect("sample catalog")
    }

    #[test]
    fn stacking_order_uses_week_total_then_catalog_hierarchy_and_keeps_archived_history() {
        let week_start = date("2026-11-16");
        let today = date("2026-11-22");
        let catalog = sample_catalog();

        let dev = cat_ref(
            1,
            DEV_UUID,
            Some(wire::BuiltinCategory::Development),
            None,
            "cat.8",
            wire::CategoryTendency::Focus,
        );
        let learn = cat_ref(
            2,
            LEARN_UUID,
            Some(wire::BuiltinCategory::Learning),
            None,
            "cat.1",
            wire::CategoryTendency::Focus,
        );
        let entertain = cat_ref(
            3,
            ENTERTAIN_UUID,
            Some(wire::BuiltinCategory::Entertainment),
            None,
            "cat.13",
            wire::CategoryTendency::Distraction,
        );
        let archived = cat_ref(
            4,
            ARCHIVED_UUID,
            None,
            Some("Legacy"),
            "cat.5",
            wire::CategoryTendency::Focus,
        );

        // Learn and Dev tie on total (3_600_000 ms each): catalog preorder puts Learn before Dev.
        // Entertain has the largest total (5_400_000 ms), so it sits first (bottom of stack).
        // Archived category retains historical duration and color.
        let response = range_response(vec![
            day_bucket(
                date("2026-11-16"),
                9_900_000,
                vec![
                    cat_group(dev.clone(), 3_600_000),
                    cat_group(entertain.clone(), 5_400_000),
                    cat_group(archived.clone(), 900_000),
                ],
            ),
            day_bucket(
                date("2026-11-17"),
                4_200_000,
                vec![
                    cat_group(learn.clone(), 3_600_000),
                    cat_group(wire::CategoryRef::Unclassified, 600_000),
                ],
            ),
        ]);

        let week =
            WeekData::from_protocol(week_start, today, response, Some(&catalog)).expect("week");
        let names: Vec<&CategoryName> = week.categories.iter().map(|cat| &cat.name).collect();
        assert_eq!(
            names,
            vec![
                &CategoryName::Builtin(BuiltinCategory::Entertainment),
                &CategoryName::Builtin(BuiltinCategory::Learning),
                &CategoryName::Builtin(BuiltinCategory::Development),
                &CategoryName::Custom("Legacy".to_owned()),
                &CategoryName::Builtin(BuiltinCategory::Uncategorized),
            ]
        );
        assert_eq!(
            week.categories.get(3).expect("archived category").color,
            CategoryColor::Palette(PaletteSlot::numbered(5))
        );

        // Day 0 segments follow the same week category order: Entertain (0), Dev (2), Archived (3).
        let day0_indices: Vec<usize> = week
            .days
            .first()
            .expect("monday")
            .segments
            .iter()
            .map(|seg| seg.category)
            .collect();
        assert_eq!(day0_indices, vec![0, 2, 3]);
        assert_eq!(week.total_active, Duration::from_millis(14_100_000));
        assert!(!week.is_empty());
    }

    #[test]
    fn share_computation_omits_zero_activity_and_future_days_while_keeping_zero_percent_active_day()
    {
        let week_start = date("2026-11-16");
        let today = date("2026-11-20");

        let dev = cat_ref(
            1,
            DEV_UUID,
            Some(wire::BuiltinCategory::Development),
            None,
            "cat.8",
            wire::CategoryTendency::Focus,
        );
        let entertain = cat_ref(
            3,
            ENTERTAIN_UUID,
            Some(wire::BuiltinCategory::Entertainment),
            None,
            "cat.13",
            wire::CategoryTendency::Distraction,
        );

        let response = range_response(vec![
            // Active day with 60% focus, 25% distraction, 15% unclassified.
            day_bucket(
                date("2026-11-16"),
                10_000_000,
                vec![
                    cat_group(dev, 6_000_000),
                    cat_group(entertain, 2_500_000),
                    cat_group(wire::CategoryRef::Unclassified, 1_500_000),
                ],
            ),
            // Zero-activity day must have NO points (None), not 0%.
            day_bucket(date("2026-11-17"), 0, vec![]),
            // Active day with only unclassified (neutral) time has 0% points for both shares.
            day_bucket(
                date("2026-11-18"),
                1_800_000,
                vec![cat_group(wire::CategoryRef::Unclassified, 1_800_000)],
            ),
        ]);

        let week = WeekData::from_protocol(week_start, today, response, None).expect("week");
        assert_eq!(week.days.len(), WEEK_DAY_COUNT);

        let mon = week.days.first().expect("monday");
        assert_eq!(mon.kind, WeekDayKind::Observed);
        assert_eq!(mon.focus_share.map(Share::percent), Some(60));
        assert_eq!(mon.distraction_share.map(Share::percent), Some(25));

        let tue = week.days.get(1).expect("tuesday");
        assert_eq!(tue.kind, WeekDayKind::Observed);
        assert_eq!(tue.active, Duration::ZERO);
        assert_eq!(tue.focus_share, None);
        assert_eq!(tue.distraction_share, None);

        let wed = week.days.get(2).expect("wednesday");
        assert_eq!(wed.focus_share.map(Share::percent), Some(0));
        assert_eq!(wed.distraction_share.map(Share::percent), Some(0));

        // Saturday and Sunday are after `today` (Friday) -> Future and empty.
        let sat = week.days.get(5).expect("saturday");
        let sun = week.days.get(6).expect("sunday");
        assert_eq!(sat.kind, WeekDayKind::Future);
        assert_eq!(sat.focus_share, None);
        assert_eq!(sun.kind, WeekDayKind::Future);
        assert_eq!(sun.distraction_share, None);
    }

    #[test]
    fn non_monday_first_weekday_anchors_week_start_and_clamps_future_request_end() {
        // 2026-11-26 is a Thursday.
        let thursday = date("2026-11-26");
        assert_eq!(
            week_start_for(thursday, Weekday::Sunday),
            Some(date("2026-11-22"))
        );
        assert_eq!(
            week_start_for(thursday, Weekday::Wednesday),
            Some(date("2026-11-25"))
        );
        assert_eq!(
            week_start_for(thursday, Weekday::Saturday),
            Some(date("2026-11-21"))
        );
        assert_eq!(
            weekday_from_protocol(wire::Weekday::Sunday),
            Weekday::Sunday
        );

        // Current Sunday-start week (Nov 22..Nov 28) clamped to today (Nov 26) -> [Nov 22, Nov 27).
        let clamped = build_week_range_request(date("2026-11-22"), thursday).expect("clamped");
        assert_eq!(clamped.start_date, date("2026-11-22"));
        assert_eq!(clamped.end_date, date("2026-11-27"));
        assert_eq!(clamped.bucket, wire::RangeBucket::Day);
        assert_eq!(clamped.dimensions, vec![wire::RangeDimension::Category]);

        // Past week (Nov 15..Nov 21) is unclamped -> full 7-day half-open [Nov 15, Nov 22).
        let past = build_week_range_request(date("2026-11-15"), thursday).expect("past");
        assert_eq!(past.start_date, date("2026-11-15"));
        assert_eq!(past.end_date, date("2026-11-22"));

        // Entirely future week produces no request.
        assert_eq!(build_week_range_request(date("2026-11-29"), thursday), None);
    }

    #[test]
    fn day_event_refreshes_only_inside_shown_week_and_coalesces_in_flight_bursts() {
        let today = date("2026-11-26");
        let mut page = WeekPage::new();

        // Open current week (Sunday-start: Nov 22..Nov 28).
        let WeekEffect::Query {
            request,
            week_start,
            generation: gen1,
            ..
        } = page.open(today, Weekday::Sunday)
        else {
            panic!("opening Week page queries immediately");
        };
        assert_eq!(week_start, date("2026-11-22"));
        assert_eq!(request.start_date, date("2026-11-22"));
        assert_eq!(request.end_date, date("2026-11-27"));
        assert!(page.is_current_week(today));
        assert_eq!(page.week_end_inclusive(), Some(date("2026-11-28")));

        // Progress updates apply only for the active generation.
        assert_eq!(
            page.apply_progress(gen1, QueryProgress { done: 2, total: 5 }),
            ProgressUpdate::Applied
        );
        assert_eq!(
            page.progress(),
            LoadProgress::Counting { done: 2, total: 5 }
        );

        // Day event OUTSIDE shown week is ignored while in flight.
        assert_eq!(
            page.on_day_changed(date("2026-11-21"), today, PageVisibility::Visible),
            WeekEffect::None
        );

        // Burst of 3 day events INSIDE shown week while in flight coalesces into ONE follow-up.
        for day_in_week in ["2026-11-22", "2026-11-24", "2026-11-26"] {
            assert_eq!(
                page.on_day_changed(date(day_in_week), today, PageVisibility::Visible),
                WeekEffect::None
            );
        }

        // Finishing gen1 immediately dispatches the single coalesced follow-up load (gen2).
        let WeekEffect::Query {
            generation: gen2, ..
        } = page.apply_loaded(gen1, week_start, today, Ok(range_response(vec![])), None)
        else {
            panic!("coalesced follow-up load should run after in-flight load completes");
        };
        assert_ne!(gen1, gen2);
        assert_eq!(
            page.apply_progress(gen1, QueryProgress { done: 5, total: 5 }),
            ProgressUpdate::Ignored
        );

        // Finishing gen2 with no further queued events settles to idle.
        assert_eq!(
            page.apply_loaded(gen2, week_start, today, Ok(range_response(vec![])), None),
            WeekEffect::None
        );

        // When idle, a day event OUTSIDE the shown week does nothing; INSIDE triggers a reload.
        assert_eq!(
            page.on_day_changed(date("2026-11-29"), today, PageVisibility::Visible),
            WeekEffect::None
        );
        assert!(matches!(
            page.on_day_changed(date("2026-11-25"), today, PageVisibility::Visible),
            WeekEffect::Query { .. }
        ));

        // Navigation to previous week bumps generation and drops late results from old week.
        assert_eq!(page.next_week(today), WeekEffect::None);
        assert_eq!(page.go_this_week(today), WeekEffect::None);
        let WeekEffect::Query {
            week_start: prev_start,
            generation: prev_gen,
            ..
        } = page.previous_week(today)
        else {
            panic!("previous_week queries previous 7-day range");
        };
        assert_eq!(prev_start, date("2026-11-15"));
        assert!(!page.is_current_week(today));
        assert_eq!(
            page.apply_loaded(gen2, week_start, today, Ok(range_response(vec![])), None),
            WeekEffect::None
        );
        assert!(matches!(page.state(), WeekLoadState::Loading));
        assert_eq!(
            page.apply_loaded(
                prev_gen,
                prev_start,
                today,
                Ok(range_response(vec![])),
                None
            ),
            WeekEffect::None
        );
        assert!(matches!(page.state(), WeekLoadState::Ready(_)));
        assert!(matches!(page.next_week(today), WeekEffect::Query { .. }));
        assert!(matches!(
            page.previous_week(today),
            WeekEffect::Query { .. }
        ));
        assert!(matches!(page.go_this_week(today), WeekEffect::Query { .. }));
        assert!(matches!(page.reload(today), WeekEffect::Query { .. }));
        assert_eq!(
            page.on_rules_mutated(today, PageVisibility::Hidden),
            WeekEffect::None
        );
        assert_eq!(
            page.on_snapshot_changed(today, today, Some(Weekday::Monday), PageVisibility::Hidden),
            WeekEffect::None
        );
        assert_eq!(page.week_start(), Some(date("2026-11-23")));
    }

    #[test]
    fn formatter_formats_week_range_and_month_day_in_chinese_and_english() {
        use crate::format::Formatter;
        use quartz_i18n::Language;

        let zh = Formatter::new(Language::SimplifiedChinese);
        let en = Formatter::new(Language::English);
        assert_eq!(zh.month_day(date("2026-11-16")), "11月16日");
        assert_eq!(en.month_day(date("2026-11-16")), "Nov 16");
        assert_eq!(
            zh.week_range(date("2026-11-16"), date("2026-11-22")),
            "11月16日 – 11月22日"
        );
        assert_eq!(
            en.week_range(date("2026-11-16"), date("2026-11-22")),
            "Nov 16 – Nov 22"
        );
    }
}
