use tuff_item::item::BurnReason;
use tuff_vanilla_data::Item;

use super::PlayerSession;
use crate::world::inventory::Inventory;
use tuff_item::item::decor::{Filterable, MAX_BOOK_PAGES, WrittenBook};
use tuff_item::item::{ItemStack, MintReason};
use tuff_nbt::Tag;

const MAX_PAGE_CHARS: usize = 1024;
const MAX_TITLE_CHARS: usize = 32;
const OFFHAND_SLOT: i32 = 40;

const MAX_RESOLVED_PAGE_CHARS: usize = 32767;

impl PlayerSession {
    /// Opening a signed book expands the selectors, scores and nbt in its
    /// pages once; a page that grows too large leaves the book as it was.
    pub async fn resolve_held_book(&mut self, off_hand: bool) {
        let Some(stack) = self.inventory.hand(off_hand) else {
            return;
        };
        let Some(book) = stack
            .components
            .written_book
            .as_deref()
            .filter(|book| !book.resolved)
        else {
            return;
        };
        let mut book = book.clone();
        let resolver = self.shared.commands.text_resolver().cloned();
        let mut ok = resolver.is_some();
        if let Some(resolver) = resolver.filter(|_| ok) {
            let mut pages = Vec::with_capacity(book.pages.len());
            for page in &book.pages {
                let mut resolved = Filterable {
                    raw: page.raw.clone(),
                    filtered: page.filtered.clone(),
                };
                for tag in std::iter::once(&mut resolved.raw).chain(resolved.filtered.as_mut()) {
                    let Some(component) = tuff_item::text::TextComponent::from_nbt(tag) else {
                        continue;
                    };
                    let mut source = self.command_source();
                    source.silent = true;
                    let mut ctx = crate::world::commands::Context {
                        actor: crate::world::commands::Actor::Player(self),
                        source,
                        nodes: &[],
                        rest: &[],
                        input: "",
                        needs_result: false,
                        frame: None,
                    };
                    let Ok(text) = resolver.resolve(&mut ctx, &component).await else {
                        continue;
                    };
                    if text.to_json().chars().count() > MAX_RESOLVED_PAGE_CHARS {
                        ok = false;
                        break;
                    }
                    *tag = text.to_nbt();
                }
                if !ok {
                    break;
                }
                pages.push(resolved);
            }
            if ok {
                book.pages = pages;
            }
        }
        book.resolved = true;
        let window = self.inventory.hand_slot(off_hand);
        if let Some(stack) = self
            .inventory_mut()
            .slot_mut(window)
            .as_mut()
            .filter(|stack| stack.item() == Item::WrittenBook)
        {
            stack.components.written_book = Some(Box::new(book));
        }
    }

    pub async fn edit_book(&mut self, slot: i32, pages: Vec<String>, title: Option<String>) {
        let valid = pages.len() <= MAX_BOOK_PAGES
            && pages.iter().all(|p| p.chars().count() <= MAX_PAGE_CHARS)
            && title
                .as_ref()
                .is_none_or(|t| t.chars().count() <= MAX_TITLE_CHARS);
        let off_hand = slot == OFFHAND_SLOT;
        if !valid || !(off_hand || (0..9).contains(&slot)) {
            return;
        }
        let window = if off_hand {
            Inventory::OFFHAND_SLOT
        } else {
            Inventory::hotbar_slot(slot.max(0) as usize)
        };
        let writable = |inventory: &mut Inventory| {
            inventory
                .slot_mut(window)
                .as_mut()
                .filter(|s| s.item() == Item::WritableBook)
                .is_some()
        };
        if !writable(self.inventory_mut()) {
            return;
        }
        let filtered = self.filter_book(&pages, title.as_deref()).await;
        let author = self.profile.name.clone();
        match title {
            None => {
                let Some(stack) = self.inventory_mut().slot_mut(window).as_mut() else {
                    return;
                };
                stack.components.writable_book = Some(
                    pages
                        .into_iter()
                        .enumerate()
                        .map(|(i, raw)| {
                            let seen = filtered.pages.get(i).filter(|f| **f != raw).cloned();
                            Filterable {
                                raw,
                                filtered: seen,
                            }
                        })
                        .collect(),
                );
            }
            Some(title) => {
                let Some(old) = self.inventory_mut().slot_mut(window).take() else {
                    return;
                };
                #[expect(
                    clippy::disallowed_methods,
                    reason = "item turning into another (edit_book)"
                )]
                let mut signed =
                    ItemStack::mint(Item::WrittenBook, old.count(), MintReason::Transform);
                old.burn(BurnReason::Transform);
                signed.components.written_book = Some(Box::new(WrittenBook {
                    title: Filterable {
                        filtered: filtered.title.filter(|f| *f != title),
                        raw: title,
                    },
                    author,
                    generation: 0,
                    pages: pages
                        .into_iter()
                        .enumerate()
                        .map(|(i, raw)| Filterable {
                            filtered: filtered
                                .pages
                                .get(i)
                                .filter(|f| **f != raw)
                                .map(|f| Tag::String(f.clone())),
                            raw: Tag::String(raw),
                        })
                        .collect(),
                    resolved: false,
                }));
                *self.inventory_mut().slot_mut(window) = Some(signed);
            }
        }
    }
}

/// What the text filter let through of a book edit; empty when no filter is
/// configured.
#[derive(Default)]
struct FilteredBook {
    pages: Vec<String>,
    title: Option<String>,
}

impl PlayerSession {
    async fn filter_book(&self, pages: &[String], title: Option<&str>) -> FilteredBook {
        let Some(filter) = self.shared.text_filter.clone() else {
            return FilteredBook::default();
        };
        let mut book = FilteredBook::default();
        for text in pages {
            let mask = filter.filter(&self.profile, text).await;
            book.pages.push(mask.apply(text).unwrap_or_default());
        }
        if let Some(title) = title {
            let mask = filter.filter(&self.profile, title).await;
            book.title = Some(mask.apply(title).unwrap_or_default());
        }
        book
    }
}
