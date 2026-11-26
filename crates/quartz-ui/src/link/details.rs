//! Read-only sidebar queries on the existing pipe runtime.
use super::{DayGeneration, UtcRange};
#[cfg(windows)]
use super::{Link, LinkEvent, LoadDataError, RequestCoreError};
use quartz_protocol::{
    ApplicationId, EntityClassificationRule, EntityDailyDuration, EntityTitleDuration, LocalDate,
    TimelineSegment, WindowInstanceRef,
};
#[cfg(windows)]
use quartz_protocol::{DetailEntity, QueryEntityDetailRequest, QuerySegmentDetailRequest};
#[cfg(windows)]
use std::{collections::BTreeSet, future::Future, pin::Pin};
#[cfg(windows)]
use tokio::sync::{mpsc, watch};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct DetailGeneration(DayGeneration);
impl DetailGeneration {
    pub fn next(self) -> Self {
        Self(self.0.next())
    }
    pub fn accepts(self, incoming: Self) -> bool {
        self == incoming
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailTarget {
    Segment {
        range: UtcRange,
        app: Option<ApplicationId>,
        window_instance: Option<WindowInstanceRef>,
    },
    App(ApplicationId),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetailRequest {
    pub date: LocalDate,
    pub target: DetailTarget,
}
impl DetailRequest {
    pub fn app(self) -> Option<ApplicationId> {
        match self.target {
            DetailTarget::App(app) | DetailTarget::Segment { app: Some(app), .. } => Some(app),
            DetailTarget::Segment { app: None, .. } => None,
        }
    }
}
#[derive(Debug)]
pub struct DetailData {
    pub request: DetailRequest,
    pub intervals: Vec<TimelineSegment>,
    pub entities: Vec<EntityData>,
}
#[derive(Debug)]
pub struct EntityData {
    pub app: ApplicationId,
    pub daily: Vec<EntityDailyDuration>,
    pub titles: Vec<EntityTitleDuration>,
    pub rules: Vec<EntityClassificationRule>,
}
#[cfg(windows)]
impl Link {
    async fn entity_details(
        &self,
        app: ApplicationId,
        date: LocalDate,
    ) -> Result<EntityData, LoadDataError> {
        let response = self
            .request(QueryEntityDetailRequest {
                reference_range: None,
                entity: DetailEntity::App { id: app },
                local_date: date,
            })
            .await?;
        Ok(EntityData {
            app,
            daily: response.daily,
            titles: response.titles,
            rules: response.rules,
        })
    }
    async fn query_details(&self, request: DetailRequest) -> Result<DetailData, LoadDataError> {
        let intervals = match request.target {
            DetailTarget::Segment {
                range,
                app,
                window_instance,
            } => {
                self.request(QuerySegmentDetailRequest {
                    start_utc_ms: range.start,
                    end_utc_ms: range.end,
                    app,
                    window_instance,
                })
                .await?
                .intervals
            }
            DetailTarget::App(_) => Vec::new(),
        };
        let mut apps = BTreeSet::new();
        if let Some(app) = request.app() {
            apps.insert(app);
        }
        for interval in &intervals {
            if let Some(app) = &interval.app {
                apps.insert(app.id);
            }
        }
        // Main-track intervals may contain several applications. Their metadata supplies
        // deciding rule labels; an entity summary is shown only for a single application.
        let mut entities = Vec::new();
        for app in apps {
            entities.push(self.entity_details(app, request.date).await?);
        }
        Ok(DetailData {
            request,
            intervals,
            entities,
        })
    }
}
#[derive(Debug, Clone, Copy, Default)]
#[cfg(windows)]
pub(super) struct DetailIntent {
    pub request: Option<DetailRequest>,
    pub generation: DetailGeneration,
}
#[cfg(windows)]
pub(super) struct DetailLoader {
    intents: watch::Sender<DetailIntent>,
    changes: watch::Sender<Option<LocalDate>>,
    date: Option<LocalDate>,
    generation: DetailGeneration,
}
#[cfg(windows)]
impl DetailLoader {
    pub fn start(link: Link, events: mpsc::Sender<LinkEvent>) -> Self {
        let (intents, requests) = watch::channel(DetailIntent::default());
        let (changes, changed) = watch::channel(None);
        tokio::spawn(Self::run(link, events, requests, changed));
        Self {
            intents,
            changes,
            date: None,
            generation: DetailGeneration::default(),
        }
    }
    pub fn select(&mut self, intent: DetailIntent) -> Result<(), RequestCoreError> {
        if intent.generation < self.generation {
            return Ok(());
        }
        self.generation = intent.generation;
        self.intents
            .send(intent)
            .map_err(|_| RequestCoreError::Closed)?;
        self.date = intent.request.map(|request| request.date);
        Ok(())
    }
    pub fn changed(&self, date: LocalDate) {
        if self.date == Some(date) {
            self.changes.send_replace(Some(date));
        }
    }
    async fn run(
        link: Link,
        events: mpsc::Sender<LinkEvent>,
        mut intents: watch::Receiver<DetailIntent>,
        mut changes: watch::Receiver<Option<LocalDate>>,
    ) {
        type LoadFuture = Pin<Box<dyn Future<Output = Result<DetailData, LoadDataError>> + Send>>;
        let mut active: Option<LoadFuture> = None;
        let mut latest: Option<DetailRequest> = None;
        let mut queued = None;
        let mut generation = DetailGeneration::default();
        loop {
            tokio::select! {
                biased;
                // A selection or close already accepted by the worker takes priority over
                // a response that becomes ready in the same runtime turn.
                change = intents.changed() => {
                    if change.is_err() { break; }
                    let intent = *intents.borrow_and_update();
                    if intent.generation < generation { continue; }
                    active = None;
                    generation = intent.generation;
                    latest = intent.request;
                    queued = latest;
                }
                change = changes.changed() => {
                    if change.is_err() { break; }
                    let date = *changes.borrow_and_update();
                    if let Some(request) = latest.filter(|request| Some(request.date) == date) { queued = Some(request); }
                }
                result = async {
                    match active.as_mut() { Some(future) => future.await, None => std::future::pending().await }
                } => {
                    active = None;
                    if events.send(LinkEvent::DetailLoaded { generation, result }).await.is_err() { break; }
                }
                () = async {}, if active.is_none() && queued.is_some() => {
                    let request = queued.take().expect("a queued selection enables this branch");
                    if events.send(LinkEvent::DetailLoading { generation }).await.is_err() { break; }
                    let link = link.clone();
                    active = Some(Box::pin(async move { link.query_details(request).await }));
                }
            }
        }
    }
}
