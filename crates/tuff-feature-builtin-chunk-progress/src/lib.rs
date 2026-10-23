use std::sync::Arc;

use tuff_core::feature::{Feature, LoadContext};
use tuff_core::protocol::play::{BossAction, BossEvent};
use tuff_core::world::session::PlayerSession;
use tuff_core::world::session::hooks::{
    HookFuture as SessionHookFuture, SessionHooks, SessionTick,
};
use tuff_item::load::{LoadError, Phase};
use tuff_item::text::TextComponent;

pub const ID: &str = "example-chunk-progress";

const BAR_UUID: u128 = 0x18A7E338AD6042CE924A518D563369C3;

const COLOR_GREEN: i32 = 3;
const OVERLAY_PROGRESS: i32 = 0;

pub struct ExampleChunkFeature;

pub struct ExampleChunkHook;

#[derive(Default)]
pub struct ExampleChunkHookData {
    shown: bool,
    prev_sent: usize,
    prev_subd: usize,
}

impl SessionHooks for ExampleChunkHook {
    fn tick<'a>(
        &'a self,
        session: &'a mut PlayerSession,
        phase: SessionTick,
    ) -> SessionHookFuture<'a> {
        if phase == SessionTick::Late {
            let sent = session.view.sent.len();
            let subd = session.view.subscribed.len();
            let complete = sent >= subd;
            let progress = if complete {
                1.0
            } else {
                sent as f32 / subd as f32
            };

            let mut data = session
                .ext
                .remove::<ExampleChunkHookData>()
                .unwrap_or_default();

            if complete {
                if data.shown {
                    data.shown = false;
                    session.send(&BossEvent {
                        uuid: BAR_UUID,
                        action: BossAction::Remove,
                    });
                }
            } else if !data.shown {
                data.shown = true;
                let title = TextComponent::new(format!("Loading Chunks: {sent} / {subd}"));
                session.send(&BossEvent {
                    uuid: BAR_UUID,
                    action: BossAction::Add {
                        title: &title,
                        progress,
                        color: COLOR_GREEN,
                        overlay: OVERLAY_PROGRESS,
                        flags: 0,
                    },
                });
            } else if sent != data.prev_sent || subd != data.prev_subd {
                let title = TextComponent::new(format!("Loading Chunks: {sent} / {subd}"));
                session.send(&BossEvent {
                    uuid: BAR_UUID,
                    action: BossAction::Title(&title),
                });
                session.send(&BossEvent {
                    uuid: BAR_UUID,
                    action: BossAction::Progress(progress),
                });
            }

            data.prev_sent = sent;
            data.prev_subd = subd;
            *session.ext.entry::<ExampleChunkHookData>() = data;
        }
        Box::pin(async {})
    }
}

impl ExampleChunkFeature {
    fn load_content(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        cx.registries()
            .session_hooks
            .push(Arc::new(ExampleChunkHook));
        Ok(())
    }
}

impl Feature for ExampleChunkFeature {
    fn id(&self) -> &'static str {
        ID
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        match cx.phase() {
            Phase::Types => (),
            Phase::Content => self.load_content(cx)?,
            Phase::Resolve => (),
            Phase::Freeze => (),
        }
        Ok(())
    }
}
