use super::*;

#[tuff_core::watchdog]
#[test]
fn ravagers_carry_pillagers_in_the_last_normal_wave_and_evokers_first_after() {
    assert_eq!(ravager_rider(3, 0), None);
    assert_eq!(ravager_rider(5, 0), Some(EntityType::Pillager));
    assert_eq!(ravager_rider(5, 1), Some(EntityType::Pillager));
    assert_eq!(ravager_rider(6, 0), None);
    assert_eq!(ravager_rider(7, 0), Some(EntityType::Evoker));
    assert_eq!(ravager_rider(7, 1), Some(EntityType::Vindicator));
    assert_eq!(ravager_rider(8, 0), Some(EntityType::Evoker));
}
