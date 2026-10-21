//! Canonical-ABI layouts computed once from a WIT world: for every type a
//! function passes, its size, alignment, flattened core types and shape,
//! so lifting and lowering never consult the WIT again.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use wit_parser::abi::{AbiVariant, WasmType};
use wit_parser::{
    Function, FunctionKind, Int, LiftLowerAbi, ManglingAndAbi, Resolve, SizeAlign, SourceMap, Type,
    TypeDefKind, TypeId, WasmExport, WasmExportKind, WasmImport, WorldItem, WorldKey,
};

/// Flags types with more members than this are refused.
pub const MAX_FLAGS: usize = 64;

/// How deeply types may nest.
const MAX_DEPTH: usize = 32;

/// A core wasm value type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flat {
    I32,
    I64,
    F32,
    F64,
}

impl Flat {
    fn join(self, other: Flat) -> Flat {
        match (self, other) {
            (a, b) if a == b => a,
            (Flat::I32, Flat::F32) | (Flat::F32, Flat::I32) => Flat::I32,
            _ => Flat::I64,
        }
    }

    fn from_wasm(ty: WasmType) -> Flat {
        match ty {
            WasmType::I32 | WasmType::Pointer | WasmType::Length => Flat::I32,
            WasmType::I64 | WasmType::PointerOrI64 => Flat::I64,
            WasmType::F32 => Flat::F32,
            WasmType::F64 => Flat::F64,
        }
    }
}

/// How a value of one type is laid out in guest memory and as core values.
#[derive(Debug)]
pub struct Layout {
    pub shape: Shape,
    pub size: u32,
    pub align: u32,
    pub flat: Vec<Flat>,
}

#[derive(Debug)]
pub enum Shape {
    Bool,
    S8,
    U8,
    S16,
    U16,
    S32,
    U32,
    S64,
    U64,
    F32,
    F64,
    Char,
    String,
    List(Arc<Layout>),
    /// Records and tuples.
    Record(Vec<Field>),
    /// Variants, enums (no payloads), options (`none`, `some`) and results
    /// (`ok`, `err`).
    Variant(Variant),
    /// A flags type with this many members.
    Flags(u32),
}

#[derive(Debug)]
pub struct Field {
    pub offset: u32,
    pub layout: Arc<Layout>,
}

#[derive(Debug)]
pub struct Variant {
    /// Bytes of the discriminant in memory: 1, 2 or 4.
    pub tag_size: u32,
    pub payload_offset: u32,
    pub cases: Vec<Option<Arc<Layout>>>,
}

/// One function crossing the boundary, as the core module sees it.
#[derive(Debug)]
pub struct Signature {
    /// The WIT name, e.g. `register-command`.
    pub name: String,
    /// The core import module; empty for exports.
    pub module: String,
    /// The core import or export name.
    pub field: String,
    /// The export run after the host has read an export's results.
    pub post_return: Option<String>,
    pub params: Vec<Arc<Layout>>,
    /// The parameters as one tuple, for when they pass through memory.
    pub params_record: Arc<Layout>,
    pub result: Option<Arc<Layout>>,
    /// Parameters pass as one pointer to `params_record`.
    pub indirect_params: bool,
    /// The result passes through memory: imports take a trailing pointer to
    /// write it to, exports return a pointer to it.
    pub retptr: bool,
    pub core_params: Vec<Flat>,
    pub core_results: Vec<Flat>,
}

/// The functions of one WIT world.
#[derive(Debug)]
pub struct Schema {
    pub imports: Vec<Signature>,
    pub exports: Vec<Signature>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaError(pub String);

impl fmt::Display for SchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SchemaError {}

fn error(message: impl fmt::Display) -> SchemaError {
    SchemaError(message.to_string())
}

impl Schema {
    /// Parses the WIT package `source` and lays out world `world`, refusing
    /// anything outside the supported subset.
    pub fn parse(source: &str, world: &str) -> Result<Self, SchemaError> {
        Self::parse_files(&[("source.wit", source)], world)
    }

    /// Like [`Self::parse`] for a package spread over `(file name, text)`
    /// files.
    pub fn parse_files(files: &[(&str, &str)], world: &str) -> Result<Self, SchemaError> {
        let mut sources = SourceMap::new();
        for (name, text) in files {
            sources.push_str(name, *text);
        }
        let group = sources.parse().map_err(|(_, err)| error(err))?;
        let mut resolve = Resolve {
            all_features: true,
            ..Resolve::default()
        };
        let package = resolve.push_group(group).map_err(error)?;
        let world = resolve
            .select_world(&[package], Some(world))
            .map_err(error)?;
        let mut sizes = SizeAlign::default();
        sizes.fill(&resolve).map_err(error)?;
        let mut builder = Builder {
            resolve: &resolve,
            sizes: &sizes,
            cache: HashMap::new(),
        };
        let mangling = ManglingAndAbi::Legacy(LiftLowerAbi::Sync);
        let world = &resolve.worlds[world];

        let mut imports = Vec::new();
        for (key, func) in functions(&resolve, world.imports.iter()) {
            let (module, field) = resolve.wasm_import_name(
                mangling,
                WasmImport::Func {
                    interface: key,
                    func,
                },
            );
            imports.push(builder.signature(func, AbiVariant::GuestImport, module, field, None)?);
        }
        let mut exports = Vec::new();
        for (key, func) in functions(&resolve, world.exports.iter()) {
            let name = |kind| {
                resolve.wasm_export_name(
                    mangling,
                    WasmExport::Func {
                        interface: key,
                        func,
                        kind,
                    },
                )
            };
            let field = name(WasmExportKind::Normal);
            let post = name(WasmExportKind::PostReturn);
            exports.push(builder.signature(
                func,
                AbiVariant::GuestExport,
                String::new(),
                field,
                Some(post),
            )?);
        }
        Ok(Self { imports, exports })
    }

    pub fn import(&self, name: &str) -> Option<(usize, &Signature)> {
        self.imports
            .iter()
            .enumerate()
            .find(|(_, s)| s.name == name)
    }

    pub fn export(&self, name: &str) -> Option<(usize, &Signature)> {
        self.exports
            .iter()
            .enumerate()
            .find(|(_, s)| s.name == name)
    }
}

fn functions<'a>(
    resolve: &'a Resolve,
    items: impl Iterator<Item = (&'a WorldKey, &'a WorldItem)>,
) -> Vec<(Option<&'a WorldKey>, &'a Function)> {
    let mut out = Vec::new();
    for (key, item) in items {
        match item {
            WorldItem::Interface { id, .. } => {
                out.extend(
                    resolve.interfaces[*id]
                        .functions
                        .values()
                        .map(|f| (Some(key), f)),
                );
            }
            WorldItem::Function(func) => out.push((None, func)),
            WorldItem::Type { .. } => {}
        }
    }
    out
}

struct Builder<'a> {
    resolve: &'a Resolve,
    sizes: &'a SizeAlign,
    cache: HashMap<TypeId, Arc<Layout>>,
}

fn tag_bytes(tag: Int) -> u32 {
    match tag {
        Int::U8 => 1,
        Int::U16 => 2,
        Int::U32 => 4,
        Int::U64 => 8,
    }
}

fn u32_of(n: usize) -> Result<u32, SchemaError> {
    u32::try_from(n).map_err(|_| error("type too large"))
}

impl Builder<'_> {
    fn signature(
        &mut self,
        func: &Function,
        variant: AbiVariant,
        module: String,
        field: String,
        post_return: Option<String>,
    ) -> Result<Signature, SchemaError> {
        if !matches!(func.kind, FunctionKind::Freestanding) {
            return Err(error(format!(
                "{}: only plain synchronous functions are supported",
                func.name
            )));
        }
        let params = func
            .params
            .iter()
            .map(|p| self.layout(&p.ty, 0))
            .collect::<Result<Vec<_>, _>>()?;
        let result = func
            .result
            .as_ref()
            .map(|t| self.layout(t, 0))
            .transpose()?;
        let params_record = self.record(func.params.iter().map(|p| &p.ty), 0)?;
        let wasm = self.resolve.wasm_signature(variant, func);
        let core_params: Vec<Flat> = wasm.params.iter().copied().map(Flat::from_wasm).collect();
        let core_results: Vec<Flat> = wasm.results.iter().copied().map(Flat::from_wasm).collect();

        let mut expected = if wasm.indirect_params {
            vec![Flat::I32]
        } else {
            params_record.flat.clone()
        };
        let mut expected_results = Vec::new();
        match (&result, wasm.retptr, variant) {
            (_, true, AbiVariant::GuestImport) => expected.push(Flat::I32),
            (_, true, _) => expected_results.push(Flat::I32),
            (Some(result), false, _) => expected_results.extend(&result.flat),
            (None, false, _) => {}
        }
        if expected != core_params || expected_results != core_results {
            return Err(error(format!(
                "{}: flattened signature disagrees with the reference",
                func.name
            )));
        }
        Ok(Signature {
            name: func.name.clone(),
            module,
            field,
            post_return: post_return.filter(|_| result.is_some()),
            params,
            params_record,
            result,
            indirect_params: wasm.indirect_params,
            retptr: wasm.retptr,
            core_params,
            core_results,
        })
    }

    fn record<'t>(
        &mut self,
        types: impl Iterator<Item = &'t Type> + Clone,
        depth: usize,
    ) -> Result<Arc<Layout>, SchemaError> {
        let info = self.sizes.record(types.clone());
        let mut fields = Vec::new();
        let mut flat = Vec::new();
        for (offset, ty) in self.sizes.field_offsets(types) {
            let layout = self.layout(ty, depth + 1)?;
            flat.extend(&layout.flat);
            fields.push(Field {
                offset: u32_of(offset.size_wasm32())?,
                layout,
            });
        }
        Ok(Arc::new(Layout {
            shape: Shape::Record(fields),
            size: u32_of(info.size.size_wasm32())?,
            align: u32_of(info.align.align_wasm32())?,
            flat,
        }))
    }

    fn variant<'t>(
        &mut self,
        tag: Int,
        cases: impl Iterator<Item = Option<&'t Type>> + Clone,
        depth: usize,
    ) -> Result<Shape, SchemaError> {
        let payload_offset = self.sizes.payload_offset(tag, cases.clone());
        let cases = cases
            .map(|c| c.map(|t| self.layout(t, depth + 1)).transpose())
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Shape::Variant(Variant {
            tag_size: tag_bytes(tag),
            payload_offset: u32_of(payload_offset.size_wasm32())?,
            cases,
        }))
    }

    fn layout(&mut self, ty: &Type, depth: usize) -> Result<Arc<Layout>, SchemaError> {
        if depth > MAX_DEPTH {
            return Err(error("types nest too deeply"));
        }
        let id = match ty {
            Type::Id(id) => *id,
            Type::ErrorContext => return Err(error("error-context is not supported")),
            _ => {
                let shape = match ty {
                    Type::Bool => Shape::Bool,
                    Type::S8 => Shape::S8,
                    Type::U8 => Shape::U8,
                    Type::S16 => Shape::S16,
                    Type::U16 => Shape::U16,
                    Type::S32 => Shape::S32,
                    Type::U32 => Shape::U32,
                    Type::S64 => Shape::S64,
                    Type::U64 => Shape::U64,
                    Type::F32 => Shape::F32,
                    Type::F64 => Shape::F64,
                    Type::Char => Shape::Char,
                    _ => Shape::String,
                };
                return self.finish(ty, shape);
            }
        };
        if let Some(layout) = self.cache.get(&id) {
            return Ok(layout.clone());
        }
        let def = &self.resolve.types[id];
        let layout = match &def.kind {
            TypeDefKind::Type(inner) => self.layout(inner, depth + 1)?,
            TypeDefKind::Record(r) => {
                let record = self.record(r.fields.iter().map(|f| &f.ty), depth)?;
                self.check(ty, &record)?;
                record
            }
            TypeDefKind::Tuple(t) => {
                let record = self.record(t.types.iter(), depth)?;
                self.check(ty, &record)?;
                record
            }
            TypeDefKind::Flags(f) => {
                if f.flags.len() > MAX_FLAGS {
                    return Err(error(format!("flags with more than {MAX_FLAGS} members")));
                }
                self.finish(ty, Shape::Flags(u32_of(f.flags.len())?))?
            }
            TypeDefKind::Variant(v) => {
                let shape = self.variant(v.tag(), v.cases.iter().map(|c| c.ty.as_ref()), depth)?;
                self.finish(ty, shape)?
            }
            TypeDefKind::Enum(e) => {
                let shape = self.variant(e.tag(), e.cases.iter().map(|_| None), depth)?;
                self.finish(ty, shape)?
            }
            TypeDefKind::Option(t) => {
                let shape = self.variant(Int::U8, [None, Some(t)].into_iter(), depth)?;
                self.finish(ty, shape)?
            }
            TypeDefKind::Result(r) => {
                let shape =
                    self.variant(Int::U8, [r.ok.as_ref(), r.err.as_ref()].into_iter(), depth)?;
                self.finish(ty, shape)?
            }
            TypeDefKind::List(t) => {
                let element = self.layout(t, depth + 1)?;
                self.finish(ty, Shape::List(element))?
            }
            other => return Err(error(format!("{} is not supported", other.as_str()))),
        };
        self.cache.insert(id, layout.clone());
        Ok(layout)
    }

    fn check(&self, ty: &Type, layout: &Layout) -> Result<(), SchemaError> {
        let size = u32_of(self.sizes.size(ty).size_wasm32())?;
        let align = u32_of(self.sizes.align(ty).align_wasm32())?;
        if (size, align) != (layout.size, layout.align) {
            return Err(error("record layout disagrees with the reference"));
        }
        Ok(())
    }

    fn finish(&self, ty: &Type, shape: Shape) -> Result<Arc<Layout>, SchemaError> {
        let flat = flatten(&shape);
        Ok(Arc::new(Layout {
            size: u32_of(self.sizes.size(ty).size_wasm32())?,
            align: u32_of(self.sizes.align(ty).align_wasm32())?,
            shape,
            flat,
        }))
    }
}

fn flatten(shape: &Shape) -> Vec<Flat> {
    match shape {
        Shape::Bool
        | Shape::S8
        | Shape::U8
        | Shape::S16
        | Shape::U16
        | Shape::S32
        | Shape::U32
        | Shape::Char => vec![Flat::I32],
        Shape::S64 | Shape::U64 => vec![Flat::I64],
        Shape::F32 => vec![Flat::F32],
        Shape::F64 => vec![Flat::F64],
        Shape::String | Shape::List(_) => vec![Flat::I32, Flat::I32],
        Shape::Record(fields) => fields
            .iter()
            .flat_map(|f| f.layout.flat.iter().copied())
            .collect(),
        Shape::Flags(0) => Vec::new(),
        Shape::Flags(n) if *n > 32 => vec![Flat::I32, Flat::I32],
        Shape::Flags(_) => vec![Flat::I32],
        Shape::Variant(variant) => {
            let mut joined: Vec<Flat> = Vec::new();
            for case in variant.cases.iter().flatten() {
                for (i, ty) in case.flat.iter().enumerate() {
                    match joined.get_mut(i) {
                        Some(slot) => *slot = slot.join(*ty),
                        None => joined.push(*ty),
                    }
                }
            }
            let mut flat = vec![Flat::I32];
            flat.extend(joined);
            flat
        }
    }
}
