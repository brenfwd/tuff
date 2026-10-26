#![cfg_attr(
    test,
    expect(
        clippy::disallowed_methods,
        reason = "test code is exempt from the door gate"
    )
)]

pub mod enchantment;
pub mod item;
pub mod load;
pub mod random;
pub mod registries;
pub mod registry;
pub mod text;
