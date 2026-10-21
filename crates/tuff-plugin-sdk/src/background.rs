// SPDX-License-Identifier: MIT
//! Pure computation off the tick thread, as a future. [`run`] starts
//! [`Plugin::background`](crate::Plugin::background) with a task name and
//! input bytes on a separate instance of this module, where every host
//! call traps, and resolves with the bytes it returns:
//!
//! ```ignore
//! impl Plugin for Mine {
//!     fn background(task: &str, input: &[u8]) -> Result<Vec<u8>, String> {
//!         match task {
//!             "sum" => Ok(input.iter().map(|b| u64::from(*b)).sum::<u64>().to_le_bytes().to_vec()),
//!             _ => Err(format!("no task {task}")),
//!         }
//!     }
//! }
//!
//! spawn(async {
//!     let sum = background::run("sum", &[1, 2, 3]).await;
//! });
//! ```
//!
//! Limits: 4 tasks queued or running per plugin, input and output at most
//! the message limit (256 KiB by default), each task gets one call's fuel
//! and memory, and tasks queued or running when the plugin unloads are
//! dropped. Dropping the future cancels the task. A task cannot read the
//! plugin's state: the instance is not the one running its callbacks.

use crate::bindings::tuff::plugin::background as host;
use crate::bindings::tuff::plugin::capability_types as cap;
use crate::exec::{Call, Completion};

/// Why a task produced no output.
#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// The host refused to start it (bad name, input too large, four
    /// tasks already in flight).
    Refused(cap::Denied),
    /// The task returned an error.
    Failed(String),
    /// The instance trapped, ran out of fuel or memory, or could not
    /// start.
    Faulted(String),
    /// The output was over the message limit.
    TooLarge,
}

impl From<cap::BackgroundError> for Error {
    fn from(error: cap::BackgroundError) -> Self {
        match error {
            cap::BackgroundError::Failed(why) => Error::Failed(why),
            cap::BackgroundError::Faulted(why) => Error::Faulted(why),
            cap::BackgroundError::TooLarge => Error::TooLarge,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Refused(why) => write!(f, "refused: {why:?}"),
            Error::Failed(why) => write!(f, "failed: {why}"),
            Error::Faulted(why) => write!(f, "faulted: {why}"),
            Error::TooLarge => f.write_str("the output was too large"),
        }
    }
}

impl std::error::Error for Error {}

pub(crate) async fn finish(handle: Result<u32, cap::Denied>) -> Result<Vec<u8>, Error> {
    let handle = handle.map_err(Error::Refused)?;
    match Call::background(handle).await {
        Completion::Background(outcome) => outcome.map_err(Error::from),
        _ => Err(Error::Faulted("mismatched completion".into())),
    }
}

/// Runs `task` with `input` and waits for its output.
pub async fn run(task: &str, input: &[u8]) -> Result<Vec<u8>, Error> {
    finish(host::run(task, input)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{spawn, testing};
    use std::cell::RefCell;
    use std::rc::Rc;

    type Slot = Rc<RefCell<Option<Result<Vec<u8>, Error>>>>;

    fn started(handle: u32) -> Slot {
        let slot = Rc::new(RefCell::new(None));
        let out = slot.clone();
        spawn(async move {
            *out.borrow_mut() = Some(finish(Ok(handle)).await);
        });
        slot
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_task_future_resolves_with_the_output_for_its_handle() {
        let result = started(0x8000_0001);
        assert!(result.borrow().is_none());
        assert!(testing::complete(0x8000_0001, Completion::Background(Ok(vec![1, 2]))).is_none());
        assert_eq!(result.borrow().clone(), Some(Ok(vec![1, 2])));
    }

    #[tuff_core::watchdog]
    #[test]
    fn task_errors_map_to_the_sdk_error() {
        let failed = started(0x8000_0002);
        let faulted = started(0x8000_0003);
        let large = started(0x8000_0004);
        testing::complete(
            0x8000_0002,
            Completion::Background(Err(cap::BackgroundError::Failed("no".into()))),
        );
        testing::complete(
            0x8000_0003,
            Completion::Background(Err(cap::BackgroundError::Faulted("trap".into()))),
        );
        testing::complete(
            0x8000_0004,
            Completion::Background(Err(cap::BackgroundError::TooLarge)),
        );
        assert_eq!(
            failed.borrow().clone(),
            Some(Err(Error::Failed("no".into())))
        );
        assert_eq!(
            faulted.borrow().clone(),
            Some(Err(Error::Faulted("trap".into())))
        );
        assert_eq!(large.borrow().clone(), Some(Err(Error::TooLarge)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_refused_start_resolves_at_once() {
        let slot = Rc::new(RefCell::new(None));
        let out = slot.clone();
        spawn(async move {
            let denied = cap::Denied::Quota("full".into());
            *out.borrow_mut() = Some(finish(Err(denied)).await);
        });
        assert_eq!(
            slot.borrow().clone(),
            Some(Err(Error::Refused(cap::Denied::Quota("full".into()))))
        );
    }
}
