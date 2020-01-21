use std::collections::{hash_map::Entry, HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use actix::prelude::*;
use actix_web::ResponseError;
use failure::Fail;
use futures::{future, Future};
use serde::{Deserialize, Serialize};

use relay_common::{metric, ProjectId};
use relay_config::Config;

use crate::actors::project::{Project, ProjectState};
use crate::actors::project_local_cache::ProjectLocalCache;
use crate::actors::project_upstream_cache::ProjectUpstreamCache;
use crate::metrics::{RelayCounters, RelayHistograms, RelayTimers};
use crate::utils::{RedisPool, Response};

#[cfg(feature = "processing")]
use {crate::actors::redis_project::RedisProjectCache, relay_common::clone};

#[derive(Fail, Debug)]
pub enum ProjectError {
    #[fail(display = "failed to fetch project state from upstream")]
    FetchFailed,

    #[fail(display = "could not schedule project fetching")]
    ScheduleFailed(#[cause] MailboxError),
}

impl ResponseError for ProjectError {}

#[derive(Clone, Copy, Debug)]
struct ProjectUpdate {
    project_id: ProjectId,
    instant: Instant,
}

impl ProjectUpdate {
    pub fn new(project_id: ProjectId) -> Self {
        ProjectUpdate {
            project_id,
            instant: Instant::now(),
        }
    }
}

pub struct ProjectCache {
    config: Arc<Config>,
    projects: HashMap<ProjectId, Addr<Project>>,
    updates: VecDeque<ProjectUpdate>,

    local_cache: Addr<ProjectLocalCache>,
    upstream_cache: Addr<ProjectUpstreamCache>,
    #[cfg(feature = "processing")]
    redis_cache: Option<Addr<RedisProjectCache>>,
}

impl ProjectCache {
    pub fn new(
        config: Arc<Config>,
        local_cache: Addr<ProjectLocalCache>,
        upstream_cache: Addr<ProjectUpstreamCache>,
        _redis: Option<RedisPool>,
    ) -> Self {
        #[cfg(feature = "processing")]
        let redis_cache = _redis.map(|pool| {
            SyncArbiter::start(
                config.cpu_concurrency(),
                clone!(config, || RedisProjectCache::new(
                    config.clone(),
                    pool.clone()
                )),
            )
        });

        ProjectCache {
            config,
            projects: HashMap::new(),
            updates: VecDeque::new(),

            local_cache,
            upstream_cache,
            #[cfg(feature = "processing")]
            redis_cache,
        }
    }
}

impl Actor for ProjectCache {
    type Context = Context<Self>;

    fn started(&mut self, context: &mut Self::Context) {
        // Set the mailbox size to the size of the event buffer. This is a rough estimate but
        // should ensure that we're not dropping messages if the main arbiter running this actor
        // gets hammered a bit.
        let mailbox_size = self.config.event_buffer_size() as usize;
        context.set_mailbox_capacity(mailbox_size);

        log::info!("project cache started");
    }

    fn stopped(&mut self, _ctx: &mut Self::Context) {
        log::info!("project cache stopped");
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GetProject {
    pub id: ProjectId,
}

impl Message for GetProject {
    type Result = Addr<Project>;
}

impl Handler<GetProject> for ProjectCache {
    type Result = Addr<Project>;

    fn handle(&mut self, message: GetProject, context: &mut Context<Self>) -> Self::Result {
        let config = self.config.clone();
        metric!(histogram(RelayHistograms::ProjectStateCacheSize) = self.projects.len() as u64);
        match self.projects.entry(message.id) {
            Entry::Occupied(entry) => {
                metric!(counter(RelayCounters::ProjectCacheHit) += 1);
                entry.get().clone()
            }
            Entry::Vacant(entry) => {
                metric!(counter(RelayCounters::ProjectCacheMiss) += 1);
                let project = Project::new(message.id, config, context.address()).start();
                entry.insert(project.clone());
                project
            }
        }
    }
}

#[derive(Clone, Copy)]
pub struct FetchProjectState {
    pub id: ProjectId,
}

pub struct ProjectStateResponse {
    pub state: Arc<ProjectState>,
    pub is_local: bool,
}

impl ProjectStateResponse {
    pub fn managed(state: Arc<ProjectState>) -> Self {
        ProjectStateResponse {
            state,
            is_local: false,
        }
    }

    pub fn local(state: Arc<ProjectState>) -> Self {
        ProjectStateResponse {
            state,
            is_local: true,
        }
    }
}

impl Message for FetchProjectState {
    type Result = Result<ProjectStateResponse, ()>;
}

pub struct FetchOptionalProjectState {
    pub id: ProjectId,
}

impl Message for FetchOptionalProjectState {
    type Result = Result<OptionalProjectStateResponse, ()>;
}

pub struct OptionalProjectStateResponse {
    pub state: Option<Arc<ProjectState>>,
}

impl Handler<FetchProjectState> for ProjectCache {
    type Result = Response<ProjectStateResponse, ()>;

    fn handle(&mut self, message: FetchProjectState, _context: &mut Self::Context) -> Self::Result {
        // Remove outdated projects that are not being refreshed from the cache. If the project is
        // being updated now, also remove its update entry from the queue, since we will be
        // inserting a new timestamp at the end (see `extend`).
        let eviction_start = Instant::now();
        let eviction_threshold = eviction_start - 2 * self.config.project_cache_expiry();
        while let Some(update) = self.updates.get(0) {
            if update.instant > eviction_threshold {
                break;
            }

            if update.project_id != message.id {
                self.projects.remove(&update.project_id);
            }

            self.updates.pop_front();
        }

        // The remaining projects are not outdated anymore. Still, clean them from the queue to
        // reinsert them at the end, as they are now receiving an updated timestamp. Then,
        // batch-insert all new projects with the new timestamp.
        //
        // TODO(markus): This is way too slow. This used to be OK when part of a batched fetch. We
        // need some priority queue dingus here.
        self.updates
            .retain(|update| update.project_id != message.id);
        self.updates.push_back(ProjectUpdate::new(message.id));

        metric!(timer(RelayTimers::ProjectStateEvictionDuration) = eviction_start.elapsed());

        let upstream_cache = self.upstream_cache.clone();
        #[cfg(feature = "processing")]
        let redis_cache = self.redis_cache.clone();

        let fetch_local = self
            .local_cache
            .send(FetchOptionalProjectState { id: message.id })
            .map_err(|_| ());

        let future = fetch_local.and_then(move |result| {
            if let Ok(response) = result {
                // response should be infallible for local cache
                if let Some(state) = response.state {
                    return Box::new(future::ok(ProjectStateResponse::local(state)))
                        as ResponseFuture<_, _>;
                }
            }

            #[cfg(not(feature = "processing"))]
            let fetch_redis = future::ok(Ok(OptionalProjectStateResponse { state: None }));

            #[cfg(feature = "processing")]
            let fetch_redis: ResponseFuture<_, _> = if let Some(ref redis_cache) = redis_cache {
                Box::new(
                    redis_cache
                        .send(FetchOptionalProjectState { id: message.id })
                        .map_err(|_| ()),
                )
            } else {
                Box::new(future::ok(Ok(OptionalProjectStateResponse { state: None })))
            };

            let fetch_redis = fetch_redis.and_then(move |result| {
                if let Ok(response) = result {
                    // response should be infallible for redis cache
                    if let Some(state) = response.state {
                        return Box::new(future::ok(ProjectStateResponse::local(state)))
                            as ResponseFuture<_, _>;
                    }
                }

                let fetch_upstream = upstream_cache
                    .send(message.clone())
                    .map_err(|_| ())
                    .and_then(move |result| result.map_err(|_| ()));

                Box::new(fetch_upstream)
            });

            Box::new(fetch_redis)
        });

        Response::r#async(future)
    }
}
