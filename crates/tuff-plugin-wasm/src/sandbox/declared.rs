//! Settings and resource packs a plugin declares.

use super::*;

/// A setting a plugin declared, by its key without the plugin's prefix.
#[derive(Clone, Debug, PartialEq)]
pub struct Setting {
    pub key: String,
    pub default: tuff_core::config::Value,
    pub description: String,
    pub secret: bool,
    pub text_component: bool,
}

impl Setting {
    /// The server setting it is stored as.
    pub fn to_key(&self, plugin: &str) -> tuff_core::config::Key {
        let key = tuff_core::config::Key::new(
            &abi::setting_name(plugin, &self.key),
            self.default.clone(),
            &self.description,
        );
        let key = if self.secret { key.secret() } else { key };
        if self.text_component {
            key.text_component()
        } else {
            key
        }
    }
}

/// A setting's value as the guest sees it.
pub fn config_value(value: tuff_core::config::Value) -> ConfigValue {
    use tuff_core::config::Value;
    match value {
        Value::Bool(b) => ConfigValue::Boolean(b),
        Value::Int(i) => ConfigValue::Integer(i),
        Value::String(text) | Value::Url(text) => ConfigValue::Text(text),
        other => ConfigValue::Text(other.to_string()),
    }
}

pub(super) fn setting_value(value: ConfigValue) -> tuff_core::config::Value {
    use tuff_core::config::Value;
    match value {
        ConfigValue::Boolean(b) => Value::Bool(b),
        ConfigValue::Integer(i) => Value::Int(i),
        ConfigValue::Text(text) => Value::String(text),
    }
}

/// A resource pack a plugin offers while it is loaded.
#[derive(Clone, Debug, PartialEq)]
pub struct Pack {
    pub name: String,
    pub data: bytes::Bytes,
    pub required: bool,
    pub prompt: Option<String>,
}

impl Pack {
    /// The pack's id: the same for the same plugin and name on every run,
    /// so clients keep their cached copy.
    pub fn id(&self, plugin: &str) -> u128 {
        let mut hash: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
        for byte in plugin.bytes().chain([0]).chain(self.name.bytes()) {
            hash ^= u128::from(byte);
            hash = hash.wrapping_mul(0x0000_0000_0100_0000_0000_0000_0000_013b);
        }
        hash
    }
}
