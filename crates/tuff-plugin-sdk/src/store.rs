// SPDX-License-Identifier: MIT
//! Typed values in the plugin's persistent store, which keeps them across
//! unloads and restarts:
//!
//! ```ignore
//! use tuff_plugin_sdk::store::{Stored, StoredMap};
//!
//! static GREETED: Stored<u64> = Stored::new("greeted");
//! static VISITS: StoredMap<u32> = StoredMap::new("visits");
//!
//! let total = GREETED.update(|n| n.unwrap_or(0) + 1)?;
//! let mine = VISITS.update(&player.uuid, |n| n.unwrap_or(0) + 1)?;
//! ```
//!
//! Values are stored as text a person (or another language's SDK) can
//! read: numbers in decimal, `true`/`false`, strings as UTF-8 and uuids
//! as 32 hex digits; byte vectors are stored as they are. A map entry's
//! key is `<prefix>/<key>`.

use std::fmt::Display;

use crate::bindings::tuff::plugin::host;
use crate::types::Uuid;

/// How a value is stored.
pub trait Codec: Sized {
    fn encode(&self) -> Vec<u8>;

    /// `None` when `bytes` is not a value of this type.
    fn decode(bytes: &[u8]) -> Option<Self>;
}

macro_rules! text_codec {
    ($($ty:ty),*) => {$(
        impl Codec for $ty {
            fn encode(&self) -> Vec<u8> {
                self.to_string().into_bytes()
            }

            fn decode(bytes: &[u8]) -> Option<Self> {
                std::str::from_utf8(bytes).ok()?.parse().ok()
            }
        }
    )*};
}

text_codec!(u8, u16, u32, u64, i8, i16, i32, i64, f32, f64, bool, String);

impl Codec for Vec<u8> {
    fn encode(&self) -> Vec<u8> {
        self.clone()
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        Some(bytes.to_vec())
    }
}

impl Codec for Uuid {
    fn encode(&self) -> Vec<u8> {
        self.to_string().into_bytes()
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        Uuid::parse(std::str::from_utf8(bytes).ok()?)
    }
}

fn load<T: Codec>(key: &str) -> Option<T> {
    host::storage_get(key).and_then(|bytes| T::decode(&bytes))
}

fn save<T: Codec>(key: &str, value: Option<&T>) -> Result<(), crate::HostError> {
    host::storage_set(key, value.map(Codec::encode).as_deref()).map_err(Into::into)
}

/// The raw bytes stored under `key`.
pub fn get_bytes(key: &str) -> Option<Vec<u8>> {
    host::storage_get(key)
}

/// Stores raw bytes under `key`; an error when the store's quota would be
/// exceeded.
pub fn set_bytes(key: &str, value: &[u8]) -> Result<(), crate::HostError> {
    host::storage_set(key, Some(value)).map_err(Into::into)
}

/// Removes `key`.
pub fn remove_bytes(key: &str) -> Result<(), crate::HostError> {
    host::storage_set(key, None).map_err(Into::into)
}

/// One stored value.
pub struct Stored<T> {
    key: &'static str,
    kind: std::marker::PhantomData<fn() -> T>,
}

impl<T: Codec> Stored<T> {
    pub const fn new(key: &'static str) -> Self {
        Self {
            key,
            kind: std::marker::PhantomData,
        }
    }

    /// `None` when unset or not readable as `T`.
    pub fn get(&self) -> Option<T> {
        load(self.key)
    }

    pub fn get_or(&self, default: T) -> T {
        self.get().unwrap_or(default)
    }

    pub fn set(&self, value: &T) -> Result<(), crate::HostError> {
        save(self.key, Some(value))
    }

    pub fn remove(&self) -> Result<(), crate::HostError> {
        save::<T>(self.key, None)
    }

    /// Stores `f(old value)` and returns it.
    pub fn update(&self, f: impl FnOnce(Option<T>) -> T) -> Result<T, crate::HostError> {
        let value = f(self.get());
        self.set(&value)?;
        Ok(value)
    }
}

/// Stored values of type `T` under one prefix, by key (a player's uuid, a
/// name).
pub struct StoredMap<T> {
    prefix: &'static str,
    kind: std::marker::PhantomData<fn() -> T>,
}

impl<T: Codec> StoredMap<T> {
    pub const fn new(prefix: &'static str) -> Self {
        Self {
            prefix,
            kind: std::marker::PhantomData,
        }
    }

    /// The store key of entry `key`.
    pub fn key(&self, key: impl Display) -> String {
        format!("{}/{key}", self.prefix)
    }

    pub fn get(&self, key: impl Display) -> Option<T> {
        load(&self.key(key))
    }

    pub fn get_or(&self, key: impl Display, default: T) -> T {
        self.get(key).unwrap_or(default)
    }

    pub fn set(&self, key: impl Display, value: &T) -> Result<(), crate::HostError> {
        save(&self.key(key), Some(value))
    }

    pub fn remove(&self, key: impl Display) -> Result<(), crate::HostError> {
        save::<T>(&self.key(key), None)
    }

    /// Stores `f(old value)` under `key` and returns it.
    pub fn update(
        &self,
        key: impl Display,
        f: impl FnOnce(Option<T>) -> T,
    ) -> Result<T, crate::HostError> {
        let key = self.key(key);
        let value = f(load(&key));
        save(&key, Some(&value))?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn values_are_stored_as_readable_text() {
        assert_eq!(42u32.encode(), b"42");
        assert_eq!(u32::decode(b"42"), Some(42));
        assert_eq!(u32::decode(b"x"), None);
        assert_eq!((-1.5f64).encode(), b"-1.5");
        assert_eq!(true.encode(), b"true");
        assert_eq!(
            String::decode("h\u{e9}".as_bytes()),
            Some("h\u{e9}".to_owned())
        );
        let uuid = Uuid::from_u128(0xab);
        assert_eq!(uuid.encode(), b"000000000000000000000000000000ab");
        assert_eq!(Uuid::decode(&uuid.encode()), Some(uuid));
        let visits: StoredMap<u32> = StoredMap::new("visits");
        assert_eq!(visits.key(uuid), "visits/000000000000000000000000000000ab");
    }
}
