//! Immutable data that is either compiled in or reference counted, so the
//! same shapes describe the built-in data pack and data a pack loads at
//! runtime, which is dropped once nothing refers to it.

use std::borrow::Borrow;
use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::sync::Arc;

pub enum Frozen<T: ?Sized + 'static> {
    Static(&'static T),
    Shared(Arc<T>),
}

impl<T> Frozen<T> {
    pub fn new(value: T) -> Self {
        Self::Shared(Arc::new(value))
    }
}

impl<T: ?Sized> Frozen<T> {
    /// The compiled-in data, if this is not runtime data.
    pub const fn as_static(&self) -> Option<&'static T> {
        match self {
            Self::Static(value) => Some(value),
            Self::Shared(_) => None,
        }
    }

    /// Whether both point at the same data.
    pub fn ptr_eq(a: &Self, b: &Self) -> bool {
        std::ptr::addr_eq(&**a as *const T, &**b as *const T)
    }
}

impl<T: ?Sized> Deref for Frozen<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        match self {
            Self::Static(value) => value,
            Self::Shared(value) => value,
        }
    }
}

impl<T: ?Sized> Clone for Frozen<T> {
    #[inline]
    fn clone(&self) -> Self {
        match self {
            Self::Static(value) => Self::Static(value),
            Self::Shared(value) => Self::Shared(value.clone()),
        }
    }
}

impl<T: ?Sized> AsRef<T> for Frozen<T> {
    fn as_ref(&self) -> &T {
        self
    }
}

impl<T: ?Sized> Borrow<T> for Frozen<T> {
    fn borrow(&self) -> &T {
        self
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for Frozen<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (**self).fmt(f)
    }
}

impl<T: ?Sized + fmt::Display> fmt::Display for Frozen<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (**self).fmt(f)
    }
}

impl<T: ?Sized + PartialEq> PartialEq for Frozen<T> {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl<T: ?Sized + Eq> Eq for Frozen<T> {}

impl<T: ?Sized + PartialOrd> PartialOrd for Frozen<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        (**self).partial_cmp(&**other)
    }
}

impl<T: ?Sized + Ord> Ord for Frozen<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        (**self).cmp(&**other)
    }
}

impl<T: ?Sized + Hash> Hash for Frozen<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (**self).hash(state)
    }
}

impl PartialEq<str> for Frozen<str> {
    fn eq(&self, other: &str) -> bool {
        &**self == other
    }
}

impl PartialEq<&str> for Frozen<str> {
    fn eq(&self, other: &&str) -> bool {
        &**self == *other
    }
}

impl PartialEq<Frozen<str>> for str {
    fn eq(&self, other: &Frozen<str>) -> bool {
        self == &**other
    }
}

impl PartialEq<Frozen<str>> for &str {
    fn eq(&self, other: &Frozen<str>) -> bool {
        *self == &**other
    }
}

impl<T: ?Sized> From<&'static T> for Frozen<T> {
    fn from(value: &'static T) -> Self {
        Self::Static(value)
    }
}

impl<T: ?Sized> From<Arc<T>> for Frozen<T> {
    fn from(value: Arc<T>) -> Self {
        Self::Shared(value)
    }
}

impl From<String> for Frozen<str> {
    fn from(value: String) -> Self {
        Self::Shared(value.into())
    }
}

impl<T> From<Vec<T>> for Frozen<[T]> {
    fn from(value: Vec<T>) -> Self {
        Self::Shared(value.into())
    }
}

impl<T> FromIterator<T> for Frozen<[T]> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self::Shared(iter.into_iter().collect())
    }
}

impl<T> Default for Frozen<[T]> {
    fn default() -> Self {
        Self::Static(&[])
    }
}

impl Default for Frozen<str> {
    fn default() -> Self {
        Self::Static("")
    }
}

impl<'a, T> IntoIterator for &'a Frozen<[T]> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_and_shared_compare_by_value() {
        let shared: Frozen<str> = String::from("a:b").into();
        assert_eq!(shared, Frozen::Static("a:b"));
        assert_eq!(shared, "a:b");
        assert!(shared.as_static().is_none());
        let list: Frozen<[u8]> = vec![1, 2].into();
        assert_eq!(&*list, &[1, 2]);
        assert!(Frozen::ptr_eq(&list, &list.clone()));
    }

    #[test]
    fn the_last_clone_frees_shared_data() {
        let data = Arc::new(5);
        let frozen = Frozen::Shared(data.clone());
        let copy = frozen.clone();
        assert_eq!(Arc::strong_count(&data), 3);
        drop((frozen, copy));
        assert_eq!(Arc::strong_count(&data), 1);
    }
}
