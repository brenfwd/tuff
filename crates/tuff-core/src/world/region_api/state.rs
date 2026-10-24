use std::any::{Any, TypeId};
use std::collections::HashMap;

/// Per-region state features keep, one value per type.
#[derive(Default)]
pub struct RegionState {
    values: HashMap<TypeId, Box<dyn Any + Send>>,
}

impl RegionState {
    /// The value of type `T`, created with its default on first use.
    pub fn get<T: Default + Send + 'static>(&mut self) -> &mut T {
        self.values
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Box::new(T::default()))
            .downcast_mut::<T>()
            .expect("keyed by its own TypeId")
    }

    /// The value of type `T` if one was created.
    pub fn peek<T: Send + 'static>(&self) -> Option<&T> {
        self.values.get(&TypeId::of::<T>())?.downcast_ref::<T>()
    }
}

/// A region's feature state shared by the entities ticking in parallel:
/// each `get` holds the region state until its guard drops. Holding one
/// guard while asking for another on the same thread panics instead of
/// deadlocking, so take what you need and drop the guard first.
#[derive(Default)]
pub struct RegionStateLock {
    state: std::sync::Mutex<RegionState>,
    holder: std::sync::atomic::AtomicUsize,
}

fn thread_token() -> usize {
    thread_local!(static TOKEN: u8 = const { 0 });
    TOKEN.with(|token| std::ptr::from_ref(token) as usize)
}

impl RegionStateLock {
    pub fn new(state: RegionState) -> Self {
        Self {
            state: std::sync::Mutex::new(state),
            holder: Default::default(),
        }
    }

    pub fn into_inner(self) -> RegionState {
        self.state
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The value of type `T`, created with its default on first use.
    pub fn get<T: Default + Send + 'static>(&self) -> RegionStateGuard<'_, T> {
        use std::sync::atomic::Ordering;
        let me = thread_token();
        assert_ne!(
            self.holder.load(Ordering::Relaxed),
            me,
            "region state asked for while this thread still holds it"
        );
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.get::<T>();
        self.holder.store(me, Ordering::Relaxed);
        RegionStateGuard {
            state,
            holder: &self.holder,
            kind: std::marker::PhantomData,
        }
    }

    /// Runs `f` with the whole region state held.
    pub fn with<R>(&self, f: impl FnOnce(&mut RegionState) -> R) -> R {
        use std::sync::atomic::Ordering;
        let me = thread_token();
        assert_ne!(
            self.holder.load(Ordering::Relaxed),
            me,
            "region state asked for while this thread still holds it"
        );
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut state)
    }

    /// The value of type `T` if one was created, copied out.
    pub fn peek<T: Clone + Send + 'static>(&self) -> Option<T> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.peek::<T>().cloned()
    }
}

/// One value of a locked region state; see `RegionStateLock::get`.
pub struct RegionStateGuard<'a, T> {
    state: std::sync::MutexGuard<'a, RegionState>,
    holder: &'a std::sync::atomic::AtomicUsize,
    kind: std::marker::PhantomData<T>,
}

impl<T: Send + 'static> std::ops::Deref for RegionStateGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.state.peek::<T>().expect("created by get")
    }
}

impl<T: Default + Send + 'static> std::ops::DerefMut for RegionStateGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.state.get::<T>()
    }
}

impl<T> Drop for RegionStateGuard<'_, T> {
    fn drop(&mut self) {
        self.holder.store(0, std::sync::atomic::Ordering::Relaxed);
    }
}
