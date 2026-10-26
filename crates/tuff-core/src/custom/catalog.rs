//! The catalog menu: every custom item, paged into a six-row chest.

use tuff_vanilla_data::Item;

use super::CustomContent;
use tuff_item::item::{ItemPicture, ItemStack, MintReason};
use tuff_item::text::TextComponent;

pub const ROWS: u8 = 6;
pub const PAGE_SIZE: usize = 45;
pub const PREVIOUS_SLOT: u8 = 45;
pub const NEXT_SLOT: u8 = 53;
pub const TITLE_KEY: &str = "tuff.catalog.title";
pub const TAKEN_KEY: &str = "tuff.catalog.taken";

/// What a click on the catalog did.
#[derive(Debug, PartialEq)]
pub enum CatalogClick {
    Page(usize),
    Take(ItemStack),
    Nothing,
}

/// A catalog menu open on one page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Catalog {
    pub page: usize,
}

impl Catalog {
    /// The title, from the pack's language table when it translates
    /// [`TITLE_KEY`], with the 1-based page and page count.
    pub fn title(&self, content: &CustomContent) -> String {
        let pages = content.catalog_pages(PAGE_SIZE).max(1);
        if content.translation(TITLE_KEY).is_some() {
            let text = TextComponent::translate(
                TITLE_KEY,
                [
                    TextComponent::new((self.page + 1).to_string()),
                    TextComponent::new(pages.to_string()),
                ],
            );
            content.plain(&text)
        } else {
            format!("Custom items ({}/{})", self.page + 1, pages)
        }
    }

    /// The feedback line for taking the custom item `id`, from the pack's
    /// language table when it translates [`TAKEN_KEY`].
    pub fn taken_message(content: &CustomContent, id: &str) -> String {
        if content.translation(TAKEN_KEY).is_some() {
            content.plain(&TextComponent::translate(
                TAKEN_KEY,
                [TextComponent::new(id)],
            ))
        } else {
            format!("Took {id}")
        }
    }

    /// The filled slots: the page's items then the paging arrows.
    pub fn slots(&self, content: &CustomContent) -> Vec<(u8, ItemPicture)> {
        let mut slots: Vec<(u8, ItemPicture)> = content
            .catalog_page(self.page, PAGE_SIZE)
            .into_iter()
            .enumerate()
            .map(|(i, stack)| (i as u8, stack))
            .collect();
        if self.page > 0 {
            slots.push((PREVIOUS_SLOT, ItemPicture::new(Item::Arrow, 1)));
        }
        if self.page + 1 < content.catalog_pages(PAGE_SIZE) {
            slots.push((NEXT_SLOT, ItemPicture::new(Item::Arrow, 1)));
        }
        slots
    }

    pub fn click(&self, content: &CustomContent, slot: u8) -> CatalogClick {
        match slot {
            PREVIOUS_SLOT if self.page > 0 => CatalogClick::Page(self.page - 1),
            NEXT_SLOT if self.page + 1 < content.catalog_pages(PAGE_SIZE) => {
                CatalogClick::Page(self.page + 1)
            }
            #[expect(
                clippy::disallowed_methods,
                reason = "creative catalog hands out a copy of the clicked custom item (click)"
            )]
            s if usize::from(s) < PAGE_SIZE => content
                .catalog_page(self.page, PAGE_SIZE)
                .into_iter()
                .nth(usize::from(s))
                .map_or(CatalogClick::Nothing, |picture| {
                    CatalogClick::Take(ItemStack::mint_like(
                        &picture,
                        picture.count(),
                        MintReason::Command,
                    ))
                }),
            _ => CatalogClick::Nothing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::custom::{AssetSet, CustomItemDef, Definitions};
    use bytes::Bytes;

    fn content(items: usize, lang: bool) -> std::sync::Arc<CustomContent> {
        let mut defs = Definitions::default();
        for i in 0..items {
            let id = format!("ex:i{i:03}");
            defs.items
                .register("ex", &id, CustomItemDef::new(&id, Item::Paper))
                .unwrap();
        }
        if lang {
            defs.register_assets(
                "ex",
                "lang",
                AssetSet {
                    namespace: "ex".into(),
                    files: vec![(
                        "lang/en_us.json".to_owned(),
                        Bytes::from_static(br#"{"tuff.catalog.title":"Gems %s of %s"}"#),
                    )],
                },
            )
            .unwrap();
        }
        defs.resolve().unwrap()
    }

    fn taken_id(content: &CustomContent, click: CatalogClick) -> String {
        let CatalogClick::Take(stack) = click else {
            panic!("not a take");
        };
        content.item_of(&stack).unwrap().id.clone()
    }

    #[tuff_core::watchdog]
    #[test]
    fn pages_through_the_catalog() {
        let content = content(50, false);
        let first = Catalog::default();
        assert_eq!(first.slots(&content).len(), 46);
        assert_eq!(taken_id(&content, first.click(&content, 0)), "ex:i000");
        assert_eq!(first.click(&content, PREVIOUS_SLOT), CatalogClick::Nothing);
        let second = Catalog { page: 1 };
        assert_eq!(first.click(&content, NEXT_SLOT), CatalogClick::Page(1));
        assert_eq!(second.slots(&content).len(), 6);
        assert_eq!(taken_id(&content, second.click(&content, 0)), "ex:i045");
        assert_eq!(second.click(&content, 5), CatalogClick::Nothing);
        assert_eq!(second.click(&content, NEXT_SLOT), CatalogClick::Nothing);
        assert_eq!(second.click(&content, PREVIOUS_SLOT), CatalogClick::Page(0));
        assert_eq!(second.title(&content), "Custom items (2/2)");
    }

    #[tuff_core::watchdog]
    #[test]
    fn taken_message_uses_the_pack_translation() {
        let plain = content(1, false);
        assert_eq!(Catalog::taken_message(&plain, "ex:i000"), "Took ex:i000");
        let mut defs = Definitions::default();
        defs.register_assets(
            "ex",
            "lang",
            AssetSet {
                namespace: "ex".into(),
                files: vec![(
                    "lang/en_us.json".to_owned(),
                    Bytes::from_static(br#"{"tuff.catalog.taken":"Got %s!"}"#),
                )],
            },
        )
        .unwrap();
        let translated = defs.resolve().unwrap();
        assert_eq!(
            Catalog::taken_message(&translated, "ex:i000"),
            "Got ex:i000!"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn title_uses_the_pack_translation() {
        let content = content(1, true);
        assert_eq!(Catalog::default().title(&content), "Gems 1 of 1");
    }
}
