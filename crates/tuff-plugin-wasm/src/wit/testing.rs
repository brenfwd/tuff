//! Helpers for hand-written WAT guests in tests: the world's imports as
//! `$name` functions (`$reply`, `$net.send`), a bump `cabi_realloc`, the
//! core signatures of exports, and data laid out with the canonical ABI
//! as constant call arguments.
//!
//! Memory map of a [`module`]: export results are built at [`OUT`],
//! results of imports land at [`RET`], [`TEXT`] builds text at 512,
//! [`Data`] lives from [`DATA`] and the heap the host allocates from
//! starts at [`HEAP`].

use std::fmt::Write as _;

use super::bindings::{EXPORTS, IMPORTS};
use super::canon::{self, CanonError, FlatVal, GuestMemory, Value};
use super::schema::{Flat, Signature};

/// Scratch for an export's result.
pub const OUT: u32 = 128;
/// Where an import's return pointer points.
pub const RET: u32 = 256;
/// WAT functions that build text at 512: `$clear`, `$put (ptr len)`,
/// `$put_char (c)`, `$put_int (i32)` and `$text_len (result i32)`.
pub const TEXT: &str = r#"
  (global $cursor (mut i32) (i32.const 512))
  (func $clear (global.set $cursor (i32.const 512)))
  (func $put (param $ptr i32) (param $len i32)
    (memory.copy (global.get $cursor) (local.get $ptr) (local.get $len))
    (global.set $cursor (i32.add (global.get $cursor) (local.get $len))))
  (func $put_char (param $c i32)
    (i32.store8 (global.get $cursor) (local.get $c))
    (global.set $cursor (i32.add (global.get $cursor) (i32.const 1))))
  (func $put_int (param $v i32)
    (local $n i32)
    (if (i32.lt_s (local.get $v) (i32.const 0))
      (then
        (call $put_char (i32.const 45))
        (local.set $v (i32.sub (i32.const 0) (local.get $v)))))
    (loop $digits
      (i32.store8 (i32.sub (i32.const 495) (local.get $n))
        (i32.add (i32.const 48) (i32.rem_u (local.get $v) (i32.const 10))))
      (local.set $n (i32.add (local.get $n) (i32.const 1)))
      (local.set $v (i32.div_u (local.get $v) (i32.const 10)))
      (br_if $digits (local.get $v)))
    (call $put (i32.sub (i32.const 496) (local.get $n)) (local.get $n)))
  (func $text_len (result i32)
    (i32.sub (global.get $cursor) (i32.const 512)))
"#;

/// Where [`Data`] starts.
pub const DATA: u32 = 1024;
/// Where `cabi_realloc` starts handing out memory.
pub const HEAP: u32 = 65536;

fn flat_name(flat: Flat) -> &'static str {
    match flat {
        Flat::I32 => "i32",
        Flat::I64 => "i64",
        Flat::F32 => "f32",
        Flat::F64 => "f64",
    }
}

fn types(kind: &str, flats: &[Flat]) -> String {
    if flats.is_empty() {
        return String::new();
    }
    let names: Vec<&str> = flats.iter().copied().map(flat_name).collect();
    format!(" ({kind} {})", names.join(" "))
}

fn import_name(module: &str, field: &str) -> String {
    let interface = module
        .split_once('/')
        .map_or(module, |(_, rest)| rest)
        .split('@')
        .next()
        .unwrap_or(module);
    match interface {
        "host" => field.to_owned(),
        other => format!("{other}.{field}"),
    }
}

/// Every import, an exported two-page `memory` and a bump `cabi_realloc`
/// from [`HEAP`] that grows memory as needed.
pub fn prelude() -> String {
    let schema = super::schema();
    let mut out = String::new();
    for (sig, (module, field)) in schema.imports.iter().zip(IMPORTS) {
        let _ = writeln!(
            out,
            "  (import \"{module}\" \"{field}\" (func ${}{}{}))",
            import_name(module, field),
            types("param", &sig.core_params),
            types("result", &sig.core_results),
        );
    }
    let _ = write!(
        out,
        r#"  (memory (export "memory") 2)
  (global $heap (mut i32) (i32.const {HEAP}))
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32)
    (local $ptr i32)
    (local.set $ptr
      (i32.and
        (i32.add (global.get $heap) (i32.sub (local.get 2) (i32.const 1)))
        (i32.sub (i32.const 0) (local.get 2))))
    (global.set $heap (i32.add (local.get $ptr) (local.get 3)))
    (if (i32.gt_u (global.get $heap) (i32.mul (memory.size) (i32.const 65536)))
      (then
        (drop (memory.grow
          (i32.add
            (i32.shr_u
              (i32.sub (global.get $heap) (i32.mul (memory.size) (i32.const 65536)))
              (i32.const 16))
            (i32.const 1))))))
    (local.get $ptr))
"#
    );
    out
}

fn with_export<R>(name: &str, f: impl FnOnce(&str, &Signature) -> R) -> R {
    let index = EXPORTS
        .iter()
        .position(|field| field.ends_with(&format!("#{name}")))
        .unwrap_or_else(|| panic!("no export {name}"));
    f(EXPORTS[index], &super::schema().exports[index])
}

fn with_import<R>(name: &str, f: impl FnOnce(&Signature) -> R) -> R {
    let schema = super::schema();
    let sig = schema
        .imports
        .iter()
        .zip(IMPORTS)
        .find(|(_, (module, field))| import_name(module, field) == name)
        .map(|(sig, _)| sig)
        .unwrap_or_else(|| panic!("no import {name}"));
    f(sig)
}

/// The head of a WAT function exporting guest function `name` (like
/// `run-command`) with its core signature; parameters are numbered
/// locals.
pub fn export(name: &str) -> String {
    with_export(name, |field, sig| {
        format!(
            "(export \"{field}\"){}{}",
            types("param", &sig.core_params),
            types("result", &sig.core_results)
        )
    })
}

/// The core parameters of guest function `name`.
pub fn export_params(name: &str) -> Vec<Flat> {
    with_export(name, |_, sig| sig.core_params.clone())
}

/// A module of [`prelude`] and `body`.
pub fn module(body: &str) -> String {
    format!("(module\n{}\n{body})", prelude())
}

/// WAT leaving the address of `ok` with no payload: what `register`
/// returns on success.
pub fn ok_unit() -> String {
    format!("(i32.store (i32.const {OUT}) (i32.const 0)) (i32.const {OUT})")
}

/// WAT leaving the address of `ok(code)` for `run-command`; `code` is a
/// WAT `i32` expression.
pub fn ok_code(code: &str) -> String {
    format!(
        "(i32.store (i32.const {OUT}) (i32.const 0)) (i32.store (i32.const {}) {code}) (i32.const {OUT})",
        OUT + 4
    )
}

/// WAT leaving the address of `err(text)`, `text` at `(ptr, len)`.
pub fn err_text((ptr, len): (u32, u32)) -> String {
    format!(
        "(i32.store (i32.const {OUT}) (i32.const 1)) (i32.store (i32.const {}) (i32.const {ptr})) (i32.store (i32.const {}) (i32.const {len})) (i32.const {OUT})",
        OUT + 4,
        OUT + 8
    )
}

/// Static data for a module, laid out from [`DATA`].
#[derive(Default)]
pub struct Data {
    bytes: Vec<u8>,
}

/// Guest memory over the data image; addresses below [`DATA`] are zeros.
struct Image {
    memory: Vec<u8>,
}

impl GuestMemory for Image {
    fn realloc(&mut self, align: u32, size: u32) -> Result<u32, CanonError> {
        let ptr = (self.memory.len() as u32).next_multiple_of(align.max(1));
        let end = ptr + size;
        if end > HEAP {
            return Err(CanonError::Alloc("test data is full".into()));
        }
        self.memory.resize(end as usize, 0);
        Ok(ptr)
    }

    fn memory(&mut self) -> &mut [u8] {
        &mut self.memory
    }
}

impl Data {
    pub fn new() -> Self {
        Self::default()
    }

    /// Places `text`; its `(ptr, len)`.
    pub fn string(&mut self, text: &str) -> (u32, u32) {
        self.bytes(text.as_bytes())
    }

    /// Places `bytes`; their `(ptr, len)`.
    pub fn bytes(&mut self, bytes: &[u8]) -> (u32, u32) {
        let ptr = DATA + self.bytes.len() as u32;
        self.bytes.extend_from_slice(bytes);
        (ptr, bytes.len() as u32)
    }

    fn lower(&mut self, f: impl FnOnce(&mut Image)) {
        let mut memory = vec![0; DATA as usize];
        memory.append(&mut self.bytes);
        let mut image = Image { memory };
        f(&mut image);
        self.bytes = image.memory.split_off(DATA as usize);
    }

    /// The core arguments a call of import `name` (its WIT name; `net.send`
    /// for interfaces other than `host`) passes for `args`, placing what
    /// they point to, then the return pointer if it takes one, as WAT
    /// constants.
    pub fn args(&mut self, name: &str, args: &[Value<'_>]) -> String {
        let mut flat = Vec::new();
        with_import(name, |sig| {
            self.lower(|image| {
                if sig.indirect_params {
                    let record = &sig.params_record;
                    let ptr = image.realloc(record.align, record.size).unwrap();
                    canon::store(record, &Value::Record(args.to_vec()), image, ptr).unwrap();
                    flat.push(FlatVal::I32(ptr as i32));
                } else {
                    for (layout, value) in sig.params.iter().zip(args) {
                        canon::lower_flat(layout, value, image, &mut flat).unwrap();
                    }
                }
            });
            if sig.retptr {
                flat.push(FlatVal::I32(RET as i32));
            }
        });
        constants(&flat)
    }

    /// `(call $name args...)`, dropping a result left on the stack.
    pub fn call(&mut self, name: &str, args: &[Value<'_>]) -> String {
        let results = with_import(name, |sig| sig.core_results.len());
        let call = format!("(call ${name} {})", self.args(name, args));
        match results {
            0 => call,
            _ => format!("(drop {call})"),
        }
    }

    /// Places the result export `name` returns for `value` (stored with a
    /// return pointer, as a list or record result is); its address.
    pub fn export_result(&mut self, name: &str, value: &Value<'_>) -> u32 {
        let mut ptr = 0;
        with_export(name, |_, sig| {
            let layout = sig.result.as_ref().expect("export has a result");
            self.lower(|image| {
                ptr = image.realloc(layout.align, layout.size).unwrap();
                canon::store(layout, value, image, ptr).unwrap();
            });
        });
        ptr
    }

    /// The data segment.
    pub fn wat(&self) -> String {
        let mut text = String::with_capacity(self.bytes.len() * 3);
        for byte in &self.bytes {
            let _ = write!(text, "\\{byte:02x}");
        }
        format!("(data (i32.const {DATA}) \"{text}\")")
    }
}

fn constants(flat: &[FlatVal]) -> String {
    let parts: Vec<String> = flat
        .iter()
        .map(|value| match value {
            FlatVal::I32(v) => format!("(i32.const {v})"),
            FlatVal::I64(v) => format!("(i64.const {v})"),
            FlatVal::F32(v) => format!("(f32.const {v:?})"),
            FlatVal::F64(v) => format!("(f64.const {v:?})"),
        })
        .collect();
    parts.join(" ")
}
