//! A core module built against a [`Schema`] instantiated on a backend
//! engine (wasmi or wasmtime, chosen by Cargo feature): every import is
//! one dynamic host function that lifts its arguments (strings and byte
//! lists borrowed from guest memory), hands them to a [`Host`] and lowers
//! the answer; exports are called the same way in reverse. Fuel and
//! memory caps are the sandbox's [`Limits`]. Everything engine specific
//! lives in the backend module; this file is engine independent.

use std::sync::Arc;

use super::backend::Instance;
use super::canon::{self, Bounds, Budget, CanonError, FlatReader, FlatVal, GuestMemory, Value};
use super::schema::{Schema, Signature};
use super::value::{self, ToValue};
use crate::sandbox::{Fault, Limits};

pub use super::backend::{Engine, engine};

/// The name of the guest allocator every module must export.
pub const REALLOC: &str = "cabi_realloc";

/// Answers a module's imports.
pub trait Host: Send + 'static {
    /// Handles a call of `schema.imports[index]`; arguments may borrow
    /// guest memory. `Err` traps, faulting the plugin.
    fn call(
        &mut self,
        index: usize,
        args: Vec<Value<'_>>,
    ) -> value::Result<Option<Box<dyn ToValue>>>;
}

/// What a backend gives the shared import path while a host call runs.
pub(super) trait ImportCx<H>: GuestMemory {
    /// Guest memory, the host and the lift bounds together, or `None`
    /// before instantiation finished.
    fn parts(&mut self) -> Option<(&[u8], &mut H, Bounds)>;
}

pub(super) fn fault(context: &str, error: impl std::fmt::Display) -> Fault {
    Fault(format!("{context}: {error}"))
}

/// A shallow copy of `value` that borrows its strings and lists.
fn borrow<'a>(value: &'a Value<'_>) -> Value<'a> {
    match value {
        Value::String(text) => Value::String(std::borrow::Cow::Borrowed(text)),
        Value::Bytes(bytes) => Value::Bytes(std::borrow::Cow::Borrowed(bytes)),
        Value::List(items) => Value::List(items.iter().map(borrow).collect()),
        Value::Record(fields) => Value::Record(fields.iter().map(borrow).collect()),
        Value::Variant(case, payload) => {
            Value::Variant(*case, payload.as_ref().map(|p| Box::new(borrow(p))))
        }
        other => other.clone(),
    }
}

/// Guest memory reached through a backend instance.
struct Guest<'a, H: Host>(&'a mut Instance<H>);

impl<H: Host> GuestMemory for Guest<'_, H> {
    fn realloc(&mut self, align: u32, size: u32) -> Result<u32, CanonError> {
        self.0.realloc(align, size)
    }

    fn memory(&mut self) -> &mut [u8] {
        self.0.memory_mut()
    }
}

fn lift_args<'m>(
    sig: &Signature,
    flat: &[FlatVal],
    mem: &'m [u8],
    budget: &mut Budget,
) -> Result<Vec<Value<'m>>, CanonError> {
    if sig.indirect_params {
        let [FlatVal::I32(ptr)] = flat else {
            return Err(CanonError::TypeMismatch);
        };
        return match canon::load(&sig.params_record, mem, *ptr as u32, budget)? {
            Value::Record(values) => Ok(values),
            _ => Err(CanonError::TypeMismatch),
        };
    }
    let mut reader = FlatReader::new(flat);
    let args = sig
        .params
        .iter()
        .map(|layout| canon::lift_flat(layout, &mut reader, mem, budget))
        .collect::<Result<Vec<_>, _>>()?;
    reader.finish()?;
    Ok(args)
}

/// Runs `schema.imports[index]` for a backend: lifts `flat`, calls the
/// host, lowers the answer and returns the core results.
pub(super) fn import<H: Host, C: ImportCx<H>>(
    schema: &Schema,
    index: usize,
    cx: &mut C,
    flat: &[FlatVal],
    results: usize,
) -> Result<Vec<FlatVal>, String> {
    let sig = &schema.imports[index];
    let (flat, retptr) = if sig.retptr {
        match flat.split_last() {
            Some((FlatVal::I32(ptr), rest)) => (rest, Some(*ptr as u32)),
            _ => return Err("missing return pointer".into()),
        }
    } else {
        (flat, None)
    };
    let answer = {
        let Some((mem, host, bounds)) = cx.parts() else {
            return Err(format!("{} called before instantiation finished", sig.name));
        };
        let mut budget = Budget::new(bounds);
        let args =
            lift_args(sig, flat, mem, &mut budget).map_err(|e| format!("{}: {e}", sig.name))?;
        host.call(index, args).map_err(|e| e.to_string())?
    };
    match (&sig.result, answer, retptr) {
        (None, None, _) => Ok(Vec::new()),
        (Some(layout), Some(value), Some(ptr)) => {
            canon::store(layout, &value.to_value(), cx, ptr)
                .map_err(|e| format!("{}: {e}", sig.name))?;
            Ok(Vec::new())
        }
        (Some(layout), Some(value), None) => {
            let mut out = Vec::new();
            canon::lower_flat(layout, &value.to_value(), cx, &mut out)
                .map_err(|e| format!("{}: {e}", sig.name))?;
            if out.len() != results {
                return Err(format!("{}: result arity", sig.name));
            }
            Ok(out)
        }
        _ => Err(format!("{}: host answered the wrong shape", sig.name)),
    }
}

/// One instantiated module.
pub struct Runtime<H: Host> {
    inst: Instance<H>,
    schema: Arc<Schema>,
}

impl<H: Host> Runtime<H> {
    /// Compiles and instantiates `wasm`, linking every import of `schema`
    /// to `host`. The module must export `memory` and [`REALLOC`]; each
    /// export of `schema` it has must have its exact core signature.
    pub fn new(
        engine: &Engine,
        schema: Arc<Schema>,
        wasm: &[u8],
        host: H,
        limits: Limits,
        bounds: Bounds,
    ) -> Result<Self, Fault> {
        let inst = Instance::new(engine, &schema, wasm, host, limits, bounds)?;
        Ok(Self { inst, schema })
    }

    pub fn host(&self) -> &H {
        self.inst.host()
    }

    pub fn host_mut(&mut self) -> &mut H {
        self.inst.host_mut()
    }

    /// Whether the module has `schema.exports[index]`.
    pub fn exports(&self, index: usize) -> bool {
        self.inst.has_export(index)
    }

    /// Calls `schema.exports[index]` with fresh fuel; `lift` reads its
    /// result, which borrows guest memory, before the guest may free it.
    pub fn call<R>(
        &mut self,
        index: usize,
        args: &[Value<'_>],
        lift: impl FnOnce(Option<Value<'_>>) -> value::Result<R>,
    ) -> Result<R, Fault> {
        let schema = self.schema.clone();
        let sig = schema
            .exports
            .get(index)
            .ok_or_else(|| Fault(format!("no export {index}")))?;
        let name = sig.field.as_str();
        if args.len() != sig.params.len() {
            return Err(Fault(format!("{name}: wrong argument count")));
        }
        if !self.inst.has_export(index) {
            return Err(Fault(format!("module exports no {name}")));
        }
        self.inst.reset_fuel()?;
        let mut flat = Vec::new();
        {
            let mut guest = Guest(&mut self.inst);
            if sig.indirect_params {
                let record = &sig.params_record;
                let ptr = guest
                    .realloc(record.align, record.size)
                    .map_err(|e| fault(name, e))?;
                let fields = args.iter().map(borrow).collect();
                canon::store(record, &Value::Record(fields), &mut guest, ptr)
                    .map_err(|e| fault(name, e))?;
                flat.push(FlatVal::I32(ptr as i32));
            } else {
                for (layout, value) in sig.params.iter().zip(args) {
                    canon::lower_flat(layout, value, &mut guest, &mut flat)
                        .map_err(|e| fault(name, e))?;
                }
            }
        }
        let out = self.inst.invoke(index, &flat)?;
        let result = match &sig.result {
            None => lift(None),
            Some(layout) => {
                let mem = self.inst.memory();
                let mut budget = Budget::new(self.inst.bounds());
                let value = if sig.retptr {
                    let [FlatVal::I32(ptr)] = out[..] else {
                        return Err(Fault(format!("{name}: no result pointer")));
                    };
                    canon::load(layout, mem, ptr as u32, &mut budget)
                } else {
                    let mut reader = FlatReader::new(&out);
                    canon::lift_flat(layout, &mut reader, mem, &mut budget)
                        .and_then(|v| reader.finish().map(|()| v))
                };
                lift(Some(value.map_err(|e| fault(name, e))?))
            }
        }
        .map_err(|e| fault(name, e))?;
        self.inst.post_return(index, &out)?;
        Ok(result)
    }
}
