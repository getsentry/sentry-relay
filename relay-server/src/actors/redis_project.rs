use std::collections::HashMap;
use std::sync::Arc;

use actix::prelude::*;
use relay_common::ProjectId;
use relay_config::Config;

use crate::actors::project::GetProjectStatesResponse;
use crate::utils::{ErrorBoundary, RedisError, RedisPool};

pub struct RedisProjectCache {
    config: Arc<Config>,
    redis: RedisPool,
}

impl RedisProjectCache {
    pub fn new(config: Arc<Config>, redis: RedisPool) -> Self {
        RedisProjectCache { config, redis }
    }
}

impl Actor for RedisProjectCache {
    type Context = SyncContext<Self>;

    fn started(&mut self, _ctx: &mut Self::Context) {
        log::info!("redis project cache started");
    }

    fn stopped(&mut self, _ctx: &mut Self::Context) {
        log::info!("redis project cache stopped");
    }
}

pub struct GetProjectStatesFromRedis {
    pub projects: Vec<ProjectId>,
}

impl Message for GetProjectStatesFromRedis {
    type Result = Result<GetProjectStatesResponse, RedisError>;
}

impl Handler<GetProjectStatesFromRedis> for RedisProjectCache {
    type Result = Result<GetProjectStatesResponse, RedisError>;

    fn handle(
        &mut self,
        request: GetProjectStatesFromRedis,
        _ctx: &mut Self::Context,
    ) -> Self::Result {
        let mut command = redis::cmd("MGET");
        for id in &request.projects {
            command.arg(format!(
                "{}:{}",
                self.config.projectconfig_cache_prefix(),
                id
            ));
        }

        let raw_response: Vec<String> = match self.redis {
            RedisPool::Cluster(ref pool) => {
                let mut client = pool.get().map_err(RedisError::RedisPool)?;
                command.query(&mut *client).map_err(RedisError::Redis)?
            }
            RedisPool::Single(ref pool) => {
                let mut client = pool.get().map_err(RedisError::RedisPool)?;
                command.query(&mut *client).map_err(RedisError::Redis)?
            }
        };

        let mut configs = HashMap::new();
        for (response, id) in raw_response.into_iter().zip(request.projects) {
            let config = match serde_json::from_str(&response) {
                Ok(project_state) => ErrorBoundary::Ok(project_state),
                Err(err) => ErrorBoundary::Err(Box::new(err)),
            };

            configs.insert(id, config);
        }

        Ok(GetProjectStatesResponse { configs })
    }
}
