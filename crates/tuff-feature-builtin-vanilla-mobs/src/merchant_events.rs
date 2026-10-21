use tuff_core::world::messages::{PlayerEvent, SessionTask};
use tuff_core::world::trading::MerchantView;
use tuff_item::text::TextComponent;

/// What a merchant-open task opens.
#[derive(Debug, Clone)]
pub struct Opened {
    pub title: TextComponent,
    pub view: MerchantView,
}

/// What a merchant-update task shows.
#[derive(Debug, Clone)]
pub struct Updated(pub MerchantView);

pub fn open(title: TextComponent, view: MerchantView) -> Box<PlayerEvent> {
    let opened = Opened {
        title: title.clone(),
        view: view.clone(),
    };
    Box::new(PlayerEvent::Run(
        SessionTask::new(move |session| Box::pin(session.open_merchant(title, view)))
            .with_data(opened),
    ))
}

pub fn update(view: MerchantView) -> Box<PlayerEvent> {
    let updated = Updated(view.clone());
    Box::new(PlayerEvent::Run(
        SessionTask::new(move |session| Box::pin(async move { session.merchant_updated(view) }))
            .with_data(updated),
    ))
}

pub fn closed(merchant: i32) -> Box<PlayerEvent> {
    Box::new(PlayerEvent::Run(SessionTask::new(move |session| {
        Box::pin(session.merchant_closed(merchant))
    })))
}
