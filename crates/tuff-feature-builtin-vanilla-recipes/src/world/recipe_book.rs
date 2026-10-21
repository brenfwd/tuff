//! The recipe book lives in core; this checks it against the vanilla
//! recipes.

pub use tuff_core::world::recipe_book::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crafting::registered::vanilla_set;
    use tuff_core::protocol::play::recipe_flags;

    #[tuff_core::watchdog]
    #[test]
    fn recipes_are_learned_once_and_special_recipes_never() {
        let mut book = RecipeBook::default();
        let entries = book.add(vanilla_set(), ["minecraft:piston"]);
        assert!(entries.iter().any(|(e, _)| e.recipe == "minecraft:piston"));
        assert!(
            entries
                .iter()
                .all(|(_, flags)| flags & recipe_flags::HIGHLIGHT != 0)
        );
        assert!(book.contains("minecraft:piston"));
        assert!(book.add(vanilla_set(), ["minecraft:piston"]).is_empty());
        assert!(
            book.add(vanilla_set(), ["minecraft:repair_item"])
                .is_empty()
        );
        assert!(!book.contains("minecraft:repair_item"));
        let table = book.add(vanilla_set(), ["minecraft:crafting_table"]);
        assert!(table.iter().all(|(e, flags)| {
            e.recipe != "minecraft:crafting_table" || flags & recipe_flags::NOTIFICATION == 0
        }));

        let piston = crate::crafting::display::displays_for("minecraft:piston")
            .next()
            .unwrap();
        book.seen(vanilla_set(), piston.id);
        let initial = book.initial_entries(vanilla_set());
        let (_, flags) = initial.iter().find(|(e, _)| e.id == piston.id).unwrap();
        assert_eq!(*flags, 0);
        let save = book.save();
        assert!(save.known.contains(&"minecraft:piston".to_owned()));
        assert!(!save.highlight.contains(&"minecraft:piston".to_owned()));
        assert_eq!(RecipeBook::load(&save, vanilla_set()), book);
    }
}
