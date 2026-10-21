//! Loading the vanilla plugins leaves every network id where clients and
//! saved worlds expect it: a synchronized registry's ids are the indices
//! of its sorted entries, as the Registry Data packets send them.

use tuff::registry::Registries;
use tuff::world::registry_id;
use tuff_vanilla_data::data_registry;

#[tuff_core::watchdog]
#[test]
fn synchronized_registry_ids_follow_sorted_entry_order() {
    let mut registries = Registries::new();
    tuff::feature::load_all(&tuff::vanilla_features::all(), &mut registries).unwrap();
    for registry in data_registry::ALL
        .iter()
        .filter(|registry| registry.synchronized)
    {
        assert!(
            registry.entries.windows(2).all(|pair| pair[0] < pair[1]),
            "{} is not sorted and unique",
            registry.key
        );
        for (index, entry) in registry.entries.iter().enumerate() {
            assert_eq!(
                registry_id(registry, entry),
                index as i32,
                "{entry} in {}",
                registry.key
            );
        }
    }
}
