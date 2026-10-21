//! Client physics for fake players: ground and air movement as the
//! vanilla client computes it before reporting a position.

mod ground;

pub use ground::{GroundMotion, Rig};

/// Registers the movement a fake player's client simulates.
pub struct PlayersFeature;

impl tuff_core::feature::Feature for PlayersFeature {
    fn id(&self) -> &'static str {
        "vanilla-players"
    }

    fn load(
        &self,
        cx: &mut tuff_core::feature::LoadContext,
    ) -> Result<(), tuff_item::load::LoadError> {
        if cx.phase() == tuff_item::load::Phase::Content {
            let id = cx.feature_id();
            cx.registries()
                .fake_motion
                .fill(id, std::sync::Arc::new(GroundMotion))?;
        }
        Ok(())
    }
}
