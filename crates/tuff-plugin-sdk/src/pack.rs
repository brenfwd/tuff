// SPDX-License-Identifier: MIT
//! Resource packs offered to every player while the plugin is loaded.

use crate::{host, types};

pub use types::RegisterError;

/// Builder for a pack; register it with [`Pack::register`] while the plugin
/// registers.
#[derive(Clone, Debug)]
pub struct Pack {
    name: String,
    data: Vec<u8>,
    required: bool,
    prompt: Option<String>,
}

impl Pack {
    /// `name` is 1 to 32 of `a-z 0-9 - _`; `zip` is the archive with a
    /// `pack.mcmeta` at its root.
    pub fn new(name: &str, zip: impl Into<Vec<u8>>) -> Self {
        Self {
            name: name.to_owned(),
            data: zip.into(),
            required: false,
            prompt: None,
        }
    }

    /// Players who decline are disconnected.
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    /// Plain text shown with the offer.
    pub fn prompt(mut self, text: &str) -> Self {
        self.prompt = Some(text.to_owned());
        self
    }

    pub fn register(self) -> Result<(), RegisterError> {
        host::register_resource_pack(&types::ResourcePack {
            name: self.name,
            data: self.data,
            required: self.required,
            prompt: self.prompt,
        })
    }
}
