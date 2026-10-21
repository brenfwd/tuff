//! A plugin's persistent key-value store: the copy the host keeps by
//! plugin id (written beside the plugin's module) and the working copy in
//! its sandbox. The file format is a magic tag, an entry count, then each
//! key (`u16` length) and value (`u32` length), all little-endian; files
//! are read as untrusted input and refused past the size limit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 4] = b"TPS1";

/// Longest key, in bytes.
pub const MAX_KEY: usize = 256;

/// What one plugin may store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quota {
    /// Bytes of keys and values together.
    pub bytes: usize,
    pub keys: usize,
}

impl Default for Quota {
    fn default() -> Self {
        Self {
            bytes: 1 << 20,
            keys: 1024,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Storage {
    entries: BTreeMap<String, Vec<u8>>,
    bytes: usize,
}

impl Storage {
    pub fn get(&self, key: &str) -> Option<&[u8]> {
        self.entries.get(key).map(Vec::as_slice)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Bytes of keys and values held.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Sets or (with `None`) removes `key`; false, changing nothing, if
    /// that would break `quota` or the key is empty or too long.
    pub fn set(&mut self, key: &str, value: Option<Vec<u8>>, quota: Quota) -> bool {
        if key.is_empty() || key.len() > MAX_KEY {
            return false;
        }
        let old = self.entries.get(key).map_or(0, |old| key.len() + old.len());
        let Some(value) = value else {
            if self.entries.remove(key).is_some() {
                self.bytes -= old;
            }
            return true;
        };
        let new = key.len() + value.len();
        if !self.entries.contains_key(key) && self.entries.len() >= quota.keys {
            return false;
        }
        if self.bytes - old + new > quota.bytes {
            return false;
        }
        self.bytes = self.bytes - old + new;
        self.entries.insert(key.to_owned(), value);
        true
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + self.bytes + 6 * self.entries.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        for (key, value) in &self.entries {
            out.extend_from_slice(&(key.len() as u16).to_le_bytes());
            out.extend_from_slice(key.as_bytes());
            out.extend_from_slice(&(value.len() as u32).to_le_bytes());
            out.extend_from_slice(value);
        }
        out
    }

    /// Reads [`Self::encode`]'s format, refusing anything malformed or
    /// past `quota`.
    pub fn decode(mut bytes: &[u8], quota: Quota) -> Result<Self, String> {
        fn take<'a>(bytes: &mut &'a [u8], n: usize) -> Result<&'a [u8], String> {
            if bytes.len() < n {
                return Err("truncated".into());
            }
            let (head, rest) = bytes.split_at(n);
            *bytes = rest;
            Ok(head)
        }
        if take(&mut bytes, 4)? != MAGIC {
            return Err("not a plugin storage file".into());
        }
        let count = u32::from_le_bytes(take(&mut bytes, 4)?.try_into().unwrap()) as usize;
        if count > quota.keys {
            return Err(format!("{count} keys is over the {} key quota", quota.keys));
        }
        let mut storage = Storage::default();
        for _ in 0..count {
            let key_len = u16::from_le_bytes(take(&mut bytes, 2)?.try_into().unwrap()) as usize;
            let key = std::str::from_utf8(take(&mut bytes, key_len)?)
                .map_err(|_| "a key is not UTF-8".to_owned())?
                .to_owned();
            let value_len = u32::from_le_bytes(take(&mut bytes, 4)?.try_into().unwrap()) as usize;
            if value_len > quota.bytes {
                return Err("a value is over the byte quota".into());
            }
            let value = take(&mut bytes, value_len)?.to_vec();
            if storage.get(&key).is_some() || !storage.set(&key, Some(value), quota) {
                return Err(format!("entry {key:?} is duplicated or over the quota"));
            }
        }
        if !bytes.is_empty() {
            return Err("trailing bytes".into());
        }
        Ok(storage)
    }
}

/// Where plugin `id`'s store lives inside the plugin directory.
pub fn path(directory: &Path, id: &str) -> PathBuf {
    directory.join(id).join("storage.bin")
}

/// Reads a stored file; a missing file is an empty store.
pub fn read(path: &Path, quota: Quota) -> Result<Storage, String> {
    let shown = path.display();
    let limit = (8 + quota.bytes + 6 * quota.keys) as u64;
    match std::fs::metadata(path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Storage::default()),
        Err(err) => return Err(format!("cannot read {shown}: {err}")),
        Ok(meta) if meta.len() > limit => {
            return Err(format!("{shown} is over the storage quota"));
        }
        Ok(_) => {}
    }
    let bytes = std::fs::read(path).map_err(|err| format!("cannot read {shown}: {err}"))?;
    Storage::decode(&bytes, quota).map_err(|err| format!("{shown}: {err}"))
}

/// Writes `storage` to `path` through a temporary file, or removes the
/// file when the store is empty.
pub fn write(path: &Path, storage: &Storage) -> std::io::Result<()> {
    if storage.is_empty() {
        return match std::fs::remove_file(path) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        };
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("bin.tmp");
    std::fs::write(&temporary, storage.encode())?;
    std::fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn quotas_hold_and_the_format_round_trips() {
        let quota = Quota { bytes: 16, keys: 2 };
        let mut storage = Storage::default();
        assert!(storage.set("a", Some(vec![1, 2, 3]), quota));
        assert!(storage.set("b", Some(vec![4]), quota));
        assert!(!storage.set("c", Some(vec![]), quota), "key quota");
        assert!(!storage.set("a", Some(vec![0; 16]), quota), "byte quota");
        assert!(storage.set("a", Some(vec![9; 10]), quota));
        assert_eq!(storage.bytes(), 13);
        assert!(!storage.set("", Some(vec![]), quota));
        assert!(!storage.set(&"k".repeat(MAX_KEY + 1), None, quota));
        let decoded = Storage::decode(&storage.encode(), quota).unwrap();
        assert_eq!(decoded, storage);
        assert!(storage.set("a", None, quota));
        assert_eq!(storage.bytes(), 2);
        assert_eq!(storage.get("b"), Some(&[4][..]));
    }

    #[tuff_core::watchdog]
    #[test]
    fn hostile_files_are_refused() {
        let quota = Quota { bytes: 16, keys: 2 };
        let mut storage = Storage::default();
        storage.set("a", Some(vec![1; 4]), quota);
        let good = storage.encode();
        for cut in 0..good.len() {
            assert!(
                Storage::decode(&good[..cut], quota).is_err(),
                "cut at {cut}"
            );
        }
        let mut trailing = good.clone();
        trailing.push(0);
        assert!(Storage::decode(&trailing, quota).is_err());
        let mut many = good.clone();
        many[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Storage::decode(&many, quota).is_err());
        let mut huge = MAGIC.to_vec();
        huge.extend_from_slice(&1u32.to_le_bytes());
        huge.extend_from_slice(&1u16.to_le_bytes());
        huge.push(b'a');
        huge.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(Storage::decode(&huge, quota).is_err());
        let mut twice = MAGIC.to_vec();
        twice.extend_from_slice(&2u32.to_le_bytes());
        for _ in 0..2 {
            twice.extend_from_slice(&1u16.to_le_bytes());
            twice.push(b'a');
            twice.extend_from_slice(&0u32.to_le_bytes());
        }
        assert!(Storage::decode(&twice, quota).is_err());
        assert!(Storage::decode(b"XXXX\0\0\0\0", quota).is_err());
    }
}
