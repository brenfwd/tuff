// SPDX-License-Identifier: MIT
//! C++ output of interface mode: structs and tagged variants over
//! `tuff/wire.hpp`, typed `Async` client methods and a `serve<S>()` table.
//! The wire layout and the method keys are the Rust generator's (`func_info`
//! and `canon` are shared), so the two sides interoperate byte for byte.

use std::collections::HashSet;
use std::fmt::Write as _;

use anyhow::{Result, bail};
use wit_parser::{FunctionKind, Type, TypeDefKind, TypeId, TypeOwner};

use super::{FuncInfo, Iface};
use crate::surface::{pascal, snake, upper};

const KEYWORDS: &[&str] = &[
    "alignas",
    "alignof",
    "and",
    "asm",
    "auto",
    "bitand",
    "bitor",
    "bool",
    "break",
    "case",
    "catch",
    "char",
    "class",
    "compl",
    "concept",
    "const",
    "consteval",
    "constexpr",
    "constinit",
    "continue",
    "co_await",
    "co_return",
    "co_yield",
    "decltype",
    "default",
    "delete",
    "do",
    "double",
    "else",
    "enum",
    "explicit",
    "export",
    "extern",
    "false",
    "float",
    "for",
    "friend",
    "goto",
    "if",
    "inline",
    "int",
    "long",
    "mutable",
    "namespace",
    "new",
    "noexcept",
    "not",
    "nullptr",
    "operator",
    "or",
    "private",
    "protected",
    "public",
    "register",
    "requires",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "struct",
    "switch",
    "template",
    "this",
    "throw",
    "true",
    "try",
    "typedef",
    "typeid",
    "typename",
    "union",
    "unsigned",
    "using",
    "virtual",
    "void",
    "volatile",
    "while",
    "xor",
];

pub(super) fn ident(name: &str) -> String {
    let s = snake(name);
    if KEYWORDS.contains(&s.as_str()) {
        format!("{s}_")
    } else {
        s
    }
}

impl Iface<'_> {
    fn cpp_ref(&self, ty: &Type) -> Result<String> {
        Ok(match ty {
            Type::Bool => "bool".into(),
            Type::U8 => "uint8_t".into(),
            Type::U16 => "uint16_t".into(),
            Type::U32 => "uint32_t".into(),
            Type::U64 => "uint64_t".into(),
            Type::S8 => "int8_t".into(),
            Type::S16 => "int16_t".into(),
            Type::S32 => "int32_t".into(),
            Type::S64 => "int64_t".into(),
            Type::F32 => "float".into(),
            Type::F64 => "double".into(),
            Type::Char => "char32_t".into(),
            Type::String => "::tuff::String".into(),
            Type::ErrorContext => bail!("error-context is not supported in plugin interfaces"),
            Type::Id(id) => {
                let def = self.def(*id);
                if let Some(name) = &def.name {
                    let owner = match def.owner {
                        TypeOwner::Interface(owner) => owner,
                        _ => self.id,
                    };
                    let module = self.resolve.interfaces[owner].name.as_deref().unwrap_or("");
                    return Ok(format!("::{}::{}", ident(module), pascal(name)));
                }
                self.cpp_anon(&def.kind)?
            }
        })
    }

    fn cpp_opt(&self, ty: Option<&Type>) -> Result<String> {
        match ty {
            Some(t) => self.cpp_ref(t),
            None => Ok("::tuff::Unit".into()),
        }
    }

    fn cpp_anon(&self, kind: &TypeDefKind) -> Result<String> {
        Ok(match kind {
            TypeDefKind::List(t) => format!("::tuff::List<{}>", self.cpp_ref(t)?),
            TypeDefKind::Option(t) => format!("::tuff::Option<{}>", self.cpp_ref(t)?),
            TypeDefKind::Result(r) => format!(
                "::tuff::WireResult<{}, {}>",
                self.cpp_opt(r.ok.as_ref())?,
                self.cpp_opt(r.err.as_ref())?
            ),
            TypeDefKind::Tuple(t) => {
                if t.types.is_empty() || t.types.len() > 4 {
                    bail!("C++ interfaces support tuples of 1 to 4 elements");
                }
                let items: Result<Vec<_>> = t.types.iter().map(|t| self.cpp_ref(t)).collect();
                format!("::tuff::Tuple<{}>", items?.join(", "))
            }
            TypeDefKind::Type(t) => self.cpp_ref(t)?,
            other => bail!("unsupported anonymous type `{}`", other.as_str()),
        })
    }

    fn deps(&self, ty: &Type, out: &mut Vec<TypeId>) {
        let Type::Id(id) = ty else { return };
        let def = self.def(*id);
        if def.name.is_some() {
            if matches!(def.owner, TypeOwner::Interface(o) if o == self.id) {
                out.push(*id);
            }
            return;
        }
        self.kind_deps(&def.kind, out);
    }

    fn kind_deps(&self, kind: &TypeDefKind, out: &mut Vec<TypeId>) {
        match kind {
            TypeDefKind::Record(r) => r.fields.iter().for_each(|f| self.deps(&f.ty, out)),
            TypeDefKind::Variant(v) => v
                .cases
                .iter()
                .filter_map(|c| c.ty.as_ref())
                .for_each(|t| self.deps(t, out)),
            TypeDefKind::Tuple(t) => t.types.iter().for_each(|t| self.deps(t, out)),
            TypeDefKind::Option(t) | TypeDefKind::List(t) | TypeDefKind::Type(t) => {
                self.deps(t, out)
            }
            TypeDefKind::Result(r) => {
                r.ok.iter()
                    .chain(r.err.iter())
                    .for_each(|t| self.deps(t, out));
            }
            _ => {}
        }
    }

    fn emit_ordered(
        &self,
        out: &mut String,
        id: TypeId,
        name: &str,
        done: &mut HashSet<TypeId>,
    ) -> Result<()> {
        if !done.insert(id) {
            return Ok(());
        }
        let mut deps = Vec::new();
        self.kind_deps(&self.def(id).kind, &mut deps);
        for dep in deps {
            let dep_name = self.def(dep).name.clone().unwrap_or_default();
            self.emit_ordered(out, dep, &dep_name, done)?;
        }
        self.emit_cpp_type(out, name, id)
    }

    pub(super) fn emit_cpp(&self, out: &mut String) -> Result<()> {
        let iface = &self.resolve.interfaces[self.id];
        let module = ident(self.name);
        writeln!(out, "namespace {module} {{\n")?;
        out.push_str("using ::tuff::wire::encode;\nusing ::tuff::wire::decode;\n\n");
        writeln!(
            out,
            "inline constexpr const char *INTERFACE = {:?};\n",
            self.wire_name
        )?;
        let client = pascal(self.name);
        for (name, _) in &iface.types {
            if pascal(name) == client || pascal(name) == "Serve" {
                bail!(
                    "type `{name}` collides with generated item names in `{}`",
                    self.name
                );
            }
        }
        let mut done = HashSet::new();
        for (name, id) in &iface.types {
            self.emit_ordered(out, *id, name, &mut done)?;
        }
        let mut funcs = Vec::new();
        for (name, f) in &iface.functions {
            if !matches!(f.kind, FunctionKind::Freestanding) {
                bail!("`{name}`: resources and async functions are not supported");
            }
            if f.params.len() > super::MAX_PARAMS {
                bail!(
                    "`{name}`: at most {} parameters are supported",
                    super::MAX_PARAMS
                );
            }
            let info = self.func_info(f)?;
            let params = f
                .params
                .iter()
                .map(|p| Ok((ident(&p.name), self.cpp_ref(&p.ty)?)))
                .collect::<Result<Vec<_>>>()?;
            let (ret, split) = match &f.result {
                None => ("::tuff::Unit".to_string(), None),
                Some(ty) => {
                    let split = match self.resolve_result(ty) {
                        Some(r) if r.err.is_some() => {
                            Some((self.cpp_opt(r.ok.as_ref())?, self.cpp_opt(r.err.as_ref())?))
                        }
                        _ => None,
                    };
                    (self.cpp_ref(ty)?, split)
                }
            };
            funcs.push(CppFunc {
                info,
                params,
                ret,
                split,
            });
        }
        for f in &funcs {
            writeln!(
                out,
                "inline constexpr uint64_t {}_KEY = {:#018x}ull;",
                upper(&f.info.name),
                f.info.key
            )?;
        }
        out.push_str(&emit_client(&client, &funcs)?);
        out.push_str(&emit_serve(&funcs)?);
        writeln!(out, "\n}} // namespace {module}\n")?;
        Ok(())
    }

    fn min_size_expr(&self, types: &[String]) -> String {
        if types.is_empty() {
            "0".into()
        } else {
            types
                .iter()
                .map(|t| format!("::tuff::wire::MinSize<{t}>::value"))
                .collect::<Vec<_>>()
                .join(" + ")
        }
    }

    /// Leaves the module namespace to specialize `MinSize` (it lives in
    /// `tuff::wire`), then returns to it.
    fn min_size_spec(&self, out: &mut String, ty: &str, size: &str) -> Result<()> {
        let module = ident(self.name);
        writeln!(
            out,
            "}} // namespace {module}\n\nnamespace tuff::wire {{\ntemplate <> struct MinSize<::{module}::{ty}> {{\n    \
             static constexpr size_t value = {size};\n}};\n}} // namespace tuff::wire\n\nnamespace {module} {{\n"
        )?;
        Ok(())
    }

    fn emit_cpp_type(&self, out: &mut String, name: &str, id: TypeId) -> Result<()> {
        let def = self.def(id);
        let ty = pascal(name);
        match &def.kind {
            TypeDefKind::Record(r) => {
                let mut fields = Vec::new();
                for f in &r.fields {
                    fields.push((ident(&f.name), self.cpp_ref(&f.ty)?));
                }
                writeln!(out, "struct {ty} {{")?;
                for (n, t) in &fields {
                    writeln!(out, "    {t} {n}{{}};")?;
                }
                writeln!(out, "}};\n")?;
                let types: Vec<_> = fields.iter().map(|(_, t)| t.clone()).collect();
                self.min_size_spec(out, &ty, &self.min_size_expr(&types))?;
                writeln!(
                    out,
                    "inline void encode(::tuff::wire::Writer &w, const {ty} &v) {{"
                )?;
                for (n, _) in &fields {
                    writeln!(out, "    encode(w, v.{n});")?;
                }
                if fields.is_empty() {
                    out.push_str("    (void)w;\n    (void)v;\n");
                }
                writeln!(
                    out,
                    "}}\n\ninline bool decode(::tuff::wire::Reader &r, {ty} &out) {{"
                )?;
                if fields.is_empty() {
                    out.push_str("    (void)r;\n    (void)out;\n    return true;\n}\n\n");
                } else {
                    let checks: Vec<_> = fields
                        .iter()
                        .map(|(n, _)| format!("decode(r, out.{n})"))
                        .collect();
                    writeln!(out, "    return {};\n}}\n", checks.join(" && "))?;
                }
            }
            TypeDefKind::Variant(v) => {
                let mut cases = Vec::new();
                for c in &v.cases {
                    let payload = c.ty.as_ref().map(|t| self.cpp_ref(t)).transpose()?;
                    cases.push((pascal(&c.name), ident(&c.name), payload));
                }
                writeln!(out, "struct {ty} {{")?;
                let tags: Vec<_> = cases.iter().map(|(p, _, _)| p.clone()).collect();
                writeln!(
                    out,
                    "    enum class Tag : uint32_t {{ {} }};\n    Tag tag = Tag(0);",
                    tags.join(", ")
                )?;
                for (_, n, p) in &cases {
                    if let Some(p) = p {
                        writeln!(out, "    ::tuff::Option<{p}> {n};")?;
                    }
                }
                for (p, n, payload) in &cases {
                    match payload {
                        Some(t) => writeln!(
                            out,
                            "\n    static {ty} of_{n}({t} value) {{\n        {ty} out;\n        out.tag = Tag::{p};\n        \
                             out.{n} = ::tuff::Option<{t}>(::tuff::move(value));\n        return out;\n    }}"
                        )?,
                        None => writeln!(
                            out,
                            "\n    static {ty} of_{n}() {{\n        {ty} out;\n        out.tag = Tag::{p};\n        return out;\n    }}"
                        )?,
                    }
                }
                writeln!(out, "}};\n")?;
                self.min_size_spec(out, &ty, "1")?;
                writeln!(
                    out,
                    "inline void encode(::tuff::wire::Writer &w, const {ty} &v) {{\n    w.leb(static_cast<uint32_t>(v.tag));\n    switch (v.tag) {{"
                )?;
                for (p, n, payload) in &cases {
                    writeln!(out, "    case {ty}::Tag::{p}:")?;
                    if payload.is_some() {
                        writeln!(
                            out,
                            "        if (v.{n}) {{\n            encode(w, *v.{n});\n        }}"
                        )?;
                    }
                    writeln!(out, "        break;")?;
                }
                writeln!(
                    out,
                    "    }}\n}}\n\ninline bool decode(::tuff::wire::Reader &r, {ty} &out) {{\n    uint32_t tag;\n    \
                     if (!r.case_of(tag, {})) {{\n        return false;\n    }}\n    out = {ty}();\n    \
                     out.tag = static_cast<{ty}::Tag>(tag);\n    switch (out.tag) {{",
                    cases.len()
                )?;
                for (p, n, payload) in &cases {
                    writeln!(out, "    case {ty}::Tag::{p}:")?;
                    if let Some(t) = payload {
                        writeln!(
                            out,
                            "        {{\n            {t} value{{}};\n            if (!decode(r, value)) {{\n                return false;\n            }}\n            \
                             out.{n} = ::tuff::Option<{t}>(::tuff::move(value));\n        }}"
                        )?;
                    }
                    writeln!(out, "        break;")?;
                }
                out.push_str("    }\n    return true;\n}\n\n");
            }
            TypeDefKind::Enum(e) => {
                let cases: Vec<_> = e.cases.iter().map(|c| pascal(&c.name)).collect();
                writeln!(
                    out,
                    "enum class {ty} : uint32_t {{ {} }};\n",
                    cases.join(", ")
                )?;
                self.min_size_spec(out, &ty, "1")?;
                writeln!(
                    out,
                    "inline void encode(::tuff::wire::Writer &w, const {ty} &v) {{\n    w.leb(static_cast<uint32_t>(v));\n}}\n\n\
                     inline bool decode(::tuff::wire::Reader &r, {ty} &out) {{\n    uint32_t tag;\n    \
                     if (!r.case_of(tag, {})) {{\n        return false;\n    }}\n    out = static_cast<{ty}>(tag);\n    return true;\n}}\n",
                    cases.len()
                )?;
            }
            TypeDefKind::Flags(f) => {
                if f.flags.len() > 64 {
                    bail!("flags `{name}` has more than 64 members");
                }
                let mask: u64 = if f.flags.len() == 64 {
                    u64::MAX
                } else {
                    (1u64 << f.flags.len()) - 1
                };
                writeln!(out, "struct {ty} {{\n    uint64_t bits = 0;")?;
                for (i, flag) in f.flags.iter().enumerate() {
                    writeln!(
                        out,
                        "    static constexpr uint64_t {} = 1ull << {i};",
                        upper(&flag.name)
                    )?;
                }
                writeln!(
                    out,
                    "    bool contains(uint64_t mask) const {{ return (bits & mask) == mask; }}\n}};\n"
                )?;
                self.min_size_spec(out, &ty, "8")?;
                writeln!(
                    out,
                    "inline void encode(::tuff::wire::Writer &w, const {ty} &v) {{\n    encode(w, v.bits);\n}}\n\n\
                     inline bool decode(::tuff::wire::Reader &r, {ty} &out) {{\n    uint64_t bits;\n    \
                     if (!decode(r, bits) || (bits & ~{mask:#x}ull) != 0) {{\n        return false;\n    }}\n    out.bits = bits;\n    return true;\n}}\n"
                )?;
            }
            TypeDefKind::Type(_)
            | TypeDefKind::Tuple(_)
            | TypeDefKind::Option(_)
            | TypeDefKind::Result(_)
            | TypeDefKind::List(_) => {
                writeln!(out, "using {ty} = {};\n", self.cpp_anon(&def.kind)?)?;
            }
            other => bail!(
                "type `{name}` is a {}; plugin interfaces support records, variants, enums, flags, options, results, tuples and lists only",
                other.as_str()
            ),
        }
        Ok(())
    }
}

struct CppFunc {
    info: FuncInfo,
    params: Vec<(String, String)>,
    ret: String,
    split: Option<(String, String)>,
}

impl CppFunc {
    fn outcome(&self) -> String {
        match &self.split {
            Some((ok, err)) => format!("::tuff::Outcome<{ok}, ::tuff::Failure<{err}>>"),
            None => format!("::tuff::Outcome<{}, ::tuff::CallError>", self.ret),
        }
    }

    fn call_error(&self, expr: &str) -> String {
        match &self.split {
            Some((_, err)) => format!("::tuff::Failure<{err}>({expr})"),
            None => expr.to_string(),
        }
    }

    fn params_decl(&self) -> String {
        self.params
            .iter()
            .map(|(n, t)| format!(", {t} {n}"))
            .collect()
    }

    fn encode_args(&self) -> String {
        let mut s = String::from("        ::tuff::wire::Writer w;\n");
        for (n, _) in &self.params {
            let _ = writeln!(s, "        encode(w, {n});");
        }
        s
    }

    fn move_args(&self) -> String {
        self.params
            .iter()
            .map(|(n, _)| format!("::tuff::move({n})"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn emit_client(client: &str, funcs: &[CppFunc]) -> Result<String> {
    let mut s = format!(
        "\nclass {client} {{\n  public:\n    static {client} any() {{ return {client}(::tuff::Target::any()); }}\n    \
         static {client} at(::tuff::Str plugin) {{ return {client}(::tuff::Target::plugin(plugin)); }}\n    \
         static {client} at(const char *plugin) {{ return {client}(::tuff::Target::plugin(plugin)); }}\n"
    );
    for f in funcs {
        let name = ident(&f.info.name);
        let outcome = f.outcome();
        let key = format!("{}_KEY", upper(&f.info.name));
        let pass: String = f
            .params
            .iter()
            .map(|(n, _)| format!(", ::tuff::move({n})"))
            .collect();
        write!(
            s,
            "\n    // Sends when awaited.\n    ::tuff::Async<{outcome}> {name}({params}) const {{\n        \
             return {name}_call(target_{pass});\n    }}\n",
            params = f.params_decl().trim_start_matches(", ")
        )?;
        if !f.info.has_result {
            write!(
                s,
                "\n    ::tuff::Outcome<uint32_t, ::tuff::CallError> notify_{name}({params}) const {{\n{enc}        \
                 if (!w.ok()) {{\n            return ::tuff::Outcome<uint32_t, ::tuff::CallError>(::tuff::CallError::refused(\"out of memory\"));\n        }}\n        \
                 return ::tuff::notify(target_, ::tuff::str(INTERFACE), {key}, w.finish());\n    }}\n",
                params = f.params_decl().trim_start_matches(", "),
                enc = f.encode_args()
            )?;
        }
    }
    s.push_str("\n  private:\n    explicit ");
    writeln!(
        s,
        "{client}(::tuff::Target target) : target_(::tuff::move(target)) {{}}"
    )?;
    for f in funcs {
        let name = ident(&f.info.name);
        let outcome = f.outcome();
        let key = format!("{}_KEY", upper(&f.info.name));
        write!(
            s,
            "\n    static ::tuff::Async<{outcome}> {name}_call(::tuff::Target target{params}) {{\n{enc}        \
             if (!w.ok()) {{\n            co_return {outcome}({oom});\n        }}\n        \
             auto reply = co_await ::tuff::request(::tuff::move(target), ::tuff::str(INTERFACE), {key}, w.finish());\n        \
             if (!reply) {{\n            co_return {outcome}({fail});\n        }}\n",
            params = f.params_decl(),
            enc = f.encode_args(),
            oom = f.call_error("::tuff::CallError::refused(\"out of memory\")"),
            fail = f.call_error("::tuff::move(reply.error())"),
        )?;
        let bad = f.call_error("::tuff::CallError::refused(\"bad reply\")");
        write!(
            s,
            "        {ret} value{{}};\n        if (!::tuff::wire::decode_all(reply->data(), reply->len(), value)) {{\n            \
             co_return {outcome}({bad});\n        }}\n",
            ret = f.ret
        )?;
        match &f.split {
            Some((ok, err)) => write!(
                s,
                "        if (value.is_err) {{\n            co_return {outcome}(::tuff::Failure<{err}>::from_app(value.err ? ::tuff::move(*value.err) : {err}()));\n        }}\n        \
                 co_return {outcome}(value.ok ? ::tuff::move(*value.ok) : {ok}());\n    }}\n"
            )?,
            None => write!(
                s,
                "        co_return {outcome}(::tuff::move(value));\n    }}\n"
            )?,
        }
    }
    s.push_str("\n    ::tuff::Target target_;\n};\n");
    Ok(s)
}

fn emit_serve(funcs: &[CppFunc]) -> Result<String> {
    let mut s = String::from(
        "\n// Serves the interface with the static methods of S: `R method(::tuff::Str from, args...)`\n\
         // answers at once; `void method(::tuff::Str from, args..., ::tuff::messaging::TypedReply<R> reply)`\n\
         // answers later (`from` is only valid during the call). Methods without a result return void.\n\
         template <class S> ::tuff::Status serve() {\n",
    );
    let keys: Vec<_> = funcs
        .iter()
        .map(|f| format!("{}_KEY", upper(&f.info.name)))
        .collect();
    write!(
        s,
        "    static constexpr uint64_t keys[] = {{{}}};\n    ::tuff::Status provided = ::tuff::messaging::provide(::tuff::str(INTERFACE), keys, {});\n    \
         if (!provided) {{\n        return provided;\n    }}\n",
        if keys.is_empty() {
            "0".to_string()
        } else {
            keys.join(", ")
        },
        keys.len()
    )?;
    for f in funcs {
        let name = ident(&f.info.name);
        let decls: String = f
            .params
            .iter()
            .map(|(n, t)| format!("            {t} {n}{{}};\n"))
            .collect();
        let decodes: String = f
            .params
            .iter()
            .map(|(n, _)| format!(" || !decode(r, {n})"))
            .collect();
        let moves = f.move_args();
        let sep = if moves.is_empty() { "" } else { ", " };
        let ok_arg = if f.info.has_result {
            "result".to_string()
        } else {
            "::tuff::Unit{}".to_string()
        };
        let call = if f.info.has_result {
            format!("auto result = S::{name}(from{sep}{moves});")
        } else {
            format!("S::{name}(from{sep}{moves});")
        };
        write!(
            s,
            "    if (!::tuff::messaging::route(::tuff::str(INTERFACE), {key}_KEY,\n        [](const ::tuff::messaging::Incoming &msg, ::tuff::messaging::Reply reply) {{\n            \
             ::tuff::wire::Reader r(msg.payload.ptr, msg.payload.len);\n{decls}            \
             if (false{decodes} || !r.finished()) {{\n                reply.err(\"bad payload\");\n                return;\n            }}\n            \
             ::tuff::Str from = msg.sender;\n            ::tuff::messaging::TypedReply<{ret}> typed(::tuff::move(reply));\n            \
             if constexpr (requires {{ S::{name}(from{sep}{moves}, ::tuff::move(typed)); }}) {{\n                \
             S::{name}(from{sep}{moves}, ::tuff::move(typed));\n            }} else {{\n                {call}\n                typed.ok({ok_arg});\n            }}\n        }})) {{\n        \
             return ::tuff::Status::fail(\"out of memory\");\n    }}\n",
            key = upper(&f.info.name),
            ret = f.ret
        )?;
    }
    s.push_str("    return ::tuff::Status();\n}\n");
    Ok(s)
}
