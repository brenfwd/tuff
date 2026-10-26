//! Conversions between Rust types and [`Value`]s: the primitives and
//! containers here, the WIT world's own types in the generated
//! [`super::bindings`]. Lowering borrows (a string goes into guest memory
//! straight from the Rust value); lifting copies out of a borrowed value.

use std::borrow::Cow;

use super::canon::Value;

pub type Result<T> = std::result::Result<T, String>;

pub fn mismatch<T>(expected: &str) -> Result<T> {
    Err(format!("expected {expected}"))
}

pub trait FromValue: Sized {
    fn from_value(value: Value<'_>) -> Result<Self>;

    #[doc(hidden)]
    fn list_from_value(value: Value<'_>) -> Result<Vec<Self>> {
        match value {
            Value::List(items) => items.into_iter().map(Self::from_value).collect(),
            Value::Bytes(bytes) => bytes
                .iter()
                .map(|b| Self::from_value(Value::U8(*b)))
                .collect(),
            _ => mismatch("a list"),
        }
    }

    /// Whether this is `()`, which a WIT `result` or `option` carries as
    /// no payload.
    #[doc(hidden)]
    fn is_unit() -> bool {
        false
    }
}

pub trait ToValue {
    fn to_value(&self) -> Value<'_>;

    #[doc(hidden)]
    fn list_to_value(items: &[Self]) -> Value<'_>
    where
        Self: Sized,
    {
        Value::List(items.iter().map(Self::to_value).collect())
    }

    #[doc(hidden)]
    fn is_unit() -> bool
    where
        Self: Sized,
    {
        false
    }
}

macro_rules! scalar {
    ($($ty:ty => $case:ident,)*) => {$(
        impl FromValue for $ty {
            fn from_value(value: Value<'_>) -> Result<Self> {
                match value {
                    Value::$case(v) => Ok(v),
                    _ => mismatch(stringify!($ty)),
                }
            }
        }

        impl ToValue for $ty {
            fn to_value(&self) -> Value<'_> {
                Value::$case(*self)
            }
        }
    )*};
}

scalar! {
    bool => Bool,
    i8 => S8,
    i16 => S16,
    u16 => U16,
    i32 => S32,
    u32 => U32,
    i64 => S64,
    u64 => U64,
    f32 => F32,
    f64 => F64,
    char => Char,
}

impl FromValue for u8 {
    fn from_value(value: Value<'_>) -> Result<Self> {
        match value {
            Value::U8(v) => Ok(v),
            _ => mismatch("u8"),
        }
    }

    fn list_from_value(value: Value<'_>) -> Result<Vec<Self>> {
        match value {
            Value::Bytes(bytes) => Ok(bytes.into_owned()),
            Value::List(items) => items.into_iter().map(Self::from_value).collect(),
            _ => mismatch("a list"),
        }
    }
}

impl ToValue for u8 {
    fn to_value(&self) -> Value<'_> {
        Value::U8(*self)
    }

    fn list_to_value(items: &[Self]) -> Value<'_> {
        Value::Bytes(Cow::Borrowed(items))
    }
}

impl FromValue for String {
    fn from_value(value: Value<'_>) -> Result<Self> {
        match value {
            Value::String(text) => Ok(text.into_owned()),
            _ => mismatch("a string"),
        }
    }
}

impl ToValue for String {
    fn to_value(&self) -> Value<'_> {
        Value::String(Cow::Borrowed(self))
    }
}

impl ToValue for str {
    fn to_value(&self) -> Value<'_> {
        Value::String(Cow::Borrowed(self))
    }
}

impl FromValue for () {
    fn from_value(value: Value<'_>) -> Result<Self> {
        match value {
            Value::Record(fields) if fields.is_empty() => Ok(()),
            _ => mismatch("an empty tuple"),
        }
    }

    fn is_unit() -> bool {
        true
    }
}

impl ToValue for () {
    fn to_value(&self) -> Value<'_> {
        Value::Record(Vec::new())
    }

    fn is_unit() -> bool {
        true
    }
}

impl<T: FromValue> FromValue for Vec<T> {
    fn from_value(value: Value<'_>) -> Result<Self> {
        T::list_from_value(value)
    }
}

impl<T: ToValue> ToValue for Vec<T> {
    fn to_value(&self) -> Value<'_> {
        T::list_to_value(self)
    }
}

impl<T: ToValue> ToValue for [T] {
    fn to_value(&self) -> Value<'_> {
        T::list_to_value(self)
    }
}

/// The payload of a case whose type is `T`, absent for `()`.
fn payload<T: FromValue>(payload: Option<Box<Value<'_>>>) -> Result<T> {
    match payload {
        Some(value) => T::from_value(*value),
        None if T::is_unit() => T::from_value(Value::Record(Vec::new())),
        None => mismatch("a payload"),
    }
}

fn boxed<T: ToValue>(value: &T) -> Option<Box<Value<'_>>> {
    (!T::is_unit()).then(|| Box::new(value.to_value()))
}

impl<T: FromValue> FromValue for Option<T> {
    fn from_value(value: Value<'_>) -> Result<Self> {
        match value {
            Value::Variant(0, None) => Ok(None),
            Value::Variant(1, inner) => payload(inner).map(Some),
            _ => mismatch("an option"),
        }
    }
}

impl<T: ToValue> ToValue for Option<T> {
    fn to_value(&self) -> Value<'_> {
        match self {
            None => Value::Variant(0, None),
            Some(value) => Value::Variant(1, boxed(value)),
        }
    }
}

impl<T: FromValue, E: FromValue> FromValue for std::result::Result<T, E> {
    fn from_value(value: Value<'_>) -> Result<Self> {
        match value {
            Value::Variant(0, inner) => payload(inner).map(Ok),
            Value::Variant(1, inner) => payload(inner).map(Err),
            _ => mismatch("a result"),
        }
    }
}

impl<T: ToValue, E: ToValue> ToValue for std::result::Result<T, E> {
    fn to_value(&self) -> Value<'_> {
        match self {
            Ok(value) => Value::Variant(0, boxed(value)),
            Err(error) => Value::Variant(1, boxed(error)),
        }
    }
}

macro_rules! tuple {
    ($(($($name:ident),+))*) => {$(
        impl<$($name: FromValue),+> FromValue for ($($name,)+) {
            fn from_value(value: Value<'_>) -> Result<Self> {
                let Value::Record(fields) = value else {
                    return mismatch("a tuple");
                };
                let mut fields = fields.into_iter();
                let tuple = ($($name::from_value(fields.next().ok_or("tuple too short")?)?,)+);
                match fields.next() {
                    None => Ok(tuple),
                    Some(_) => mismatch("a shorter tuple"),
                }
            }
        }

        impl<$($name: ToValue),+> ToValue for ($($name,)+) {
            #[expect(non_snake_case, reason = "name mirrors the generated interface")]
            fn to_value(&self) -> Value<'_> {
                let ($($name,)+) = self;
                Value::Record(vec![$($name.to_value()),+])
            }
        }
    )*};
}

tuple! {
    (A)
    (A, B)
    (A, B, C)
    (A, B, C, D)
}

/// The fields of a record of `count` fields, in order.
pub fn record<'a>(
    value: Value<'a>,
    count: usize,
    what: &str,
) -> Result<std::vec::IntoIter<Value<'a>>> {
    match value {
        Value::Record(fields) if fields.len() == count => Ok(fields.into_iter()),
        _ => mismatch(what),
    }
}

/// The next of `fields`, which [`record`] has counted.
pub fn field<'a, T: FromValue>(fields: &mut std::vec::IntoIter<Value<'a>>) -> Result<T> {
    T::from_value(fields.next().ok_or("missing field")?)
}

/// A variant's case and payload.
pub fn variant<'a>(value: Value<'a>, what: &str) -> Result<(u32, Option<Box<Value<'a>>>)> {
    match value {
        Value::Variant(case, payload) => Ok((case, payload)),
        _ => mismatch(what),
    }
}

/// The payload of a variant case typed `T`.
pub fn case<T: FromValue>(payload: Option<Box<Value<'_>>>) -> Result<T> {
    match payload {
        Some(value) => T::from_value(*value),
        None => mismatch("a payload"),
    }
}

/// A flags value's bits.
pub fn flags(value: Value<'_>, what: &str) -> Result<u64> {
    match value {
        Value::Flags(bits) => Ok(bits),
        _ => mismatch(what),
    }
}

/// The next argument of an import.
pub fn arg<'a>(args: &mut std::vec::IntoIter<Value<'a>>) -> Result<Value<'a>> {
    args.next().ok_or_else(|| "missing argument".to_owned())
}

/// A `string` argument, borrowed from guest memory where it was lifted.
pub fn str_arg<'a>(value: Value<'a>) -> Result<Cow<'a, str>> {
    match value {
        Value::String(text) => Ok(text),
        _ => mismatch("a string"),
    }
}

/// A `list<u8>` argument, borrowed from guest memory where it was lifted.
pub fn bytes_arg<'a>(value: Value<'a>) -> Result<Cow<'a, [u8]>> {
    match value {
        Value::Bytes(bytes) => Ok(bytes),
        Value::List(items) => items
            .into_iter()
            .map(u8::from_value)
            .collect::<Result<Vec<u8>>>()
            .map(Cow::Owned),
        _ => mismatch("a byte list"),
    }
}
