// SPDX-License-Identifier: MIT
use std::cell::RefCell;
use std::thread::LocalKey;

/// In-memory plugin state, reset when the plugin reloads. Declare with
/// [`plugin_state!`](crate::plugin_state) and access through [`State::with`].
pub struct State<T: 'static>(pub &'static LocalKey<RefCell<T>>);

impl<T> Clone for State<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for State<T> {}

impl<T> State<T> {
    /// Runs `f` with exclusive access. Do not call `with` on the same state
    /// from inside `f`.
    pub fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        self.0.with(|cell| f(&mut cell.borrow_mut()))
    }
}

/// `plugin_state!(NAME: Type = init;)` declares a `static NAME: State<Type>`.
#[macro_export]
macro_rules! plugin_state {
    ($(#[$m:meta])* $vis:vis $name:ident: $ty:ty = $init:expr) => {
        $vis static $name: $crate::state::State<$ty> = {
            ::std::thread_local! {
                static CELL: ::std::cell::RefCell<$ty> = ::std::cell::RefCell::new($init);
            }
            $crate::state::State(&CELL)
        };
    };
}

#[cfg(test)]
mod tests {
    plugin_state!(COUNT: u32 = 5);

    #[tuff_core::watchdog]
    #[test]
    fn state_persists_across_calls() {
        COUNT.with(|c| *c += 1);
        assert_eq!(COUNT.with(|c| *c), 6);
    }
}
