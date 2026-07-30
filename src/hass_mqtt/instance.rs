use crate::hass_mqtt::base::EntityConfig;
use crate::service::hass::HassClient;
use crate::service::state::StateHandle;
use anyhow::Context;
use async_trait::async_trait;
use serde::Serialize;
use std::sync::Arc;
use tokio::task::JoinSet;

// Bound discovery traffic while avoiding a fixed delay for every entity.
const CONFIG_PUBLISH_CONCURRENCY: usize = 4;

#[async_trait]
pub trait EntityInstance: Send + Sync {
    async fn publish_config(&self, state: &StateHandle, client: &HassClient) -> anyhow::Result<()>;
    async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()>;
}

pub async fn publish_entity_config<T: Serialize>(
    integration: &str,
    state: &StateHandle,
    client: &HassClient,
    base: &EntityConfig,
    config: &T,
) -> anyhow::Result<()> {
    // TODO: remember all published topics for future GC

    let disco = state.get_hass_disco_prefix().await;
    let topic = format!(
        "{disco}/{integration}/{unique_id}/config",
        unique_id = base.unique_id
    );

    client.publish_obj(topic, config).await
}

#[derive(Default, Clone)]
pub struct EntityList {
    entities: Vec<Arc<dyn EntityInstance + Send + Sync + 'static>>,
}

impl EntityList {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add<E: EntityInstance + Send + Sync + 'static>(&mut self, e: E) {
        self.entities.push(Arc::new(e));
    }

    pub fn len(&self) -> usize {
        self.entities.len()
    }

    pub async fn publish_config(
        &self,
        state: &StateHandle,
        client: &HassClient,
    ) -> anyhow::Result<()> {
        let state = state.clone();
        let client = client.clone();
        let mut entities = self.entities.iter().cloned();
        let mut jobs = JoinSet::new();

        loop {
            while jobs.len() < CONFIG_PUBLISH_CONCURRENCY {
                let Some(entity) = entities.next() else {
                    break;
                };
                let state = state.clone();
                let client = client.clone();
                jobs.spawn(async move {
                    entity
                        .publish_config(&state, &client)
                        .await
                        .context("EntityList::publish_config")
                });
            }

            let Some(result) = jobs.join_next().await else {
                break;
            };
            result.context("MQTT discovery publication task")??;
        }

        Ok(())
    }

    pub async fn notify_state(&self, client: &HassClient) -> anyhow::Result<()> {
        for e in &self.entities {
            e.notify_state(client)
                .await
                .context("EntityList::notify_state")?;
        }
        Ok(())
    }
}
