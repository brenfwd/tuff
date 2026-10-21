//! Generates typed host bindings for `wit/` (the `plugin` world): a Rust
//! type with [`FromValue`]/[`ToValue`] conversions for every WIT type, an
//! `Imports` trait the host implements with a dispatcher for the runtime,
//! and a typed function per guest export.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::Path;

use wit_parser::{
    Docs, Function, FunctionKind, Handle, InterfaceId, LiftLowerAbi, ManglingAndAbi, Resolve,
    SourceMap, Type, TypeDefKind, TypeId, TypeOwner, WasmExport, WasmExportKind, WasmImport,
    WorldItem, WorldKey,
};

/// The package's files, in the order the runtime parses them too.
const SOURCES: [&str; 2] = ["capabilities.wit", "plugin.wit"];
const WORLD: &str = "plugin-worldgen";

const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use",
    "where", "while", "abstract", "become", "box", "do", "final", "macro", "override", "priv",
    "try", "typeof", "unsized", "virtual", "yield", "gen",
];

fn camel(name: &str) -> String {
    name.split('-')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

fn snake(name: &str) -> String {
    let snake = name.replace('-', "_");
    if KEYWORDS.contains(&snake.as_str()) {
        format!("r#{snake}")
    } else {
        snake
    }
}

fn shouty(name: &str) -> String {
    name.replace('-', "_").to_ascii_uppercase()
}

fn docs(out: &mut String, docs: &Docs, indent: &str) {
    if let Some(text) = &docs.contents {
        for line in text.lines() {
            let line = line.trim_end();
            if line.is_empty() {
                let _ = writeln!(out, "{indent}///");
            } else {
                let _ = writeln!(out, "{indent}/// {line}");
            }
        }
    }
}

struct Gen<'a> {
    resolve: &'a Resolve,
    out: String,
}

impl Gen<'_> {
    /// Follows `use`d and aliased ids to the definition that names a type.
    fn root(&self, id: TypeId) -> TypeId {
        match &self.resolve.types[id].kind {
            TypeDefKind::Type(Type::Id(inner)) => self.root(*inner),
            _ => id,
        }
    }

    /// Whether values of `ty` can be `Copy`: scalars, and records,
    /// tuples, options, enums and flags of them.
    fn copy(&self, ty: &Type) -> bool {
        match ty {
            Type::String | Type::ErrorContext => false,
            Type::Id(id) => match &self.resolve.types[*id].kind {
                TypeDefKind::Record(r) => r.fields.iter().all(|f| self.copy(&f.ty)),
                TypeDefKind::Tuple(t) => t.types.iter().all(|t| self.copy(t)),
                TypeDefKind::Option(t) | TypeDefKind::Type(t) => self.copy(t),
                TypeDefKind::Enum(_) | TypeDefKind::Flags(_) => true,
                _ => false,
            },
            _ => true,
        }
    }

    fn module_of(&self, interface: InterfaceId) -> String {
        snake(
            self.resolve.interfaces[interface]
                .name
                .as_deref()
                .unwrap_or("anonymous"),
        )
    }

    fn ty(&self, ty: &Type) -> String {
        match ty {
            Type::Bool => "bool".into(),
            Type::U8 => "u8".into(),
            Type::U16 => "u16".into(),
            Type::U32 => "u32".into(),
            Type::U64 => "u64".into(),
            Type::S8 => "i8".into(),
            Type::S16 => "i16".into(),
            Type::S32 => "i32".into(),
            Type::S64 => "i64".into(),
            Type::F32 => "f32".into(),
            Type::F64 => "f64".into(),
            Type::Char => "char".into(),
            Type::String => "String".into(),
            Type::ErrorContext => panic!("error-context is not supported"),
            Type::Id(id) => self.id(*id),
        }
    }

    fn id(&self, id: TypeId) -> String {
        let id = self.root(id);
        let def = &self.resolve.types[id];
        if let (Some(name), TypeOwner::Interface(owner)) = (&def.name, def.owner) {
            return format!(
                "crate::wit::bindings::{}::{}",
                self.module_of(owner),
                camel(name)
            );
        }
        match &def.kind {
            TypeDefKind::Option(inner) => format!("Option<{}>", self.ty(inner)),
            TypeDefKind::Result(r) => format!(
                "Result<{}, {}>",
                r.ok.as_ref().map_or("()".into(), |t| self.ty(t)),
                r.err.as_ref().map_or("()".into(), |t| self.ty(t)),
            ),
            TypeDefKind::List(inner) => format!("Vec<{}>", self.ty(inner)),
            TypeDefKind::Tuple(t) => {
                let items: Vec<String> = t.types.iter().map(|t| self.ty(t)).collect();
                format!("({},)", items.join(", "))
            }
            TypeDefKind::Type(inner) => self.ty(inner),
            other => panic!("unsupported anonymous type {}", other.as_str()),
        }
    }

    fn typedef(&mut self, id: TypeId) {
        let def = &self.resolve.types[id];
        let name = camel(def.name.as_deref().expect("named"));
        let what = def.name.clone().unwrap();
        let mut out = String::new();
        docs(&mut out, &def.docs, "    ");
        match &def.kind {
            TypeDefKind::Type(Type::Id(_)) => return,
            TypeDefKind::Type(inner) => {
                let _ = writeln!(out, "    pub type {name} = {};", self.ty(inner));
            }
            TypeDefKind::Record(record) => {
                let copy = if self.copy(&Type::Id(id)) {
                    "Copy, "
                } else {
                    ""
                };
                let _ = writeln!(out, "    #[derive(Clone, {copy}Debug, PartialEq)]");
                let _ = writeln!(out, "    pub struct {name} {{");
                for field in &record.fields {
                    docs(&mut out, &field.docs, "        ");
                    let _ = writeln!(
                        out,
                        "        pub {}: {},",
                        snake(&field.name),
                        self.ty(&field.ty)
                    );
                }
                let _ = writeln!(out, "    }}\n");
                let count = record.fields.len();
                let _ = writeln!(out, "    impl FromValue for {name} {{");
                let _ = writeln!(
                    out,
                    "        fn from_value(value: Value<'_>) -> value::Result<Self> {{"
                );
                let _ = writeln!(out, "            #[allow(unused_mut, unused_variables)]");
                let _ = writeln!(
                    out,
                    "            let mut fields = value::record(value, {count}, \"{what}\")?;"
                );
                let _ = writeln!(out, "            Ok(Self {{");
                for field in &record.fields {
                    let _ = writeln!(
                        out,
                        "                {}: value::field(&mut fields)?,",
                        snake(&field.name)
                    );
                }
                let _ = writeln!(out, "            }})\n        }}\n    }}\n");
                let _ = writeln!(out, "    impl ToValue for {name} {{");
                let _ = writeln!(out, "        fn to_value(&self) -> Value<'_> {{");
                let fields: Vec<String> = record
                    .fields
                    .iter()
                    .map(|f| format!("self.{}.to_value()", snake(&f.name)))
                    .collect();
                let _ = writeln!(
                    out,
                    "            Value::Record(vec![{}])",
                    fields.join(", ")
                );
                let _ = writeln!(out, "        }}\n    }}");
            }
            TypeDefKind::Variant(variant) => {
                let _ = writeln!(out, "    #[derive(Clone, Debug, PartialEq)]");
                let _ = writeln!(out, "    pub enum {name} {{");
                for case in &variant.cases {
                    docs(&mut out, &case.docs, "        ");
                    match &case.ty {
                        Some(ty) => {
                            let _ =
                                writeln!(out, "        {}({}),", camel(&case.name), self.ty(ty));
                        }
                        None => {
                            let _ = writeln!(out, "        {},", camel(&case.name));
                        }
                    }
                }
                let _ = writeln!(out, "    }}\n");
                let _ = writeln!(out, "    impl FromValue for {name} {{");
                let _ = writeln!(
                    out,
                    "        fn from_value(value: Value<'_>) -> value::Result<Self> {{"
                );
                let _ = writeln!(out, "            #[allow(unused_variables)]");
                let _ = writeln!(
                    out,
                    "            let (case, payload) = value::variant(value, \"{what}\")?;"
                );
                let _ = writeln!(out, "            Ok(match case {{");
                for (i, case) in variant.cases.iter().enumerate() {
                    let case_name = camel(&case.name);
                    match &case.ty {
                        Some(_) => {
                            let _ = writeln!(
                                out,
                                "                {i} => Self::{case_name}(value::case(payload)?),"
                            );
                        }
                        None => {
                            let _ = writeln!(out, "                {i} => Self::{case_name},");
                        }
                    }
                }
                let _ = writeln!(
                    out,
                    "                _ => return value::mismatch(\"{what}\"),"
                );
                let _ = writeln!(out, "            }})\n        }}\n    }}\n");
                let _ = writeln!(out, "    impl ToValue for {name} {{");
                let _ = writeln!(out, "        fn to_value(&self) -> Value<'_> {{");
                let _ = writeln!(out, "            match self {{");
                for (i, case) in variant.cases.iter().enumerate() {
                    let case_name = camel(&case.name);
                    match &case.ty {
                        Some(_) => {
                            let _ = writeln!(
                                out,
                                "                Self::{case_name}(payload) => Value::Variant({i}, Some(Box::new(payload.to_value()))),"
                            );
                        }
                        None => {
                            let _ = writeln!(
                                out,
                                "                Self::{case_name} => Value::Variant({i}, None),"
                            );
                        }
                    }
                }
                let _ = writeln!(out, "            }}\n        }}\n    }}");
            }
            TypeDefKind::Enum(e) => {
                let _ = writeln!(
                    out,
                    "    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]"
                );
                let _ = writeln!(out, "    pub enum {name} {{");
                for case in &e.cases {
                    docs(&mut out, &case.docs, "        ");
                    let _ = writeln!(out, "        {},", camel(&case.name));
                }
                let _ = writeln!(out, "    }}\n");
                let _ = writeln!(out, "    impl {name} {{");
                let cases: Vec<String> = e
                    .cases
                    .iter()
                    .map(|c| format!("Self::{}", camel(&c.name)))
                    .collect();
                let _ = writeln!(
                    out,
                    "        pub const ALL: [Self; {}] = [{}];",
                    cases.len(),
                    cases.join(", ")
                );
                let _ = writeln!(out, "    }}\n");
                let _ = writeln!(out, "    impl FromValue for {name} {{");
                let _ = writeln!(
                    out,
                    "        fn from_value(value: Value<'_>) -> value::Result<Self> {{"
                );
                let _ = writeln!(
                    out,
                    "            match value::variant(value, \"{what}\")? {{"
                );
                let _ = writeln!(
                    out,
                    "                (case, None) => Self::ALL.get(case as usize).copied().ok_or_else(|| \"{what} out of range\".to_owned()),"
                );
                let _ = writeln!(out, "                _ => value::mismatch(\"{what}\"),");
                let _ = writeln!(out, "            }}\n        }}\n    }}\n");
                let _ = writeln!(out, "    impl ToValue for {name} {{");
                let _ = writeln!(out, "        fn to_value(&self) -> Value<'_> {{");
                let _ = writeln!(out, "            Value::Variant(*self as u32, None)");
                let _ = writeln!(out, "        }}\n    }}");
            }
            TypeDefKind::Flags(flags) => {
                assert!(flags.flags.len() <= 64, "flags with more than 64 members");
                let _ = writeln!(
                    out,
                    "    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]"
                );
                let _ = writeln!(out, "    pub struct {name}(u64);\n");
                let _ = writeln!(out, "    impl {name} {{");
                for (i, flag) in flags.flags.iter().enumerate() {
                    docs(&mut out, &flag.docs, "        ");
                    let _ = writeln!(
                        out,
                        "        pub const {}: Self = Self(1 << {i});",
                        shouty(&flag.name)
                    );
                }
                let all = if flags.flags.len() == 64 {
                    u64::MAX
                } else {
                    (1u64 << flags.flags.len()) - 1
                };
                let _ = writeln!(out, "        pub const ALL: Self = Self({all:#x});");
                let _ = writeln!(out, "        pub const fn empty() -> Self {{ Self(0) }}");
                let _ = writeln!(out, "        pub const fn bits(self) -> u64 {{ self.0 }}");
                let _ = writeln!(out, "        /// Keeps only the bits that name a member.");
                let _ = writeln!(
                    out,
                    "        pub const fn from_bits_truncate(bits: u64) -> Self {{ Self(bits & {all:#x}) }}"
                );
                let _ = writeln!(
                    out,
                    "        pub const fn contains(self, other: Self) -> bool {{ self.0 & other.0 == other.0 }}"
                );
                let _ = writeln!(
                    out,
                    "        pub const fn intersects(self, other: Self) -> bool {{ self.0 & other.0 != 0 }}"
                );
                let _ = writeln!(
                    out,
                    "        pub const fn is_empty(self) -> bool {{ self.0 == 0 }}"
                );
                let _ = writeln!(out, "    }}\n");
                let _ = writeln!(out, "    impl std::ops::BitOr for {name} {{");
                let _ = writeln!(out, "        type Output = Self;");
                let _ = writeln!(
                    out,
                    "        fn bitor(self, other: Self) -> Self {{ Self(self.0 | other.0) }}"
                );
                let _ = writeln!(out, "    }}\n");
                let _ = writeln!(out, "    impl std::ops::BitOrAssign for {name} {{");
                let _ = writeln!(
                    out,
                    "        fn bitor_assign(&mut self, other: Self) {{ self.0 |= other.0; }}"
                );
                let _ = writeln!(out, "    }}\n");
                let _ = writeln!(out, "    impl FromValue for {name} {{");
                let _ = writeln!(
                    out,
                    "        fn from_value(value: Value<'_>) -> value::Result<Self> {{"
                );
                let _ = writeln!(
                    out,
                    "            value::flags(value, \"{what}\").map(Self::from_bits_truncate)"
                );
                let _ = writeln!(out, "        }}\n    }}\n");
                let _ = writeln!(out, "    impl ToValue for {name} {{");
                let _ = writeln!(out, "        fn to_value(&self) -> Value<'_> {{");
                let _ = writeln!(out, "            Value::Flags(self.0)");
                let _ = writeln!(out, "        }}\n    }}");
            }
            TypeDefKind::Handle(Handle::Own(_) | Handle::Borrow(_)) | TypeDefKind::Resource => {
                panic!("resources are not supported")
            }
            other => panic!("unsupported named type {}", other.as_str()),
        }
        self.out.push_str(&out);
        self.out.push('\n');
    }

    fn interface_types(&mut self, interface: InterfaceId) {
        let iface = &self.resolve.interfaces[interface];
        let own: Vec<TypeId> = iface
            .types
            .values()
            .copied()
            .filter(|id| !matches!(self.resolve.types[*id].kind, TypeDefKind::Type(Type::Id(_))))
            .collect();
        if own.is_empty() {
            return;
        }
        let _ = writeln!(self.out, "pub mod {} {{", self.module_of(interface));
        self.out.push_str("    #![allow(clippy::all, dead_code)]\n");
        self.out.push_str("    use crate::wit::canon::Value;\n");
        self.out
            .push_str("    use crate::wit::value::{self, FromValue, ToValue};\n\n");
        for id in own {
            self.typedef(id);
        }
        self.out.push_str("}\n\n");
    }
}

fn functions<'a>(
    resolve: &'a Resolve,
    items: impl Iterator<Item = (&'a WorldKey, &'a WorldItem)>,
) -> Vec<(Option<&'a WorldKey>, Option<InterfaceId>, &'a Function)> {
    let mut out = Vec::new();
    for (key, item) in items {
        match item {
            WorldItem::Interface { id, .. } => out.extend(
                resolve.interfaces[*id]
                    .functions
                    .values()
                    .map(|f| (Some(key), Some(*id), f)),
            ),
            WorldItem::Function(func) => out.push((None, None, func)),
            WorldItem::Type { .. } => {}
        }
    }
    out
}

fn prefix(resolve: &Resolve, interface: Option<InterfaceId>, func: &Function) -> String {
    let iface = interface
        .and_then(|id| resolve.interfaces[id].name.clone())
        .map(|name| format!("{}_", name.replace('-', "_")))
        .unwrap_or_default();
    format!("{iface}{}", func.name.replace('-', "_"))
}

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("wit");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut sources = SourceMap::new();
    for file in SOURCES {
        let path = dir.join(file);
        println!("cargo:rerun-if-changed={}", path.display());
        let text = std::fs::read_to_string(&path).expect("reading the WIT");
        sources.push_str(file, text);
    }
    let group = sources.parse().map_err(|(_, err)| err).expect("valid WIT");
    let mut resolve = Resolve {
        all_features: true,
        ..Resolve::default()
    };
    let package = resolve.push_group(group).expect("resolvable WIT");
    let world = resolve
        .select_world(&[package], Some(WORLD))
        .expect("the plugin world");

    let mut generator = Gen {
        resolve: &resolve,
        out: String::new(),
    };
    generator
        .out
        .push_str("// Generated by build.rs from wit/; do not edit.\n\n");

    let mut seen = HashSet::new();
    for (_, item) in resolve.worlds[world]
        .imports
        .iter()
        .chain(&resolve.worlds[world].exports)
    {
        if let WorldItem::Interface { id, .. } = item {
            let mut queue = vec![*id];
            while let Some(id) = queue.pop() {
                if !seen.insert(id) {
                    continue;
                }
                for ty in resolve.interfaces[id].types.values() {
                    if let TypeDefKind::Type(Type::Id(inner)) = resolve.types[*ty].kind
                        && let TypeOwner::Interface(owner) =
                            resolve.types[generator.root(inner)].owner
                    {
                        queue.push(owner);
                    }
                }
            }
        }
    }
    let mut interfaces: Vec<InterfaceId> = seen.into_iter().collect();
    interfaces.sort_by_key(|id| resolve.interfaces[*id].name.clone());
    for id in interfaces {
        generator.interface_types(id);
    }

    let mangling = ManglingAndAbi::Legacy(LiftLowerAbi::Sync);
    let imports = functions(&resolve, resolve.worlds[world].imports.iter());
    let exports = functions(&resolve, resolve.worlds[world].exports.iter());
    for (_, _, func) in imports.iter().chain(&exports) {
        assert!(
            matches!(func.kind, FunctionKind::Freestanding),
            "{}: only plain functions are supported",
            func.name
        );
    }

    let out = &mut generator.out;
    out.push_str("#[allow(unused_imports)]\nuse crate::wit::canon::Value;\n");
    out.push_str(
        "#[allow(unused_imports)]\nuse crate::wit::value::{self, FromValue, ToValue};\n\n",
    );

    let names: Vec<String> = imports
        .iter()
        .map(|(key, _, func)| {
            let (module, field) = resolve.wasm_import_name(
                mangling,
                WasmImport::Func {
                    interface: *key,
                    func,
                },
            );
            format!("(\"{module}\", \"{field}\")")
        })
        .collect();
    let _ = writeln!(
        out,
        "/// Every import as `(module, field)`, in the runtime schema's order.\npub const IMPORTS: [(&str, &str); {}] = [{}];\n",
        names.len(),
        names.join(", ")
    );
    let fields: Vec<String> = exports
        .iter()
        .map(|(key, _, func)| {
            let field = resolve.wasm_export_name(
                mangling,
                WasmExport::Func {
                    interface: *key,
                    func,
                    kind: WasmExportKind::Normal,
                },
            );
            format!("\"{field}\"")
        })
        .collect();
    let _ = writeln!(
        out,
        "/// Every export's core name, in the runtime schema's order.\npub const EXPORTS: [&str; {}] = [{}];\n",
        fields.len(),
        fields.join(", ")
    );

    let ty = |t: &Type| gen_ty(&resolve, t);

    out.push_str("/// The `plugin` world's imports, as the host answers them. `Err` traps.\npub trait Imports {\n");
    for (_, iface, func) in &imports {
        docs(out, &func.docs, "    ");
        let params: Vec<String> = func
            .params
            .iter()
            .map(|p| format!("{}: {}", snake(&p.name), param_ty(&resolve, &p.ty)))
            .collect();
        let result = func.result.as_ref().map_or("()".into(), &ty);
        let _ = writeln!(
            out,
            "    fn {}(&mut self{}{}) -> value::Result<{result}>;",
            prefix(&resolve, *iface, func),
            if params.is_empty() { "" } else { ", " },
            params.join(", ")
        );
    }
    out.push_str("}\n\n");

    out.push_str("/// Lifts import `index`'s arguments, runs it on `host` and boxes its answer.\n");
    out.push_str("pub fn dispatch<H: Imports + ?Sized>(\n    host: &mut H,\n    index: usize,\n    args: Vec<Value<'_>>,\n) -> value::Result<Option<Box<dyn ToValue>>> {\n");
    out.push_str("    #[allow(unused_mut, unused_variables)]\n    let mut args = args.into_iter();\n    match index {\n");
    for (i, (_, iface, func)) in imports.iter().enumerate() {
        let _ = writeln!(out, "        {i} => {{");
        let mut call_args = Vec::new();
        for p in &func.params {
            let name = format!("arg_{}", p.name.replace('-', "_"));
            match borrowed(&resolve, &p.ty) {
                Some("str") => {
                    let _ = writeln!(
                        out,
                        "            let {name} = value::str_arg(value::arg(&mut args)?)?;"
                    );
                    call_args.push(format!("&{name}"));
                }
                Some(_) => {
                    let _ = writeln!(
                        out,
                        "            let {name} = value::bytes_arg(value::arg(&mut args)?)?;"
                    );
                    call_args.push(format!("&{name}"));
                }
                None => {
                    let _ = writeln!(
                        out,
                        "            let {name}: {} = FromValue::from_value(value::arg(&mut args)?)?;",
                        ty(&p.ty)
                    );
                    call_args.push(name);
                }
            }
        }
        let call = format!(
            "host.{}({})",
            prefix(&resolve, *iface, func),
            call_args.join(", ")
        );
        if func.result.is_some() {
            let _ = writeln!(out, "            Ok(Some(Box::new({call}?)))");
        } else {
            let _ = writeln!(out, "            {call}?;\n            Ok(None)");
        }
        out.push_str("        }\n");
    }
    out.push_str("        _ => Err(format!(\"no import {index}\")),\n    }\n}\n\n");

    for (i, (_, iface, func)) in exports.iter().enumerate() {
        let name = prefix(&resolve, *iface, func);
        let _ = writeln!(
            out,
            "/// The index of export `{name}`.\npub const {}: usize = {i};\n",
            shouty(&name)
        );
        docs(out, &func.docs, "");
        let params: Vec<String> = func
            .params
            .iter()
            .map(|p| format!("{}: &{}", snake(&p.name), export_param_ty(&resolve, &p.ty)))
            .collect();
        let values: Vec<String> = func
            .params
            .iter()
            .map(|p| format!("{}.to_value()", snake(&p.name)))
            .collect();
        let result = func.result.as_ref().map_or("()".into(), &ty);
        let _ = writeln!(
            out,
            "pub fn {name}<H: crate::wit::runtime::Host>(\n    runtime: &mut crate::wit::runtime::Runtime<H>{}{}\n) -> Result<{result}, crate::sandbox::Fault> {{",
            if params.is_empty() { "" } else { ",\n    " },
            params.join(",\n    ")
        );
        let lift = if func.result.is_some() {
            "|value| FromValue::from_value(value.ok_or(\"no result\")?)"
        } else {
            "|_| Ok(())"
        };
        let _ = writeln!(
            out,
            "    runtime.call({}, &[{}], {lift})\n}}\n",
            shouty(&name),
            values.join(", ")
        );
    }

    let path = Path::new(&std::env::var("OUT_DIR").unwrap()).join("bindings.rs");
    std::fs::write(path, generator.out).expect("writing the bindings");
}

fn gen_ty(resolve: &Resolve, ty: &Type) -> String {
    Gen {
        resolve,
        out: String::new(),
    }
    .ty(ty)
}

/// `str` or `[u8]` when a parameter of this type passes borrowed.
fn borrowed(resolve: &Resolve, ty: &Type) -> Option<&'static str> {
    match ty {
        Type::String => Some("str"),
        Type::Id(id) => match &resolve.types[*id].kind {
            TypeDefKind::List(Type::U8) => Some("[u8]"),
            TypeDefKind::Type(inner) => borrowed(resolve, inner),
            _ => None,
        },
        _ => None,
    }
}

fn param_ty(resolve: &Resolve, ty: &Type) -> String {
    match borrowed(resolve, ty) {
        Some(unsized_ty) => format!("&{unsized_ty}"),
        None => gen_ty(resolve, ty),
    }
}

fn export_param_ty(resolve: &Resolve, ty: &Type) -> String {
    borrowed(resolve, ty).map_or_else(|| gen_ty(resolve, ty), str::to_owned)
}
