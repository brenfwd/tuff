//! The capability sandbox shared by every network and file capability a
//! runtime plugin can use: manifest requests, owner grants, destination
//! checks before and after DNS, and per-plugin quotas. It is independent
//! of the guest ABI.

pub mod address;
pub mod classes;
pub mod grants;
pub mod pattern;
pub mod policy;
pub mod quota;
pub mod resolve;
pub mod size;
pub mod target;

#[cfg(test)]
mod tests;

pub use grants::{Grant, Grants};
pub use pattern::{Entry, Kind, Rule};
pub use policy::{Approval, Denied, Policy, Request};
pub use quota::{Limits, Usage};
pub use target::Target;
