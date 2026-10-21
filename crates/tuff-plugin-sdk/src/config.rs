// SPDX-License-Identifier: MIT
//! Typed settings. Declare each one as a `static` and [`Setting::declare`]
//! it while registering; owners edit it as `plugins.<id>.<key>`:
//!
//! ```ignore
//! use tuff_plugin_sdk::config::Setting;
//!
//! static GREETING: Setting<String> = Setting::new("greeting", "Hello", "What /hello says");
//! static LIMIT: Setting<i64> = Setting::new("limit", 10, "Greetings per player");
//!
//! GREETING.declare()?;
//! GREETING.on_change(|text| broadcast(&format!("now saying {text}")))?;
//! let text: String = GREETING.get();
//! ```

use crate::bindings::tuff::plugin::host;
use crate::events::{self, ev};
use crate::types::{ConfigDecl, ConfigValue, RegisterError};

/// A Rust type a setting holds: `bool`, `i64` or `String`.
pub trait ConfigType: Sized + 'static {
    /// How a `static` spells the default (`&'static str` for `String`).
    type Default: Copy + Send + Sync + 'static;

    fn from_default(default: Self::Default) -> Self;

    fn into_value(self) -> ConfigValue;

    /// `None` when `value` is of another kind.
    fn from_value(value: ConfigValue) -> Option<Self>;
}

impl ConfigType for bool {
    type Default = bool;

    fn from_default(default: bool) -> Self {
        default
    }

    fn into_value(self) -> ConfigValue {
        ConfigValue::Boolean(self)
    }

    fn from_value(value: ConfigValue) -> Option<Self> {
        match value {
            ConfigValue::Boolean(b) => Some(b),
            _ => None,
        }
    }
}

impl ConfigType for i64 {
    type Default = i64;

    fn from_default(default: i64) -> Self {
        default
    }

    fn into_value(self) -> ConfigValue {
        ConfigValue::Integer(self)
    }

    fn from_value(value: ConfigValue) -> Option<Self> {
        match value {
            ConfigValue::Integer(i) => Some(i),
            _ => None,
        }
    }
}

impl ConfigType for String {
    type Default = &'static str;

    fn from_default(default: &'static str) -> Self {
        default.to_owned()
    }

    fn into_value(self) -> ConfigValue {
        ConfigValue::Text(self)
    }

    fn from_value(value: ConfigValue) -> Option<Self> {
        match value {
            ConfigValue::Text(t) => Some(t),
            _ => None,
        }
    }
}

/// One setting of type `T`.
pub struct Setting<T: ConfigType> {
    key: &'static str,
    default: T::Default,
    description: &'static str,
    secret: bool,
    text_component: bool,
}

impl<T: ConfigType> Setting<T> {
    pub const fn new(key: &'static str, default: T::Default, description: &'static str) -> Self {
        Self {
            key,
            default,
            description,
            secret: false,
            text_component: false,
        }
    }

    /// Hides the value from the panel and command output (a token, a
    /// password).
    pub const fn secret(mut self) -> Self {
        self.secret = true;
        self
    }

    /// Marks a text setting as a MiniMessage text component (a message,
    /// a kick reason), edited in the panel with a live preview.
    pub const fn text_component(mut self) -> Self {
        self.text_component = true;
        self
    }

    pub const fn key(&self) -> &'static str {
        self.key
    }

    pub fn default_value(&self) -> T {
        T::from_default(self.default)
    }

    /// Declares the setting. Only while registering.
    pub fn declare(&self) -> Result<(), RegisterError> {
        host::declare_config(&ConfigDecl {
            key: self.key.to_owned(),
            default: self.default_value().into_value(),
            description: self.description.to_owned(),
            secret: self.secret,
            text_component: self.text_component,
        })
    }

    /// The live value; the default when the host has none.
    pub fn get(&self) -> T {
        host::config_get(self.key)
            .and_then(T::from_value)
            .unwrap_or_else(|| self.default_value())
    }

    /// Sets and saves the value, as an owner would; the error says why not.
    pub fn set(&self, value: T) -> Result<(), crate::HostError> {
        host::config_set(self.key, &value.into_value()).map_err(Into::into)
    }

    /// Runs `handler` with each new value. Only while registering.
    pub fn on_change(&self, handler: impl Fn(T) + 'static) -> Result<(), RegisterError> {
        let key = self.key;
        events::listen::<ev::ConfigChange>()
            .when(move |c| c.key == key)
            .run(move |c| {
                if let Some(value) = T::from_value(c.value.clone()) {
                    handler(value);
                }
            })
    }
}
