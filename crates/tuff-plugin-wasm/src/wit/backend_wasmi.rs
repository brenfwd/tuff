//! The wasmi backend: a pure-Rust interpreter, the default engine.

use std::sync::Arc;

use wasmi::{
    Caller, Config, Extern, Func, FuncType, Linker, Memory, Module, Store, StoreLimits,
    StoreLimitsBuilder, Val, ValType,
};

pub use wasmi::Engine;

use super::canon::{Bounds, CanonError, FlatVal, GuestMemory};
use super::runtime::{self, Host, ImportCx, REALLOC, fault};
use super::schema::{Flat, Schema};
use crate::sandbox::{Fault, Limits};

/// The engine every sandbox shares: fuel metering on, recursion bounded.
pub fn engine(max_recursion: usize) -> Engine {
    let mut config = Config::default();
    config.consume_fuel(true);
    config.set_max_recursion_depth(max_recursion);
    Engine::new(&config)
}

fn convert(error: wasmi::Error) -> Fault {
    Fault(match error.as_trap_code() {
        Some(code) => format!("trap: {code}"),
        None => error.to_string(),
    })
}

struct Data<H> {
    host: H,
    store_limits: StoreLimits,
    bounds: Bounds,
    memory: Option<Memory>,
    realloc: Option<Func>,
}

fn val_type(ty: Flat) -> ValType {
    match ty {
        Flat::I32 => ValType::I32,
        Flat::I64 => ValType::I64,
        Flat::F32 => ValType::F32,
        Flat::F64 => ValType::F64,
    }
}

fn func_type(params: &[Flat], results: &[Flat]) -> FuncType {
    FuncType::new(
        params.iter().copied().map(val_type),
        results.iter().copied().map(val_type),
    )
}

fn to_flat(val: &Val) -> Result<FlatVal, CanonError> {
    Ok(match val {
        Val::I32(v) => FlatVal::I32(*v),
        Val::I64(v) => FlatVal::I64(*v),
        Val::F32(v) => FlatVal::F32(f32::from(*v)),
        Val::F64(v) => FlatVal::F64(f64::from(*v)),
        _ => return Err(CanonError::TypeMismatch),
    })
}

fn to_val(flat: FlatVal) -> Val {
    match flat {
        FlatVal::I32(v) => Val::I32(v),
        FlatVal::I64(v) => Val::I64(v),
        FlatVal::F32(v) => Val::F32(v.into()),
        FlatVal::F64(v) => Val::F64(v.into()),
    }
}

fn trap(message: impl std::fmt::Display) -> wasmi::Error {
    wasmi::Error::new(message.to_string())
}

fn call_realloc(
    cx: impl wasmi::AsContextMut,
    realloc: Func,
    align: u32,
    size: u32,
) -> Result<u32, CanonError> {
    let mut cx = cx;
    let mut out = [Val::I32(0)];
    let args = [
        Val::I32(0),
        Val::I32(0),
        Val::I32(align as i32),
        Val::I32(size as i32),
    ];
    realloc
        .call(&mut cx, &args, &mut out)
        .map_err(|e| CanonError::Alloc(convert(e).0))?;
    match out[0] {
        Val::I32(ptr) => Ok(ptr as u32),
        _ => Err(CanonError::TypeMismatch),
    }
}

struct Cx<'a, 'c, H> {
    caller: &'a mut Caller<'c, Data<H>>,
    memory: Memory,
    realloc: Func,
}

impl<H> GuestMemory for Cx<'_, '_, H> {
    fn realloc(&mut self, align: u32, size: u32) -> Result<u32, CanonError> {
        call_realloc(&mut *self.caller, self.realloc, align, size)
    }

    fn memory(&mut self) -> &mut [u8] {
        self.memory.data_mut(&mut *self.caller)
    }
}

impl<H: Host> ImportCx<H> for Cx<'_, '_, H> {
    fn parts(&mut self) -> Option<(&[u8], &mut H, Bounds)> {
        let (mem, data) = self.memory.data_and_store_mut(&mut *self.caller);
        Some((&*mem, &mut data.host, data.bounds))
    }
}

pub struct Instance<H: Host> {
    store: Store<Data<H>>,
    memory: Memory,
    realloc: Func,
    exports: Vec<Option<(Func, Option<Func>)>>,
    fuel: u64,
}

impl<H: Host> Instance<H> {
    pub fn new(
        engine: &Engine,
        schema: &Arc<Schema>,
        wasm: &[u8],
        host: H,
        limits: Limits,
        bounds: Bounds,
    ) -> Result<Self, Fault> {
        let module = Module::new(engine, wasm).map_err(convert)?;
        let mut linker = Linker::<Data<H>>::new(engine);
        for (index, sig) in schema.imports.iter().enumerate() {
            let schema = schema.clone();
            linker
                .func_new(
                    &sig.module,
                    &sig.field,
                    func_type(&sig.core_params, &sig.core_results),
                    move |mut caller, params, results| {
                        let (Some(memory), Some(realloc)) =
                            (caller.data().memory, caller.data().realloc)
                        else {
                            return Err(trap("called before instantiation finished"));
                        };
                        let flat = params
                            .iter()
                            .map(to_flat)
                            .collect::<Result<Vec<_>, _>>()
                            .map_err(trap)?;
                        let mut cx = Cx {
                            caller: &mut caller,
                            memory,
                            realloc,
                        };
                        let out = runtime::import(&schema, index, &mut cx, &flat, results.len())
                            .map_err(trap)?;
                        for (slot, value) in results.iter_mut().zip(out) {
                            *slot = to_val(value);
                        }
                        Ok(())
                    },
                )
                .map_err(|e| fault("link", e))?;
        }
        let mut store = Store::new(
            engine,
            Data {
                host,
                store_limits: StoreLimitsBuilder::new()
                    .memory_size(limits.memory)
                    .memories(1)
                    .tables(4)
                    .table_elements(1 << 16)
                    .instances(1)
                    .build(),
                bounds,
                memory: None,
                realloc: None,
            },
        );
        store.limiter(|data| &mut data.store_limits);
        store.set_fuel(limits.fuel).map_err(convert)?;
        let instance = linker
            .instantiate_and_start(&mut store, &module)
            .map_err(convert)?;
        let memory = instance
            .get_export(&store, "memory")
            .and_then(Extern::into_memory)
            .ok_or_else(|| Fault("module exports no memory".into()))?;
        let realloc = instance
            .get_func(&store, REALLOC)
            .filter(|f| {
                f.ty(&store)
                    == func_type(&[Flat::I32, Flat::I32, Flat::I32, Flat::I32], &[Flat::I32])
            })
            .ok_or_else(|| {
                Fault(format!(
                    "module exports no {REALLOC}(i32, i32, i32, i32) -> i32"
                ))
            })?;
        let mut exports = Vec::new();
        for sig in &schema.exports {
            let Some(func) = instance.get_func(&store, &sig.field) else {
                exports.push(None);
                continue;
            };
            if func.ty(&store) != func_type(&sig.core_params, &sig.core_results) {
                return Err(Fault(format!("{} has the wrong type", sig.field)));
            }
            let post = match &sig.post_return {
                Some(name) => match instance.get_func(&store, name) {
                    Some(f) if f.ty(&store) == func_type(&sig.core_results, &[]) => Some(f),
                    Some(_) => return Err(Fault(format!("{name} has the wrong type"))),
                    None => None,
                },
                None => None,
            };
            exports.push(Some((func, post)));
        }
        store.data_mut().memory = Some(memory);
        store.data_mut().realloc = Some(realloc);
        Ok(Self {
            store,
            memory,
            realloc,
            exports,
            fuel: limits.fuel,
        })
    }

    pub fn host(&self) -> &H {
        &self.store.data().host
    }

    pub fn host_mut(&mut self) -> &mut H {
        &mut self.store.data_mut().host
    }

    pub fn bounds(&self) -> Bounds {
        self.store.data().bounds
    }

    pub fn has_export(&self, index: usize) -> bool {
        matches!(self.exports.get(index), Some(Some(_)))
    }

    pub fn reset_fuel(&mut self) -> Result<(), Fault> {
        self.store.set_fuel(self.fuel).map_err(convert)
    }

    pub fn realloc(&mut self, align: u32, size: u32) -> Result<u32, CanonError> {
        call_realloc(&mut self.store, self.realloc, align, size)
    }

    pub fn memory(&self) -> &[u8] {
        self.memory.data(&self.store)
    }

    pub fn memory_mut(&mut self) -> &mut [u8] {
        self.memory.data_mut(&mut self.store)
    }

    /// Calls export `index` (which must exist) with core arguments.
    pub fn invoke(&mut self, index: usize, args: &[FlatVal]) -> Result<Vec<FlatVal>, Fault> {
        let Some(Some((func, _))) = self.exports.get(index) else {
            return Err(Fault(format!("no export {index}")));
        };
        let func = *func;
        let inputs: Vec<Val> = args.iter().copied().map(to_val).collect();
        let ty = func.ty(&self.store);
        let mut outputs: Vec<Val> = ty
            .results()
            .iter()
            .map(|ty| Val::default_for_ty(*ty))
            .collect();
        func.call(&mut self.store, &inputs, &mut outputs)
            .map_err(convert)?;
        outputs
            .iter()
            .map(to_flat)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| fault("result", e))
    }

    pub fn post_return(&mut self, index: usize, results: &[FlatVal]) -> Result<(), Fault> {
        let Some(Some((_, Some(post)))) = self.exports.get(index) else {
            return Ok(());
        };
        let post = *post;
        let inputs: Vec<Val> = results.iter().copied().map(to_val).collect();
        post.call(&mut self.store, &inputs, &mut [])
            .map_err(convert)
    }
}
