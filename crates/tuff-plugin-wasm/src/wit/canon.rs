//! Lifting and lowering of the canonical ABI's synchronous subset over a
//! [`Layout`]. Everything read from guest memory is untrusted: pointers are
//! bounds- and alignment-checked, discriminants and chars validated, strings
//! must be UTF-8, and every string, list and the call as a whole are capped
//! by a [`Budget`] before anything is allocated.

use std::borrow::Cow;
use std::fmt;

use super::schema::{Flat, Layout, Shape};

/// A value of any supported WIT type. Records and tuples are their fields
/// in order; enums, options and results are variants (option: 0 `none`,
/// 1 `some`; result: 0 `ok`, 1 `err`). Strings and byte lists lifted from
/// guest memory borrow it.
#[derive(Clone, Debug, PartialEq)]
pub enum Value<'a> {
    Bool(bool),
    S8(i8),
    U8(u8),
    S16(i16),
    U16(u16),
    S32(i32),
    U32(u32),
    S64(i64),
    U64(u64),
    F32(f32),
    F64(f64),
    Char(char),
    String(Cow<'a, str>),
    /// A `list<u8>`; lifting always yields this, lowering takes either.
    Bytes(Cow<'a, [u8]>),
    List(Vec<Value<'a>>),
    Record(Vec<Value<'a>>),
    Variant(u32, Option<Box<Value<'a>>>),
    Flags(u64),
}

impl Value<'_> {
    /// Copies whatever this borrows.
    pub fn into_owned(self) -> Value<'static> {
        match self {
            Value::Bool(v) => Value::Bool(v),
            Value::S8(v) => Value::S8(v),
            Value::U8(v) => Value::U8(v),
            Value::S16(v) => Value::S16(v),
            Value::U16(v) => Value::U16(v),
            Value::S32(v) => Value::S32(v),
            Value::U32(v) => Value::U32(v),
            Value::S64(v) => Value::S64(v),
            Value::U64(v) => Value::U64(v),
            Value::F32(v) => Value::F32(v),
            Value::F64(v) => Value::F64(v),
            Value::Char(v) => Value::Char(v),
            Value::String(v) => Value::String(Cow::Owned(v.into_owned())),
            Value::Bytes(v) => Value::Bytes(Cow::Owned(v.into_owned())),
            Value::List(v) => Value::List(v.into_iter().map(Value::into_owned).collect()),
            Value::Record(v) => Value::Record(v.into_iter().map(Value::into_owned).collect()),
            Value::Variant(case, payload) => {
                Value::Variant(case, payload.map(|p| Box::new(p.into_owned())))
            }
            Value::Flags(v) => Value::Flags(v),
        }
    }
}

/// A core wasm value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FlatVal {
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
}

impl FlatVal {
    fn zero(ty: Flat) -> Self {
        match ty {
            Flat::I32 => FlatVal::I32(0),
            Flat::I64 => FlatVal::I64(0),
            Flat::F32 => FlatVal::F32(0.0),
            Flat::F64 => FlatVal::F64(0.0),
        }
    }
}

/// Caps on what one call may move across the boundary.
#[derive(Clone, Copy, Debug)]
pub struct Bounds {
    /// Longest string, in bytes.
    pub string: usize,
    /// Most elements in one list.
    pub list: usize,
    /// Bytes of strings and list storage in one call, in total.
    pub bytes: usize,
    /// List elements in one call, in total.
    pub values: usize,
}

impl Default for Bounds {
    fn default() -> Self {
        Self {
            string: 32 << 10,
            list: 4096,
            bytes: 1 << 20,
            values: 1 << 16,
        }
    }
}

/// What is left of one call's [`Bounds`].
#[derive(Debug)]
pub struct Budget {
    bounds: Bounds,
    bytes: usize,
    values: usize,
}

impl Budget {
    pub fn new(bounds: Bounds) -> Self {
        Self {
            bounds,
            bytes: bounds.bytes,
            values: bounds.values,
        }
    }

    fn bytes(&mut self, n: u64) -> Result<()> {
        let n = usize::try_from(n).map_err(|_| CanonError::BudgetExhausted)?;
        self.bytes = self
            .bytes
            .checked_sub(n)
            .ok_or(CanonError::BudgetExhausted)?;
        Ok(())
    }

    fn values(&mut self, n: usize) -> Result<()> {
        self.values = self
            .values
            .checked_sub(n)
            .ok_or(CanonError::BudgetExhausted)?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CanonError {
    OutOfBounds {
        ptr: u64,
        len: u64,
    },
    Misaligned {
        ptr: u32,
        align: u32,
    },
    InvalidUtf8,
    InvalidChar(u32),
    InvalidDiscriminant(u32),
    StringTooLong(u32),
    ListTooLong(u32),
    BudgetExhausted,
    /// A value or core value of the wrong type or count.
    TypeMismatch,
    /// The guest allocator failed or trapped.
    Alloc(String),
}

impl fmt::Display for CanonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CanonError::OutOfBounds { ptr, len } => {
                write!(f, "{len} bytes at {ptr} are outside guest memory")
            }
            CanonError::Misaligned { ptr, align } => {
                write!(f, "pointer {ptr} is not aligned to {align}")
            }
            CanonError::InvalidUtf8 => f.write_str("string is not UTF-8"),
            CanonError::InvalidChar(c) => write!(f, "{c:#x} is not a char"),
            CanonError::InvalidDiscriminant(d) => write!(f, "discriminant {d} out of range"),
            CanonError::StringTooLong(n) => write!(f, "string of {n} bytes is too long"),
            CanonError::ListTooLong(n) => write!(f, "list of {n} elements is too long"),
            CanonError::BudgetExhausted => f.write_str("call moves too much data"),
            CanonError::TypeMismatch => f.write_str("value does not match its type"),
            CanonError::Alloc(e) => write!(f, "guest allocation failed: {e}"),
        }
    }
}

impl std::error::Error for CanonError {}

type Result<T> = std::result::Result<T, CanonError>;

fn span(mem: &[u8], ptr: u32, len: u64) -> Result<&[u8]> {
    let start = u64::from(ptr);
    let end = start.checked_add(len).filter(|&e| e <= mem.len() as u64);
    match end {
        Some(end) => Ok(&mem[start as usize..end as usize]),
        None => Err(CanonError::OutOfBounds { ptr: start, len }),
    }
}

fn span_mut(mem: &mut [u8], ptr: u32, len: u64) -> Result<&mut [u8]> {
    let start = u64::from(ptr);
    let end = start.checked_add(len).filter(|&e| e <= mem.len() as u64);
    match end {
        Some(end) => Ok(&mut mem[start as usize..end as usize]),
        None => Err(CanonError::OutOfBounds { ptr: start, len }),
    }
}

fn aligned(ptr: u32, align: u32) -> Result<()> {
    if align > 1 && !ptr.is_multiple_of(align) {
        return Err(CanonError::Misaligned { ptr, align });
    }
    Ok(())
}

fn read<const N: usize>(mem: &[u8], ptr: u32) -> Result<[u8; N]> {
    let bytes = span(mem, ptr, N as u64)?;
    Ok(bytes.try_into().expect("span has length N"))
}

fn read_uint(mem: &[u8], ptr: u32, size: u32) -> Result<u32> {
    Ok(match size {
        0 => 0,
        1 => u32::from(read::<1>(mem, ptr)?[0]),
        2 => u32::from(u16::from_le_bytes(read(mem, ptr)?)),
        _ => u32::from_le_bytes(read(mem, ptr)?),
    })
}

fn offset(ptr: u32, by: u32) -> Result<u32> {
    ptr.checked_add(by).ok_or(CanonError::OutOfBounds {
        ptr: u64::from(ptr),
        len: u64::from(by),
    })
}

fn flags_mask(count: u32) -> u64 {
    if count >= 64 {
        u64::MAX
    } else {
        (1 << count) - 1
    }
}

fn read_flags(mem: &[u8], ptr: u32, size: u32) -> Result<u64> {
    if size > 4 {
        Ok(u64::from_le_bytes(read(mem, ptr)?))
    } else {
        read_uint(mem, ptr, size).map(u64::from)
    }
}

fn write_flags(guest: &mut dyn GuestMemory, ptr: u32, size: u32, value: u64) -> Result<()> {
    if size > 4 {
        write(guest, ptr, &value.to_le_bytes())
    } else {
        write_uint(guest, ptr, size, value as u32)
    }
}

/// Reads a value of `layout` at `ptr`.
pub fn load<'m>(
    layout: &Layout,
    mem: &'m [u8],
    ptr: u32,
    budget: &mut Budget,
) -> Result<Value<'m>> {
    aligned(ptr, layout.align)?;
    span(mem, ptr, u64::from(layout.size))?;
    Ok(match &layout.shape {
        Shape::Bool => Value::Bool(read::<1>(mem, ptr)?[0] != 0),
        Shape::S8 => Value::S8(i8::from_le_bytes(read(mem, ptr)?)),
        Shape::U8 => Value::U8(u8::from_le_bytes(read(mem, ptr)?)),
        Shape::S16 => Value::S16(i16::from_le_bytes(read(mem, ptr)?)),
        Shape::U16 => Value::U16(u16::from_le_bytes(read(mem, ptr)?)),
        Shape::S32 => Value::S32(i32::from_le_bytes(read(mem, ptr)?)),
        Shape::U32 => Value::U32(u32::from_le_bytes(read(mem, ptr)?)),
        Shape::S64 => Value::S64(i64::from_le_bytes(read(mem, ptr)?)),
        Shape::U64 => Value::U64(u64::from_le_bytes(read(mem, ptr)?)),
        Shape::F32 => Value::F32(f32::from_le_bytes(read(mem, ptr)?)),
        Shape::F64 => Value::F64(f64::from_le_bytes(read(mem, ptr)?)),
        Shape::Char => char_of(u32::from_le_bytes(read(mem, ptr)?))?,
        Shape::String => {
            let (data, len) = pointer_pair(mem, ptr)?;
            load_string(mem, data, len, budget)?
        }
        Shape::List(element) => {
            let (data, len) = pointer_pair(mem, ptr)?;
            load_list(element, mem, data, len, budget)?
        }
        Shape::Record(fields) => {
            let mut values = Vec::with_capacity(fields.len());
            for field in fields {
                values.push(load(
                    &field.layout,
                    mem,
                    offset(ptr, field.offset)?,
                    budget,
                )?);
            }
            Value::Record(values)
        }
        Shape::Variant(variant) => {
            let case = read_uint(mem, ptr, variant.tag_size)?;
            let payload = variant
                .cases
                .get(case as usize)
                .ok_or(CanonError::InvalidDiscriminant(case))?;
            let payload = match payload {
                Some(layout) => Some(Box::new(load(
                    layout,
                    mem,
                    offset(ptr, variant.payload_offset)?,
                    budget,
                )?)),
                None => None,
            };
            Value::Variant(case, payload)
        }
        Shape::Flags(count) => {
            Value::Flags(read_flags(mem, ptr, layout.size)? & flags_mask(*count))
        }
    })
}

fn pointer_pair(mem: &[u8], ptr: u32) -> Result<(u32, u32)> {
    let data = u32::from_le_bytes(read(mem, ptr)?);
    let len = u32::from_le_bytes(read(mem, offset(ptr, 4)?)?);
    Ok((data, len))
}

fn char_of(c: u32) -> Result<Value<'static>> {
    char::from_u32(c)
        .map(Value::Char)
        .ok_or(CanonError::InvalidChar(c))
}

fn load_string<'m>(mem: &'m [u8], ptr: u32, len: u32, budget: &mut Budget) -> Result<Value<'m>> {
    if len as usize > budget.bounds.string {
        return Err(CanonError::StringTooLong(len));
    }
    budget.bytes(u64::from(len))?;
    let bytes = span(mem, ptr, u64::from(len))?;
    let text = std::str::from_utf8(bytes).map_err(|_| CanonError::InvalidUtf8)?;
    Ok(Value::String(Cow::Borrowed(text)))
}

fn load_list<'m>(
    element: &Layout,
    mem: &'m [u8],
    ptr: u32,
    len: u32,
    budget: &mut Budget,
) -> Result<Value<'m>> {
    if matches!(element.shape, Shape::U8) {
        budget.bytes(u64::from(len))?;
        return Ok(Value::Bytes(Cow::Borrowed(span(mem, ptr, u64::from(len))?)));
    }
    if len as usize > budget.bounds.list {
        return Err(CanonError::ListTooLong(len));
    }
    budget.values(len as usize)?;
    let total = u64::from(len) * u64::from(element.size);
    budget.bytes(total)?;
    aligned(ptr, element.align)?;
    span(mem, ptr, total)?;
    let mut values = Vec::with_capacity(len as usize);
    for i in 0..len {
        values.push(load(element, mem, ptr + i * element.size, budget)?);
    }
    Ok(Value::List(values))
}

/// Core values being lifted, consumed front to back.
pub struct FlatReader<'a> {
    values: &'a [FlatVal],
    next: usize,
}

impl<'a> FlatReader<'a> {
    pub fn new(values: &'a [FlatVal]) -> Self {
        Self { values, next: 0 }
    }

    fn take(&mut self) -> Result<FlatVal> {
        let value = self
            .values
            .get(self.next)
            .copied()
            .ok_or(CanonError::TypeMismatch)?;
        self.next += 1;
        Ok(value)
    }

    fn take_n(&mut self, n: usize) -> Result<&'a [FlatVal]> {
        let end = self.next + n;
        let values = self
            .values
            .get(self.next..end)
            .ok_or(CanonError::TypeMismatch)?;
        self.next = end;
        Ok(values)
    }

    fn i32(&mut self) -> Result<i32> {
        match self.take()? {
            FlatVal::I32(v) => Ok(v),
            _ => Err(CanonError::TypeMismatch),
        }
    }

    fn i64(&mut self) -> Result<i64> {
        match self.take()? {
            FlatVal::I64(v) => Ok(v),
            _ => Err(CanonError::TypeMismatch),
        }
    }

    /// Fails unless every value was consumed.
    pub fn finish(&self) -> Result<()> {
        if self.next == self.values.len() {
            Ok(())
        } else {
            Err(CanonError::TypeMismatch)
        }
    }
}

/// Reads a value of `layout` from core values.
pub fn lift_flat<'m>(
    layout: &Layout,
    reader: &mut FlatReader<'_>,
    mem: &'m [u8],
    budget: &mut Budget,
) -> Result<Value<'m>> {
    Ok(match &layout.shape {
        Shape::Bool => Value::Bool(reader.i32()? != 0),
        Shape::S8 => Value::S8(reader.i32()? as i8),
        Shape::U8 => Value::U8(reader.i32()? as u8),
        Shape::S16 => Value::S16(reader.i32()? as i16),
        Shape::U16 => Value::U16(reader.i32()? as u16),
        Shape::S32 => Value::S32(reader.i32()?),
        Shape::U32 => Value::U32(reader.i32()? as u32),
        Shape::S64 => Value::S64(reader.i64()?),
        Shape::U64 => Value::U64(reader.i64()? as u64),
        Shape::F32 => match reader.take()? {
            FlatVal::F32(v) => Value::F32(v),
            _ => return Err(CanonError::TypeMismatch),
        },
        Shape::F64 => match reader.take()? {
            FlatVal::F64(v) => Value::F64(v),
            _ => return Err(CanonError::TypeMismatch),
        },
        Shape::Char => char_of(reader.i32()? as u32)?,
        Shape::String => {
            let (ptr, len) = (reader.i32()? as u32, reader.i32()? as u32);
            load_string(mem, ptr, len, budget)?
        }
        Shape::List(element) => {
            let (ptr, len) = (reader.i32()? as u32, reader.i32()? as u32);
            load_list(element, mem, ptr, len, budget)?
        }
        Shape::Record(fields) => {
            let mut values = Vec::with_capacity(fields.len());
            for field in fields {
                values.push(lift_flat(&field.layout, reader, mem, budget)?);
            }
            Value::Record(values)
        }
        Shape::Flags(0) => Value::Flags(0),
        Shape::Flags(count) => {
            let low = u64::from(reader.i32()? as u32);
            let high = if *count > 32 {
                u64::from(reader.i32()? as u32)
            } else {
                0
            };
            Value::Flags((high << 32 | low) & flags_mask(*count))
        }
        Shape::Variant(variant) => {
            let case = reader.i32()? as u32;
            let joined = reader.take_n(layout.flat.len() - 1)?;
            let payload = variant
                .cases
                .get(case as usize)
                .ok_or(CanonError::InvalidDiscriminant(case))?;
            let payload = match payload {
                Some(payload) => {
                    let coerced = payload
                        .flat
                        .iter()
                        .zip(joined)
                        .map(|(want, have)| coerce_lift(*have, *want))
                        .collect::<Result<Vec<_>>>()?;
                    let mut sub = FlatReader::new(&coerced);
                    Some(Box::new(lift_flat(payload, &mut sub, mem, budget)?))
                }
                None => None,
            };
            Value::Variant(case, payload)
        }
    })
}

fn coerce_lift(have: FlatVal, want: Flat) -> Result<FlatVal> {
    Ok(match (have, want) {
        (FlatVal::I32(v), Flat::I32) => FlatVal::I32(v),
        (FlatVal::I32(v), Flat::F32) => FlatVal::F32(f32::from_bits(v as u32)),
        (FlatVal::I64(v), Flat::I32) => FlatVal::I32(v as i32),
        (FlatVal::I64(v), Flat::I64) => FlatVal::I64(v),
        (FlatVal::I64(v), Flat::F32) => FlatVal::F32(f32::from_bits(v as u32)),
        (FlatVal::I64(v), Flat::F64) => FlatVal::F64(f64::from_bits(v as u64)),
        (FlatVal::F32(v), Flat::F32) => FlatVal::F32(v),
        (FlatVal::F64(v), Flat::F64) => FlatVal::F64(v),
        _ => return Err(CanonError::TypeMismatch),
    })
}

fn coerce_lower(have: FlatVal, want: Flat) -> Result<FlatVal> {
    Ok(match (have, want) {
        (FlatVal::I32(v), Flat::I32) => FlatVal::I32(v),
        (FlatVal::F32(v), Flat::I32) => FlatVal::I32(v.to_bits() as i32),
        (FlatVal::I32(v), Flat::I64) => FlatVal::I64(i64::from(v as u32)),
        (FlatVal::F32(v), Flat::I64) => FlatVal::I64(i64::from(v.to_bits())),
        (FlatVal::F64(v), Flat::I64) => FlatVal::I64(v.to_bits() as i64),
        (FlatVal::I64(v), Flat::I64) => FlatVal::I64(v),
        (FlatVal::F32(v), Flat::F32) => FlatVal::F32(v),
        (FlatVal::F64(v), Flat::F64) => FlatVal::F64(v),
        _ => return Err(CanonError::TypeMismatch),
    })
}

/// Guest memory the host writes into, with the guest's allocator.
pub trait GuestMemory {
    /// Allocates `size` bytes aligned to `align` with the guest's
    /// `cabi_realloc`, checked to lie inside memory.
    fn realloc(&mut self, align: u32, size: u32) -> Result<u32>;

    fn memory(&mut self) -> &mut [u8];
}

fn alloc(guest: &mut dyn GuestMemory, align: u32, size: u32) -> Result<u32> {
    let ptr = guest.realloc(align, size)?;
    aligned(ptr, align)?;
    span(guest.memory(), ptr, u64::from(size))?;
    Ok(ptr)
}

fn write(guest: &mut dyn GuestMemory, ptr: u32, bytes: &[u8]) -> Result<()> {
    span_mut(guest.memory(), ptr, bytes.len() as u64)?.copy_from_slice(bytes);
    Ok(())
}

fn write_uint(guest: &mut dyn GuestMemory, ptr: u32, size: u32, value: u32) -> Result<()> {
    match size {
        0 => Ok(()),
        1 => write(guest, ptr, &[value as u8]),
        2 => write(guest, ptr, &(value as u16).to_le_bytes()),
        _ => write(guest, ptr, &value.to_le_bytes()),
    }
}

fn lower_string(guest: &mut dyn GuestMemory, text: &str) -> Result<(u32, u32)> {
    let len = u32::try_from(text.len()).map_err(|_| CanonError::StringTooLong(u32::MAX))?;
    let ptr = alloc(guest, 1, len)?;
    write(guest, ptr, text.as_bytes())?;
    Ok((ptr, len))
}

fn lower_bytes(guest: &mut dyn GuestMemory, bytes: &[u8]) -> Result<(u32, u32)> {
    let len = u32::try_from(bytes.len()).map_err(|_| CanonError::ListTooLong(u32::MAX))?;
    let ptr = alloc(guest, 1, len)?;
    write(guest, ptr, bytes)?;
    Ok((ptr, len))
}

fn lower_list(
    guest: &mut dyn GuestMemory,
    element: &Layout,
    items: &[Value<'_>],
) -> Result<(u32, u32)> {
    let len = u32::try_from(items.len()).map_err(|_| CanonError::ListTooLong(u32::MAX))?;
    let size = len
        .checked_mul(element.size)
        .ok_or(CanonError::ListTooLong(len))?;
    let ptr = alloc(guest, element.align, size)?;
    for (i, item) in (0..).zip(items) {
        store(element, item, guest, ptr + i * element.size)?;
    }
    Ok((ptr, len))
}

/// Writes `value` of `layout` at `ptr`, allocating strings and lists with
/// the guest's allocator.
pub fn store(
    layout: &Layout,
    value: &Value<'_>,
    guest: &mut dyn GuestMemory,
    ptr: u32,
) -> Result<()> {
    aligned(ptr, layout.align)?;
    span(guest.memory(), ptr, u64::from(layout.size))?;
    match (&layout.shape, value) {
        (Shape::Bool, Value::Bool(v)) => write(guest, ptr, &[u8::from(*v)]),
        (Shape::S8, Value::S8(v)) => write(guest, ptr, &v.to_le_bytes()),
        (Shape::U8, Value::U8(v)) => write(guest, ptr, &v.to_le_bytes()),
        (Shape::S16, Value::S16(v)) => write(guest, ptr, &v.to_le_bytes()),
        (Shape::U16, Value::U16(v)) => write(guest, ptr, &v.to_le_bytes()),
        (Shape::S32, Value::S32(v)) => write(guest, ptr, &v.to_le_bytes()),
        (Shape::U32, Value::U32(v)) => write(guest, ptr, &v.to_le_bytes()),
        (Shape::S64, Value::S64(v)) => write(guest, ptr, &v.to_le_bytes()),
        (Shape::U64, Value::U64(v)) => write(guest, ptr, &v.to_le_bytes()),
        (Shape::F32, Value::F32(v)) => write(guest, ptr, &v.to_le_bytes()),
        (Shape::F64, Value::F64(v)) => write(guest, ptr, &v.to_le_bytes()),
        (Shape::Char, Value::Char(v)) => write(guest, ptr, &u32::from(*v).to_le_bytes()),
        (Shape::String, Value::String(text)) => {
            let (data, len) = lower_string(guest, text)?;
            write(guest, ptr, &data.to_le_bytes())?;
            write(guest, ptr + 4, &len.to_le_bytes())
        }
        (Shape::List(element), Value::List(items)) => {
            let (data, len) = lower_list(guest, element, items)?;
            write(guest, ptr, &data.to_le_bytes())?;
            write(guest, ptr + 4, &len.to_le_bytes())
        }
        (Shape::List(element), Value::Bytes(bytes)) if matches!(element.shape, Shape::U8) => {
            let (data, len) = lower_bytes(guest, bytes)?;
            write(guest, ptr, &data.to_le_bytes())?;
            write(guest, ptr + 4, &len.to_le_bytes())
        }
        (Shape::Record(fields), Value::Record(values)) if fields.len() == values.len() => {
            for (field, value) in fields.iter().zip(values) {
                store(&field.layout, value, guest, ptr + field.offset)?;
            }
            Ok(())
        }
        (Shape::Variant(variant), Value::Variant(case, payload)) => {
            let layout = variant
                .cases
                .get(*case as usize)
                .ok_or(CanonError::TypeMismatch)?;
            write_uint(guest, ptr, variant.tag_size, *case)?;
            match (layout, payload) {
                (Some(layout), Some(payload)) => {
                    store(layout, payload, guest, ptr + variant.payload_offset)
                }
                (None, None) => Ok(()),
                _ => Err(CanonError::TypeMismatch),
            }
        }
        (Shape::Flags(count), Value::Flags(bits)) if bits & !flags_mask(*count) == 0 => {
            write_flags(guest, ptr, layout.size, *bits)
        }
        _ => Err(CanonError::TypeMismatch),
    }
}

/// Lowers `value` of `layout` to core values appended to `out`.
pub fn lower_flat(
    layout: &Layout,
    value: &Value<'_>,
    guest: &mut dyn GuestMemory,
    out: &mut Vec<FlatVal>,
) -> Result<()> {
    let flat = match (&layout.shape, value) {
        (Shape::Bool, Value::Bool(v)) => FlatVal::I32(i32::from(*v)),
        (Shape::S8, Value::S8(v)) => FlatVal::I32(i32::from(*v)),
        (Shape::U8, Value::U8(v)) => FlatVal::I32(i32::from(*v)),
        (Shape::S16, Value::S16(v)) => FlatVal::I32(i32::from(*v)),
        (Shape::U16, Value::U16(v)) => FlatVal::I32(i32::from(*v)),
        (Shape::S32, Value::S32(v)) => FlatVal::I32(*v),
        (Shape::U32, Value::U32(v)) => FlatVal::I32(*v as i32),
        (Shape::S64, Value::S64(v)) => FlatVal::I64(*v),
        (Shape::U64, Value::U64(v)) => FlatVal::I64(*v as i64),
        (Shape::F32, Value::F32(v)) => FlatVal::F32(*v),
        (Shape::F64, Value::F64(v)) => FlatVal::F64(*v),
        (Shape::Char, Value::Char(v)) => FlatVal::I32(u32::from(*v) as i32),
        (Shape::String, Value::String(text)) => {
            let (ptr, len) = lower_string(guest, text)?;
            out.extend([FlatVal::I32(ptr as i32), FlatVal::I32(len as i32)]);
            return Ok(());
        }
        (Shape::List(element), Value::List(items)) => {
            let (ptr, len) = lower_list(guest, element, items)?;
            out.extend([FlatVal::I32(ptr as i32), FlatVal::I32(len as i32)]);
            return Ok(());
        }
        (Shape::List(element), Value::Bytes(bytes)) if matches!(element.shape, Shape::U8) => {
            let (ptr, len) = lower_bytes(guest, bytes)?;
            out.extend([FlatVal::I32(ptr as i32), FlatVal::I32(len as i32)]);
            return Ok(());
        }
        (Shape::Record(fields), Value::Record(values)) if fields.len() == values.len() => {
            for (field, value) in fields.iter().zip(values) {
                lower_flat(&field.layout, value, guest, out)?;
            }
            return Ok(());
        }
        (Shape::Flags(0), Value::Flags(0)) => return Ok(()),
        (Shape::Flags(count), Value::Flags(bits)) if bits & !flags_mask(*count) == 0 => {
            if *count > 32 {
                out.push(FlatVal::I32(*bits as u32 as i32));
                FlatVal::I32((*bits >> 32) as u32 as i32)
            } else {
                FlatVal::I32(*bits as u32 as i32)
            }
        }
        (Shape::Variant(variant), Value::Variant(case, payload)) => {
            let case_layout = variant
                .cases
                .get(*case as usize)
                .ok_or(CanonError::TypeMismatch)?;
            let mut lowered = Vec::new();
            match (case_layout, payload) {
                (Some(layout), Some(payload)) => lower_flat(layout, payload, guest, &mut lowered)?,
                (None, None) => {}
                _ => return Err(CanonError::TypeMismatch),
            }
            out.push(FlatVal::I32(*case as i32));
            let joined = &layout.flat[1..];
            if lowered.len() > joined.len() {
                return Err(CanonError::TypeMismatch);
            }
            for (i, ty) in joined.iter().enumerate() {
                out.push(match lowered.get(i) {
                    Some(v) => coerce_lower(*v, *ty)?,
                    None => FlatVal::zero(*ty),
                });
            }
            return Ok(());
        }
        _ => return Err(CanonError::TypeMismatch),
    };
    out.push(flat);
    Ok(())
}
