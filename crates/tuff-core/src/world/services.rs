//! Long-running services features run beside the world's actors (the
//! dragon fights). A feature registers a [`ServiceFactory`]; when a world
//! opens, each factory makes its service, which is kept in
//! `Shared::services` and started once the shared state exists. Feature
//! code finds its own service again by type.

use std::any::Any;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use super::Shared;
use super::dimension::{DimensionId, Dimensions};
use tuff_world_model::BlockPos;

/// Makes a feature's service for one world.
pub trait ServiceFactory: Send + Sync {
    fn create(&self, dimensions: &Dimensions) -> Arc<dyn WorldService>;
}

/// What item frames tell the service that tracks filled maps.
pub trait MapFrames: Send + Sync {
    /// The frame of entity `entity_id` at `pos` shows map `id`.
    fn framed(&self, id: u32, dimension: DimensionId, pos: BlockPos, rotation: i32, entity_id: i32);

    /// Map `id` left the frame of entity `entity_id` at `pos`.
    fn unframed(&self, id: u32, pos: BlockPos, entity_id: i32);
}

/// One feature's running part of a world.
pub trait WorldService: Any + Send + Sync {
    /// Starts the service's actors; the world's shared state now exists.
    fn start(&self, shared: &Arc<Shared>);

    /// Resolves once the service has handled game tick `tick` (tests wait
    /// on it the way they wait on regions).
    fn settled(&self, _tick: u64) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }

    /// Resolves once everything the service keeps is saved; called as the
    /// world shuts down.
    fn flush(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }

    /// The map frame sink this service offers, if it keeps maps.
    fn map_frames(&self) -> Option<&dyn MapFrames> {
        None
    }

    /// The feature host this service offers, if it owns the generators.
    fn feature_host(&self) -> Option<Arc<dyn super::features::FeatureHost>> {
        None
    }
}

/// The services a world runs.
#[derive(Default)]
pub struct Services(Vec<Arc<dyn WorldService>>);

impl Services {
    pub fn create(factories: &[Arc<dyn ServiceFactory>], dimensions: &Dimensions) -> Self {
        let mut services: Vec<Arc<dyn WorldService>> =
            vec![Arc::new(super::contact::PortalClaims::default())];
        services.extend(factories.iter().map(|factory| factory.create(dimensions)));
        Self(services)
    }

    /// The service of type `T`, if one runs.
    pub fn get<T: WorldService>(&self) -> Option<&T> {
        self.0
            .iter()
            .find_map(|service| (&**service as &dyn Any).downcast_ref::<T>())
    }

    pub fn map_frames(&self) -> Option<&dyn MapFrames> {
        self.0.iter().find_map(|service| service.map_frames())
    }

    pub fn feature_host(&self) -> Option<Arc<dyn super::features::FeatureHost>> {
        self.0.iter().find_map(|service| service.feature_host())
    }

    pub fn start(&self, shared: &Arc<Shared>) {
        for service in &self.0 {
            service.start(shared);
        }
    }

    pub async fn flush(&self) {
        for service in &self.0 {
            service.flush().await;
        }
    }

    pub async fn settled(&self, tick: u64) {
        for service in &self.0 {
            service.settled(tick).await;
        }
    }
}
