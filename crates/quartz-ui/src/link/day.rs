//! Version-bound day loading on the pipe runtime; GPUI only sends intents and receives results.
use super::RequestCoreError;
#[cfg(windows)]
use super::{BRIDGE_CAPACITY, Link, LinkEvent};
use quartz_pipe::CallCoreError;
use quartz_protocol::{
    ApplicationRank, DayMetrics, DocumentRank, ErrorKind, HourStack, LocalDate,
    MillisecondsPerPixel, PendingAway, ReferenceRange, TimelineSegment, WebsiteRank,
};
#[cfg(windows)]
use quartz_protocol::{
    QueryDayRequest, QueryPendingAwayRequest, QueryTimelineRequest, QueryVersions, TimelineDetail,
};
use std::num::NonZeroU32;
#[cfg(windows)]
use std::{future::Future, pin::Pin, time::Duration};
#[cfg(windows)]
use tokio::{
    sync::{mpsc, watch},
    time::Instant,
};

/// Entire query sets may restart twice after version invalidation.
#[cfg(windows)]
pub const MAX_STALE_RESTARTS: usize = 2;
/// A settled timeline view waits this long before querying again.
#[cfg(windows)]
pub const ZOOM_SETTLE_DELAY: Duration = Duration::from_millis(180);
/// Fast refreshes leave the date bar unchanged.
#[cfg(windows)]
pub const REFRESH_INDICATOR_DELAY: Duration = Duration::from_millis(300);
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct DayGeneration(u64);
impl DayGeneration {
    pub fn accepts(self, generation: Self) -> bool {
        self == generation
    }
    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UtcRange {
    pub start: i64,
    pub end: i64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DayView {
    WholeDay,
    Range(UtcRange),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DayRequest {
    pub date: LocalDate,
    pub view: DayView,
    pub physical_width: NonZeroU32,
}
impl DayRequest {
    pub fn ms_per_pixel(&self, range: UtcRange) -> Result<MillisecondsPerPixel, LoadDataError> {
        let span = u64::try_from(
            range
                .end
                .checked_sub(range.start)
                .ok_or(LoadDataError::InvalidData)?,
        )
        .map_err(|_| LoadDataError::InvalidData)?;
        let value = span.div_ceil(u64::from(self.physical_width.get()));
        Ok(MillisecondsPerPixel(
            std::num::NonZeroU64::new(value).ok_or(LoadDataError::InvalidData)?,
        ))
    }
    pub fn physical_width(logical: f32, scale: f32) -> Result<NonZeroU32, LoadDataError> {
        use num_traits::ToPrimitive as _;
        if !logical.is_finite() || !scale.is_finite() || logical <= 0. || scale <= 0. {
            return Err(LoadDataError::InvalidData);
        }
        (logical * scale)
            .round()
            .to_u32()
            .and_then(NonZeroU32::new)
            .ok_or(LoadDataError::InvalidData)
    }
}
#[derive(Debug, snafu::Snafu)]
pub enum LoadDataError {
    #[snafu(display("could not request view data"))]
    Request { source: RequestCoreError },
    #[snafu(display("could not decode valid view data"))]
    InvalidData,
    #[snafu(display("view data changed while loading"))]
    Stale,
    #[snafu(display("could not load a stable data version after retrying"))]
    StaleExhausted,
}
/// Local failures have a separate namespace from core protocol errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalFailureCode {
    Timeout,
    Connection,
    InvalidResponse,
    StaleExhausted,
}
impl LocalFailureCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Connection => "connection_error",
            Self::InvalidResponse => "invalid_response",
            Self::StaleExhausted => "stale_exhausted",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadDataErrorCode {
    Protocol(ErrorKind),
    Local(LocalFailureCode),
}
impl LoadDataErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Protocol(kind) => kind.code(),
            Self::Local(code) => code.as_str(),
        }
    }
}
impl std::fmt::Display for LoadDataErrorCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}
impl LoadDataError {
    pub fn is_busy(&self) -> bool {
        matches!(self.code(), LoadDataErrorCode::Protocol(ErrorKind::Busy))
    }
    pub fn code(&self) -> LoadDataErrorCode {
        match self {
            Self::Request {
                source:
                    RequestCoreError::Call {
                        source: CallCoreError::Protocol { kind },
                    },
            } => LoadDataErrorCode::Protocol(kind.clone()),
            Self::Request {
                source: RequestCoreError::Busy,
            } => LoadDataErrorCode::Protocol(ErrorKind::Busy),
            Self::Request {
                source: RequestCoreError::Timeout,
            } => LoadDataErrorCode::Local(LocalFailureCode::Timeout),
            Self::Request { .. } => LoadDataErrorCode::Local(LocalFailureCode::Connection),
            Self::InvalidData => LoadDataErrorCode::Local(LocalFailureCode::InvalidResponse),
            Self::Stale | Self::StaleExhausted => {
                LoadDataErrorCode::Local(LocalFailureCode::StaleExhausted)
            }
        }
    }
    #[cfg(windows)]
    pub(super) fn is_stale(&self) -> bool {
        matches!(self, Self::Stale)
    }
}
impl From<RequestCoreError> for LoadDataError {
    fn from(source: RequestCoreError) -> Self {
        Self::Request { source }
    }
}
#[derive(Debug, Clone, PartialEq)]
pub struct DayData {
    pub date: LocalDate,
    pub bounds: UtcRange,
    pub visible: UtcRange,
    pub reference_range: Option<ReferenceRange>,
    pub metrics: DayMetrics,
    pub hours: Vec<HourStack>,
    pub applications: Vec<ApplicationRank>,
    pub documents: Vec<DocumentRank>,
    pub websites: Vec<WebsiteRank>,
    pub truncated: bool,
    pub categories: Vec<TimelineSegment>,
    pub raw: Vec<TimelineSegment>,
    pub pending: Vec<PendingAway>,
}
#[derive(Clone)]
#[cfg(windows)]
struct DayCache {
    data: DayData,
    day_versions: QueryVersions,
    aggregate_versions: QueryVersions,
    reference_range: Option<ReferenceRange>,
}
#[cfg(windows)]
impl DayCache {
    fn aggregates(&self) -> Option<Self> {
        if self.reference_range.is_some() {
            return None;
        }
        Some(Self {
            day_versions: self.day_versions.clone(),
            aggregate_versions: self.aggregate_versions.clone(),
            reference_range: self.reference_range,
            data: DayData {
                date: self.data.date,
                bounds: self.data.bounds,
                visible: self.data.visible,
                reference_range: self.data.reference_range,
                metrics: self.data.metrics.clone(),
                hours: self.data.hours.clone(),
                applications: self.data.applications.clone(),
                documents: self.data.documents.clone(),
                websites: self.data.websites.clone(),
                truncated: self.data.truncated,
                pending: self.data.pending.clone(),
                categories: Vec::new(),
                raw: Vec::new(),
            },
        })
    }
}
#[cfg(windows)]
impl Link {
    async fn query_day_once(&self, request: DayRequest) -> Result<DayCache, LoadDataError> {
        let day = self
            .request(QueryDayRequest {
                local_date: request.date,
            })
            .await?;
        let reference_range = day.reference_range;
        let bounds = UtcRange {
            start: day
                .hours
                .first()
                .ok_or(LoadDataError::InvalidData)?
                .start_utc_ms,
            end: day
                .hours
                .last()
                .ok_or(LoadDataError::InvalidData)?
                .end_utc_ms,
        };
        let visible = match request.view {
            DayView::WholeDay => bounds,
            DayView::Range(range) => range,
        };
        if visible.start < bounds.start || visible.end > bounds.end {
            return Err(LoadDataError::InvalidData);
        }
        let resolution = request.ms_per_pixel(visible)?;
        let categories = self
            .request(QueryTimelineRequest {
                start_utc_ms: visible.start,
                end_utc_ms: visible.end,
                detail: TimelineDetail::Category,
                ms_per_pixel: resolution,
            })
            .await?;
        let raw = self
            .request(QueryTimelineRequest {
                start_utc_ms: visible.start,
                end_utc_ms: visible.end,
                detail: TimelineDetail::Raw,
                ms_per_pixel: resolution,
            })
            .await?;
        if categories.versions != raw.versions {
            return Err(Self::stale_day());
        }
        let pending = self
            .request(QueryPendingAwayRequest {
                local_date: request.date,
            })
            .await?;
        if (reference_range.is_none() && pending.versions != day.versions)
            || (visible == bounds && pending.versions != categories.versions)
        {
            return Err(Self::stale_day());
        }
        if reference_range.is_some() {
            let latest = self
                .request(QueryDayRequest {
                    local_date: request.date,
                })
                .await?;
            if latest.versions != day.versions {
                return Err(Self::stale_day());
            }
        }
        Ok(DayCache {
            day_versions: pending.versions,
            aggregate_versions: day.versions,
            reference_range,
            data: DayData {
                date: request.date,
                bounds,
                visible,
                reference_range,
                metrics: day.metrics,
                hours: day.hours,
                applications: day.applications,
                documents: day.documents,
                websites: day.websites,
                truncated: day.truncated,
                categories: categories.segments,
                raw: raw.segments,
                pending: pending.ranges,
            },
        })
    }
    pub(super) fn stale_day() -> LoadDataError {
        LoadDataError::Stale
    }
    async fn validate_cached_day(&self, cached: &DayCache) -> Result<(), LoadDataError> {
        let latest = self
            .request(QueryDayRequest {
                local_date: cached.data.date,
            })
            .await?;
        if latest.reference_range.is_some() || latest.versions != cached.aggregate_versions {
            return Err(Self::stale_day());
        }
        Ok(())
    }
    async fn refine_day(
        &self,
        request: DayRequest,
        mut cached: DayCache,
    ) -> Result<DayCache, LoadDataError> {
        self.validate_cached_day(&cached).await?;
        let visible = match request.view {
            DayView::WholeDay => cached.data.bounds,
            DayView::Range(range) => range,
        };
        if visible.start < cached.data.bounds.start || visible.end > cached.data.bounds.end {
            return Err(LoadDataError::InvalidData);
        }
        let resolution = request.ms_per_pixel(visible)?;
        let categories = self
            .request(QueryTimelineRequest {
                start_utc_ms: visible.start,
                end_utc_ms: visible.end,
                detail: TimelineDetail::Category,
                ms_per_pixel: resolution,
            })
            .await?;
        let raw = self
            .request(QueryTimelineRequest {
                start_utc_ms: visible.start,
                end_utc_ms: visible.end,
                detail: TimelineDetail::Raw,
                ms_per_pixel: resolution,
            })
            .await?;
        if categories.versions != raw.versions
            || (visible == cached.data.bounds && categories.versions != cached.day_versions)
        {
            return Err(Self::stale_day());
        }
        self.validate_cached_day(&cached).await?;
        cached.data.visible = visible;
        cached.data.categories = categories.segments;
        cached.data.raw = raw.segments;
        Ok(cached)
    }
    async fn query_day(
        &self,
        request: DayRequest,
        mut cached: Option<DayCache>,
    ) -> Result<DayCache, LoadDataError> {
        for attempt in 0..=MAX_STALE_RESTARTS {
            let result = match cached
                .take()
                .filter(|cached| cached.data.date == request.date)
            {
                Some(cached) => self.refine_day(request, cached).await,
                None => self.query_day_once(request).await,
            };
            match result {
                Err(error) if error.is_stale() && attempt < MAX_STALE_RESTARTS => continue,
                Err(error) if error.is_stale() => return Err(LoadDataError::StaleExhausted),
                result => return result,
            }
        }
        Err(LoadDataError::StaleExhausted)
    }
}
#[derive(Debug, Clone, Copy)]
pub enum DayLoad {
    Immediate,
    Settled,
}
#[cfg(windows)]
pub(super) enum DayCommand {
    Load {
        request: DayRequest,
        generation: DayGeneration,
        mode: DayLoad,
    },
}
#[cfg(windows)]
pub(super) struct DayLoader {
    sender: mpsc::Sender<DayCommand>,
    changed: watch::Sender<Option<LocalDate>>,
    date: Option<LocalDate>,
    task: tokio::task::JoinHandle<()>,
}
#[cfg(windows)]
impl Drop for DayLoader {
    fn drop(&mut self) {
        self.task.abort();
    }
}
#[cfg(windows)]
impl DayLoader {
    pub fn start(link: Link, events: mpsc::Sender<LinkEvent>) -> Self {
        let (sender, receiver) = mpsc::channel(BRIDGE_CAPACITY);
        let (changed, changes) = watch::channel(None);
        let task = tokio::spawn(Self::run(link, events, receiver, changes));
        Self {
            sender,
            changed,
            date: None,
            task,
        }
    }
    pub fn send(&mut self, command: DayCommand) -> Result<(), RequestCoreError> {
        let DayCommand::Load { request, .. } = &command;
        let date = request.date;
        // The pipe reader must keep servicing responses while subscription changes await them.
        self.sender.try_send(command).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => RequestCoreError::Busy,
            mpsc::error::TrySendError::Closed(_) => RequestCoreError::Closed,
        })?;
        self.date = Some(date);
        Ok(())
    }
    pub fn invalidate(&self) {
        if let Some(date) = self.date {
            self.changed(date);
        }
    }
    pub fn changed(&self, date: LocalDate) {
        if self.date == Some(date) {
            // A watch value retains one invalidation regardless of the event burst size.
            self.changed.send_replace(Some(date));
        }
    }
    async fn run(
        link: Link,
        events: mpsc::Sender<LinkEvent>,
        mut commands: mpsc::Receiver<DayCommand>,
        mut changes: watch::Receiver<Option<LocalDate>>,
    ) {
        type LoadFuture = Pin<Box<dyn Future<Output = Result<DayCache, LoadDataError>> + Send>>;
        let mut active: Option<LoadFuture> = None;
        let mut cached: Option<DayCache> = None;
        let mut revision = 0_u64;
        let mut loading_revision = revision;
        let mut latest: Option<DayRequest> = None;
        let mut generation = DayGeneration::default();
        let mut loading_generation = generation;
        let mut queued = None;
        let mut deadline: Option<Instant> = None;
        loop {
            tokio::select! {
                command = commands.recv() => {
                    match command {
                        None => break,
                        Some(DayCommand::Load { request, generation: next, mode }) => {
                            if next < generation {
                                continue;
                            }
                            if next > generation {
                                // Dropping the request future detaches its reply receiver; the pipe
                                // worker still consumes late responses by their request identities.
                                active = None;
                            }
                            let mode = if latest.is_none() { DayLoad::Immediate } else { mode };
                            if matches!(mode, DayLoad::Immediate) || matches!(request.view, DayView::WholeDay)
                                || latest.is_some_and(|previous| previous.date != request.date) { cached = None; }
                            generation = next;
                            latest = Some(request);
                            queued = Some(request);
                            deadline = Some(Instant::now() + match mode {
                                DayLoad::Immediate => Duration::ZERO,
                                DayLoad::Settled => ZOOM_SETTLE_DELAY,
                            });
                        }
                    }
                }
                changed = changes.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let date = *changes.borrow_and_update();
                    cached = None;
                    revision = revision.saturating_add(1);
                    if let Some(request) = latest.filter(|request| Some(request.date) == date) {
                        queued = Some(request);
                        if active.is_none() {
                            deadline = Some(Instant::now());
                        }
                    }
                }
                result = async {
                    match active.as_mut() {
                        Some(future) => future.await,
                        None => std::future::pending().await,
                    }
                } => {
                    active = None;
                    let result = result.map(|loaded| {
                        if revision == loading_revision { cached = loaded.aggregates(); }
                        loaded.data
                    });
                    if events.send(LinkEvent::DayLoaded {
                        generation: loading_generation,
                        result,
                    }).await.is_err() {
                        break;
                    }
                    if queued.is_some() && deadline.is_none() {
                        deadline = Some(Instant::now());
                    }
                }
                () = async {
                    match deadline {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                }, if active.is_none() => {
                    deadline = None;
                    if let Some(request) = queued.take() {
                        loading_generation = generation;
                        if events.send(LinkEvent::DayLoading { generation }).await.is_err() {
                            break;
                        }
                        let link = link.clone();
                        let cached = cached.clone();
                        loading_revision = revision;
                        active = Some(Box::pin(async move { link.query_day(request, cached).await }));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failure_codes_preserve_protocol_kinds_and_type_local_failures() {
        struct Case {
            failure: LoadDataError,
            expected: LoadDataErrorCode,
            shown: &'static str,
        }
        for case in [
            Case {
                failure: LoadDataError::Request {
                    source: RequestCoreError::Timeout,
                },
                expected: LoadDataErrorCode::Local(LocalFailureCode::Timeout),
                shown: "timeout",
            },
            Case {
                failure: LoadDataError::Request {
                    source: RequestCoreError::Disconnected,
                },
                expected: LoadDataErrorCode::Local(LocalFailureCode::Connection),
                shown: "connection_error",
            },
            Case {
                failure: LoadDataError::InvalidData,
                expected: LoadDataErrorCode::Local(LocalFailureCode::InvalidResponse),
                shown: "invalid_response",
            },
            Case {
                failure: LoadDataError::StaleExhausted,
                expected: LoadDataErrorCode::Local(LocalFailureCode::StaleExhausted),
                shown: "stale_exhausted",
            },
            Case {
                failure: LoadDataError::Request {
                    source: RequestCoreError::Busy,
                },
                expected: LoadDataErrorCode::Protocol(ErrorKind::Busy),
                shown: "busy",
            },
            Case {
                failure: LoadDataError::from(RequestCoreError::Call {
                    source: CallCoreError::Protocol {
                        kind: ErrorKind::InvalidRange,
                    },
                }),
                expected: LoadDataErrorCode::Protocol(ErrorKind::InvalidRange),
                shown: "invalid_range",
            },
        ] {
            assert_eq!(case.failure.code(), case.expected);
            assert_eq!(case.failure.code().as_str(), case.shown);
            assert_eq!(case.failure.code().to_string(), case.shown);
        }
    }
    #[test]
    fn milliseconds_per_pixel_uses_physical_pixels_at_three_scales() {
        struct Case {
            scale: f32,
            width: u32,
            resolution: u64,
        }
        for case in [
            Case {
                scale: 1.,
                width: 1000,
                resolution: 86400,
            },
            Case {
                scale: 1.25,
                width: 1250,
                resolution: 69120,
            },
            Case {
                scale: 1.5,
                width: 1500,
                resolution: 57600,
            },
        ] {
            let width = DayRequest::physical_width(1000., case.scale).expect("physical width");
            assert_eq!(width.get(), case.width);
            let request = DayRequest {
                date: "2026-11-21".parse().expect("date"),
                view: DayView::WholeDay,
                physical_width: width,
            };
            assert_eq!(
                request
                    .ms_per_pixel(UtcRange {
                        start: 0,
                        end: 86_400_000
                    })
                    .expect("resolution")
                    .0
                    .get(),
                case.resolution
            );
        }
        assert!(DayRequest::physical_width(0., 1.).is_err());
        assert!(
            !DayGeneration::default()
                .next()
                .accepts(DayGeneration::default())
        );
    }
}
