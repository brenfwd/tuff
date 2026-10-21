// SPDX-License-Identifier: MIT
//! Binary codec for plugin interface payloads. Payloads come from other
//! plugins, so decoding is hostile-input safe: every length is checked
//! against the remaining bytes before allocating and nothing panics.
//!
//! Layout: little-endian fixed-width ints and floats, bool as one byte
//! (0 or 1), char as a u32 scalar, string and list as a LEB128 u32 length
//! plus items, option/result/variant/enum as a LEB128 discriminant plus
//! payload, record and tuple fields in order, flags as a u64 bitset.

use std::fmt;

/// Zero-sized list elements are capped to this many per list.
pub const MAX_ZERO_SIZED_ITEMS: usize = 1 << 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireError {
    Eof,
    BadLength,
    BadDiscriminant,
    BadUtf8,
    BadBool,
    BadChar,
    BadFlags,
    BadVarint,
    Trailing,
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            WireError::Eof => "unexpected end of payload",
            WireError::BadLength => "declared length exceeds payload",
            WireError::BadDiscriminant => "invalid discriminant",
            WireError::BadUtf8 => "invalid utf-8",
            WireError::BadBool => "invalid bool",
            WireError::BadChar => "invalid char",
            WireError::BadFlags => "unknown flag bits",
            WireError::BadVarint => "malformed varint",
            WireError::Trailing => "trailing bytes",
        };
        f.write_str(text)
    }
}

impl std::error::Error for WireError {}

pub trait Wire: Sized {
    /// The fewest bytes any encoding of this type takes.
    const MIN_SIZE: usize;

    fn encode(&self, out: &mut Vec<u8>);

    fn decode(input: &mut &[u8]) -> Result<Self, WireError>;
}

pub fn encode_to_vec<T: Wire>(value: &T) -> Vec<u8> {
    let mut out = Vec::new();
    value.encode(&mut out);
    out
}

/// Decodes one value and rejects trailing bytes.
pub fn decode_all<T: Wire>(mut input: &[u8]) -> Result<T, WireError> {
    let value = T::decode(&mut input)?;
    if input.is_empty() {
        Ok(value)
    } else {
        Err(WireError::Trailing)
    }
}

pub fn write_leb(out: &mut Vec<u8>, mut value: u32) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

pub fn read_leb(input: &mut &[u8]) -> Result<u32, WireError> {
    let mut value = 0u32;
    for index in 0..5 {
        let byte = take(input, 1)?[0];
        let bits = u32::from(byte & 0x7f);
        if index == 4 && bits > 0x0f {
            return Err(WireError::BadVarint);
        }
        value |= bits << (7 * index);
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(WireError::BadVarint)
}

pub fn take<'a>(input: &mut &'a [u8], n: usize) -> Result<&'a [u8], WireError> {
    if input.len() < n {
        return Err(WireError::Eof);
    }
    let (head, rest) = input.split_at(n);
    *input = rest;
    Ok(head)
}

/// Reads a list length and checks it against what the input can hold.
pub fn read_len(input: &mut &[u8], min_item: usize) -> Result<usize, WireError> {
    let len = read_leb(input)? as usize;
    if min_item == 0 {
        if len > MAX_ZERO_SIZED_ITEMS {
            return Err(WireError::BadLength);
        }
    } else if len > input.len() / min_item {
        return Err(WireError::BadLength);
    }
    Ok(len)
}

/// Reads a discriminant below `count`.
pub fn read_case(input: &mut &[u8], count: u32) -> Result<u32, WireError> {
    let case = read_leb(input)?;
    if case < count {
        Ok(case)
    } else {
        Err(WireError::BadDiscriminant)
    }
}

macro_rules! int_wire {
    ($($t:ty),*) => {$(
        impl Wire for $t {
            const MIN_SIZE: usize = std::mem::size_of::<$t>();
            fn encode(&self, out: &mut Vec<u8>) {
                out.extend_from_slice(&self.to_le_bytes());
            }
            fn decode(input: &mut &[u8]) -> Result<Self, WireError> {
                let bytes = take(input, std::mem::size_of::<$t>())?;
                Ok(<$t>::from_le_bytes(bytes.try_into().map_err(|_| WireError::Eof)?))
            }
        }
    )*};
}

int_wire!(u8, i8, u16, i16, u32, i32, u64, i64, f32, f64);

impl Wire for bool {
    const MIN_SIZE: usize = 1;
    fn encode(&self, out: &mut Vec<u8>) {
        out.push(u8::from(*self));
    }
    fn decode(input: &mut &[u8]) -> Result<Self, WireError> {
        match take(input, 1)?[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(WireError::BadBool),
        }
    }
}

impl Wire for char {
    const MIN_SIZE: usize = 4;
    fn encode(&self, out: &mut Vec<u8>) {
        u32::from(*self).encode(out);
    }
    fn decode(input: &mut &[u8]) -> Result<Self, WireError> {
        char::from_u32(u32::decode(input)?).ok_or(WireError::BadChar)
    }
}

impl Wire for () {
    const MIN_SIZE: usize = 0;
    fn encode(&self, _: &mut Vec<u8>) {}
    fn decode(_: &mut &[u8]) -> Result<Self, WireError> {
        Ok(())
    }
}

impl Wire for String {
    const MIN_SIZE: usize = 1;
    fn encode(&self, out: &mut Vec<u8>) {
        write_leb(out, self.len() as u32);
        out.extend_from_slice(self.as_bytes());
    }
    fn decode(input: &mut &[u8]) -> Result<Self, WireError> {
        let len = read_len(input, 1)?;
        let bytes = take(input, len)?;
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| WireError::BadUtf8)
    }
}

impl<T: Wire> Wire for Vec<T> {
    const MIN_SIZE: usize = 1;
    fn encode(&self, out: &mut Vec<u8>) {
        write_leb(out, self.len() as u32);
        for item in self {
            item.encode(out);
        }
    }
    fn decode(input: &mut &[u8]) -> Result<Self, WireError> {
        let len = read_len(input, T::MIN_SIZE)?;
        let mut items = Vec::with_capacity(len);
        for _ in 0..len {
            items.push(T::decode(input)?);
        }
        Ok(items)
    }
}

impl<T: Wire> Wire for Option<T> {
    const MIN_SIZE: usize = 1;
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            None => write_leb(out, 0),
            Some(value) => {
                write_leb(out, 1);
                value.encode(out);
            }
        }
    }
    fn decode(input: &mut &[u8]) -> Result<Self, WireError> {
        match read_case(input, 2)? {
            0 => Ok(None),
            _ => Ok(Some(T::decode(input)?)),
        }
    }
}

impl<T: Wire, E: Wire> Wire for Result<T, E> {
    const MIN_SIZE: usize = 1;
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Ok(value) => {
                write_leb(out, 0);
                value.encode(out);
            }
            Err(error) => {
                write_leb(out, 1);
                error.encode(out);
            }
        }
    }
    fn decode(input: &mut &[u8]) -> Result<Self, WireError> {
        match read_case(input, 2)? {
            0 => Ok(Ok(T::decode(input)?)),
            _ => Ok(Err(E::decode(input)?)),
        }
    }
}

macro_rules! tuple_wire {
    ($(($($name:ident $idx:tt),+))*) => {$(
        impl<$($name: Wire),+> Wire for ($($name,)+) {
            const MIN_SIZE: usize = 0 $(+ $name::MIN_SIZE)+;
            fn encode(&self, out: &mut Vec<u8>) {
                $(self.$idx.encode(out);)+
            }
            fn decode(input: &mut &[u8]) -> Result<Self, WireError> {
                Ok(($($name::decode(input)?,)+))
            }
        }
    )*};
}

tuple_wire! {
    (A 0)
    (A 0, B 1)
    (A 0, B 1, C 2)
    (A 0, B 1, C 2, D 3)
    (A 0, B 1, C 2, D 3, E 4)
    (A 0, B 1, C 2, D 3, E 4, F 5)
}

use std::cell::RefCell;
use std::rc::Rc;

use crate::messaging::{self, CallError, Handler, Target};

/// A failed call to a method whose WIT result carries an error type.
#[derive(Debug, Clone, PartialEq)]
pub enum Error<E> {
    Call(CallError),
    App(E),
}

fn bad_reply(error: WireError) -> CallError {
    CallError::Refused(format!("bad reply: {error}"))
}

/// Sends a request and decodes the answer.
pub async fn call<R: Wire>(
    target: &Target,
    interface: &str,
    method: u64,
    args: Vec<u8>,
) -> Result<R, CallError> {
    let bytes = messaging::request(target.clone(), interface, method, args).await?;
    decode_all(&bytes).map_err(bad_reply)
}

/// Like [`call`] for a method returning `result<T, E>`.
pub async fn call_result<T: Wire, E: Wire>(
    target: &Target,
    interface: &str,
    method: u64,
    args: Vec<u8>,
) -> Result<T, Error<E>> {
    match call::<Result<T, E>>(target, interface, method, args).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(Error::App(error)),
        Err(error) => Err(Error::Call(error)),
    }
}

/// One-way send; returns how many deliveries were queued.
pub fn tell(
    target: &Target,
    interface: &str,
    method: u64,
    args: Vec<u8>,
) -> Result<u32, CallError> {
    messaging::notify(target.clone(), interface, method, &args)
}

/// Adapts a typed method into a route handler: decodes the arguments,
/// refuses malformed payloads, and answers with the encoded return value.
pub fn method<S: 'static, A: Wire, R: Wire>(
    imp: &Rc<RefCell<S>>,
    f: impl Fn(&mut S, &str, A) -> R + 'static,
) -> Handler {
    let imp = imp.clone();
    messaging::handler(move |msg, reply| {
        let args = match decode_all::<A>(&msg.payload) {
            Ok(args) => args,
            Err(error) => return reply.err(format!("bad payload: {error}")),
        };
        match imp.try_borrow_mut() {
            Ok(mut imp) => reply.ok(encode_to_vec(&f(&mut imp, &msg.sender, args))),
            Err(_) => reply.err("busy"),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round<T: Wire + PartialEq + fmt::Debug>(value: T) {
        let bytes = encode_to_vec(&value);
        assert!(bytes.len() >= T::MIN_SIZE);
        assert_eq!(decode_all::<T>(&bytes).unwrap(), value);
    }

    #[tuff_core::watchdog]
    #[test]
    fn every_shape_round_trips() {
        round(0xabu8);
        round(-5i8);
        round(60000u16);
        round(-3i16);
        round(u32::MAX);
        round(-9i32);
        round(u64::MAX);
        round(i64::MIN);
        round(1.5f32);
        round(-2.25f64);
        round(true);
        round('x');
        round('\u{1f600}');
        round(());
        round(String::from("h\u{e9}llo"));
        round(vec![1u32, 2, 3]);
        round(vec![vec![String::from("a")], vec![]]);
        round(Some(7u8));
        round(None::<u8>);
        round(Ok::<u8, String>(1));
        round(Err::<u8, String>("no".into()));
        round((1u8, String::from("t"), Some(2i64)));
        round(vec![(); 10]);
        round(u32::MAX as usize as u32);
    }

    #[tuff_core::watchdog]
    #[test]
    fn leb_is_canonical_and_bounded() {
        let mut out = Vec::new();
        write_leb(&mut out, 300);
        assert_eq!(out, [0xac, 0x02]);
        write_leb(&mut out, u32::MAX);
        let mut input = &out[2..];
        assert_eq!(read_leb(&mut input), Ok(u32::MAX));
        assert_eq!(
            read_leb(&mut &[0xff, 0xff, 0xff, 0xff, 0x1f][..]),
            Err(WireError::BadVarint)
        );
        assert_eq!(
            read_leb(&mut &[0x80, 0x80, 0x80, 0x80, 0x80, 0x01][..]),
            Err(WireError::BadVarint)
        );
        assert_eq!(read_leb(&mut &[0x80][..]), Err(WireError::Eof));
    }

    #[tuff_core::watchdog]
    #[test]
    fn truncated_input_fails_at_every_prefix() {
        let value = (String::from("abc"), vec![1u32, 2], Some(9u64));
        let bytes = encode_to_vec(&value);
        for cut in 0..bytes.len() {
            assert!(decode_all::<(String, Vec<u32>, Option<u64>)>(&bytes[..cut]).is_err());
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn declared_lengths_beyond_input_fail_without_allocating() {
        let mut huge = Vec::new();
        write_leb(&mut huge, u32::MAX);
        assert_eq!(decode_all::<Vec<u64>>(&huge), Err(WireError::BadLength));
        assert_eq!(decode_all::<String>(&huge), Err(WireError::BadLength));
        assert_eq!(decode_all::<Vec<()>>(&huge), Err(WireError::BadLength));
        let mut ok = Vec::new();
        write_leb(&mut ok, MAX_ZERO_SIZED_ITEMS as u32);
        assert_eq!(
            decode_all::<Vec<()>>(&ok).unwrap().len(),
            MAX_ZERO_SIZED_ITEMS
        );
        let mut nested = Vec::new();
        write_leb(&mut nested, 1 << 20);
        assert_eq!(
            decode_all::<Vec<Vec<Vec<u8>>>>(&nested),
            Err(WireError::BadLength)
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn malformed_values_fail() {
        assert_eq!(decode_all::<bool>(&[2]), Err(WireError::BadBool));
        assert_eq!(
            decode_all::<Option<u8>>(&[2]),
            Err(WireError::BadDiscriminant)
        );
        assert_eq!(
            decode_all::<Result<u8, u8>>(&[5, 0]),
            Err(WireError::BadDiscriminant)
        );
        assert_eq!(
            decode_all::<String>(&[2, 0xff, 0xfe]),
            Err(WireError::BadUtf8)
        );
        assert_eq!(
            decode_all::<char>(&0xd800u32.to_le_bytes()),
            Err(WireError::BadChar)
        );
        assert_eq!(decode_all::<u8>(&[1, 2]), Err(WireError::Trailing));
        assert_eq!(decode_all::<u32>(&[1]), Err(WireError::Eof));
    }

    #[tuff_core::watchdog]
    #[test]
    fn deep_option_nesting_is_bounded_by_input() {
        let bytes = vec![1u8; 10_000];
        type Deep = Option<Option<Option<Option<u8>>>>;
        assert!(decode_all::<Deep>(&bytes).is_err());
        assert!(decode_all::<Deep>(&[1, 1, 1, 1, 7]).is_ok());
    }
}
