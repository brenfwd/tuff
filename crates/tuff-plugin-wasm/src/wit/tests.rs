use std::sync::Arc;

use super::bindings::types::{
    ArgValue, BlockPos, CommandNode, CommandTree, DoubleRange, IntRange, NodeKind, Parser,
    TextKind, Vec3,
};
use super::canon::{self, Bounds, Budget, CanonError, FlatReader, FlatVal, GuestMemory, Value};
use super::runtime::{Host, Runtime};
use super::schema::{Flat, Layout, Schema, Shape};
use super::tree::{self, Context};
use super::value::{self, FromValue, ToValue};
use crate::sandbox::{Limits, engine};

/// Guest memory for tests: a byte vector with a bump allocator.
struct Arena {
    mem: Vec<u8>,
    top: u32,
}

impl Arena {
    fn new(size: usize, top: u32) -> Self {
        Self {
            mem: vec![0; size],
            top,
        }
    }
}

impl GuestMemory for Arena {
    fn realloc(&mut self, align: u32, size: u32) -> Result<u32, CanonError> {
        let ptr = self.top.next_multiple_of(align.max(1));
        let end = ptr + size;
        if end as usize > self.mem.len() {
            return Err(CanonError::Alloc("arena full".into()));
        }
        self.top = end;
        Ok(ptr)
    }

    fn memory(&mut self) -> &mut [u8] {
        &mut self.mem
    }
}

const TEST_WIT: &str = r#"
package test:canon;

interface api {
    record point { x: s32, y: f32 }
    variant mixed { a(u8), b(f64), c(string), d(s64), e(f32), f }
    flags perms { read, write, exec }
    enum color { red, green }

    roundtrip: func(
        p: point, m: mixed, f: perms, c: color, o: option<string>,
        r: result<u32, string>, l: list<list<u16>>, ch: char, b: bool,
        t: tuple<u64, s8>, e: list<tuple<>>,
    ) -> mixed;
}

interface host-api {
    many: func(
        a: u32, b: u32, c: u32, d: u32, e: u32, f: u32, g: u32, h: u32, i: u32,
        j: u32, k: u32, l: u32, m: u32, n: u32, o: u32, p: u32, q: u32,
    ) -> string;
}

interface guest-api {
    many: func(
        a: u32, b: u32, c: u32, d: u32, e: u32, f: u32, g: u32, h: u32, i: u32,
        j: u32, k: u32, l: u32, m: u32, n: u32, o: u32, p: u32, q: u32,
    ) -> u32;
    call-host: func() -> u32;
}

world canon {
    export api;
}

world runtime {
    import host-api;
    export guest-api;
}
"#;

fn test_schema(world: &str) -> Schema {
    Schema::parse(TEST_WIT, world).unwrap()
}

fn params(schema: &Schema, name: &str) -> Vec<Arc<Layout>> {
    schema.export(name).unwrap().1.params.clone()
}

fn s(text: &str) -> Value<'_> {
    Value::String(text.into())
}

fn some(v: Value<'_>) -> Value<'_> {
    Value::Variant(1, Some(Box::new(v)))
}

fn roundtrip_values() -> Vec<Value<'static>> {
    vec![
        Value::Record(vec![Value::S32(-7), Value::F32(1.25)]),
        Value::Variant(2, Some(Box::new(s("mixed")))),
        Value::Flags(0b101),
        Value::Variant(1, None),
        some(s("present")),
        Value::Variant(1, Some(Box::new(s("bad")))),
        Value::List(vec![
            Value::List(vec![Value::U16(1), Value::U16(65535)]),
            Value::List(vec![]),
        ]),
        Value::Char('\u{1F600}'),
        Value::Bool(true),
        Value::Record(vec![Value::U64(u64::MAX), Value::S8(-128)]),
        Value::List(vec![Value::Record(vec![]); 3]),
    ]
}

#[tuff_core::watchdog]
#[test]
fn plugin_wit_lays_out_like_the_c_bindings() {
    let schema = super::schema();
    let (_, register) = schema.import("register-command").unwrap();
    assert_eq!(register.module, "tuff:plugin/host@0.1.0");
    assert_eq!(register.core_params, [Flat::I32; 3]);
    assert!(register.retptr && !register.indirect_params);

    let Shape::Record(fields) = &register.params[0].shape else {
        panic!("tree is a record");
    };
    let Shape::List(node) = &fields[0].layout.shape else {
        panic!("nodes is a list");
    };
    assert_eq!((node.size, node.align), (96, 8));

    let (_, run) = schema.export("run-command").unwrap();
    assert_eq!(run.field, "tuff:plugin/guest@0.1.0#run-command");
    assert_eq!(
        run.post_return.as_deref(),
        Some("cabi_post_tuff:plugin/guest@0.1.0#run-command")
    );
    assert_eq!((run.params[0].size, run.params[0].align), (56, 8));
    assert_eq!(run.core_params.len(), 10);
    assert_eq!(run.core_results, [Flat::I32]);

    let (_, event) = schema.export("on-event").unwrap();
    assert_eq!(event.core_params, [Flat::I32]);
    assert!(event.indirect_params);
    assert_eq!((event.params[0].size, event.params[0].align), (112, 8));
    assert!(event.post_return.is_none());
}

#[tuff_core::watchdog]
#[test]
fn unsupported_wit_is_refused() {
    let flags: Vec<String> = (0..65).map(|i| format!("f{i}")).collect();
    let big = format!("flags big {{ {} }} f: func(x: big);", flags.join(", "));
    for body in [
        "resource r; f: func(x: borrow<r>);",
        "f: async func();",
        "f: func(x: future<u8>);",
        &big,
    ] {
        let wit = format!("package t:t; interface i {{ {body} }} world w {{ export i; }}");
        assert!(Schema::parse(&wit, "w").is_err(), "{body}");
    }
}

#[tuff_core::watchdog]
#[test]
fn values_round_trip_flat_and_through_memory() {
    let schema = test_schema("canon");
    let layouts = params(&schema, "roundtrip");
    let values = roundtrip_values();
    let bounds = Bounds::default();

    let mut arena = Arena::new(1 << 16, 64);
    let mut flat = Vec::new();
    for (layout, value) in layouts.iter().zip(&values) {
        canon::lower_flat(layout, value, &mut arena, &mut flat).unwrap();
    }
    let mut reader = FlatReader::new(&flat);
    let mut budget = Budget::new(bounds);
    let lifted: Vec<Value> = layouts
        .iter()
        .map(|l| canon::lift_flat(l, &mut reader, &arena.mem, &mut budget).unwrap())
        .collect();
    reader.finish().unwrap();
    assert_eq!(lifted, values);

    let (_, sig) = schema.export("roundtrip").unwrap();
    let record = Value::Record(values);
    let ptr = arena
        .realloc(sig.params_record.align, sig.params_record.size)
        .unwrap();
    canon::store(&sig.params_record, &record, &mut arena, ptr).unwrap();
    let loaded = canon::load(
        &sig.params_record,
        &arena.mem,
        ptr,
        &mut Budget::new(bounds),
    );
    assert_eq!(loaded.unwrap(), record);

    let wrong = Value::Variant(5, Some(Box::new(Value::U8(1))));
    assert_eq!(
        canon::lower_flat(&layouts[1], &wrong, &mut arena, &mut Vec::new()),
        Err(CanonError::TypeMismatch)
    );
    assert_eq!(
        canon::store(&layouts[2], &Value::Flags(0b1000), &mut arena, 0),
        Err(CanonError::TypeMismatch)
    );
}

#[tuff_core::watchdog]
#[test]
fn joined_variant_payloads_bitcast_like_the_spec() {
    let schema = test_schema("canon");
    let mixed = &params(&schema, "roundtrip")[1];
    assert_eq!(mixed.flat, [Flat::I32, Flat::I64, Flat::I32]);
    let mut arena = Arena::new(4096, 64);
    let mut lower = |v: Value| {
        let mut out = Vec::new();
        canon::lower_flat(mixed, &v, &mut arena, &mut out).unwrap();
        out
    };
    assert_eq!(
        lower(Value::Variant(4, Some(Box::new(Value::F32(1.5))))),
        [
            FlatVal::I32(4),
            FlatVal::I64(i64::from(1.5f32.to_bits())),
            FlatVal::I32(0)
        ]
    );
    assert_eq!(
        lower(Value::Variant(1, Some(Box::new(Value::F64(-2.5))))),
        [
            FlatVal::I32(1),
            FlatVal::I64((-2.5f64).to_bits() as i64),
            FlatVal::I32(0)
        ]
    );
    assert_eq!(
        lower(Value::Variant(0, Some(Box::new(Value::U8(200))))),
        [FlatVal::I32(0), FlatVal::I64(200), FlatVal::I32(0)]
    );
    let lift = |flat: &[FlatVal]| {
        let mut reader = FlatReader::new(flat);
        canon::lift_flat(mixed, &mut reader, &[], &mut Budget::new(Bounds::default()))
    };
    assert_eq!(
        lift(&[FlatVal::I32(4), FlatVal::I64(0x3fc0_0000), FlatVal::I32(0)]).unwrap(),
        Value::Variant(4, Some(Box::new(Value::F32(1.5))))
    );
    assert_eq!(
        lift(&[FlatVal::I32(6), FlatVal::I64(0), FlatVal::I32(0)]),
        Err(CanonError::InvalidDiscriminant(6))
    );
    assert_eq!(
        lift(&[FlatVal::I32(5), FlatVal::I64(0)]),
        Err(CanonError::TypeMismatch)
    );
}

fn pair_at(mem: &mut [u8], at: usize, ptr: u32, len: u32) {
    mem[at..at + 4].copy_from_slice(&ptr.to_le_bytes());
    mem[at + 4..at + 8].copy_from_slice(&len.to_le_bytes());
}

#[tuff_core::watchdog]
#[test]
fn hostile_memory_is_refused_before_allocating() {
    let schema = test_schema("canon");
    let layouts = params(&schema, "roundtrip");
    let (perms, color, option_string) = (&layouts[2], &layouts[3], &layouts[4]);
    let (lists, character, empties) = (&layouts[6], &layouts[7], &layouts[10]);
    let bounds = Bounds {
        string: 64,
        list: 1000,
        bytes: 4096,
        values: 2000,
    };
    let load = |layout: &Layout, mem: &[u8], ptr: u32| {
        canon::load(layout, mem, ptr, &mut Budget::new(bounds)).map(Value::into_owned)
    };
    let out_of_bounds =
        |r: Result<Value, CanonError>| matches!(r, Err(CanonError::OutOfBounds { .. }));
    let misaligned = |r: Result<Value, CanonError>| matches!(r, Err(CanonError::Misaligned { .. }));

    let mut mem = vec![0u8; 1024];
    mem[0] = 1;
    pair_at(&mut mem, 4, 100, 65);
    assert_eq!(
        load(option_string, &mem, 0),
        Err(CanonError::StringTooLong(65))
    );
    pair_at(&mut mem, 4, 1020, 10);
    assert!(out_of_bounds(load(option_string, &mem, 0)));
    pair_at(&mut mem, 4, u32::MAX, 2);
    assert!(out_of_bounds(load(option_string, &mem, 0)));
    mem[100..102].copy_from_slice(&[0xff, 0xfe]);
    pair_at(&mut mem, 4, 100, 2);
    assert_eq!(load(option_string, &mem, 0), Err(CanonError::InvalidUtf8));
    mem[0] = 2;
    assert_eq!(
        load(option_string, &mem, 0),
        Err(CanonError::InvalidDiscriminant(2))
    );
    assert!(misaligned(load(option_string, &mem, 2)));
    assert!(out_of_bounds(load(option_string, &mem, 1020)));
    assert!(out_of_bounds(load(option_string, &mem[..6], 0)));

    pair_at(&mut mem, 0, 200, 1001);
    assert_eq!(load(lists, &mem, 0), Err(CanonError::ListTooLong(1001)));
    pair_at(&mut mem, 0, 201, 1);
    assert!(misaligned(load(lists, &mem, 0)));
    pair_at(&mut mem, 0, 200, 600);
    assert_eq!(load(lists, &mem, 0), Err(CanonError::BudgetExhausted));
    let mut big = vec![0u8; 4096];
    for i in 0..100 {
        pair_at(&mut big, i * 8, 0, 1000);
    }
    pair_at(&mut big, 3000, 0, 100);
    let generous = Bounds {
        bytes: 1 << 20,
        ..bounds
    };
    let nested = canon::load(lists, &big, 3000, &mut Budget::new(generous));
    assert_eq!(
        nested,
        Err(CanonError::BudgetExhausted),
        "elements are capped per call"
    );

    pair_at(&mut mem, 0, 0, u32::MAX);
    assert_eq!(
        load(empties, &mem, 0),
        Err(CanonError::ListTooLong(u32::MAX))
    );
    pair_at(&mut mem, 0, 0, 1000);
    assert!(matches!(load(empties, &mem, 0), Ok(Value::List(l)) if l.len() == 1000));

    mem[0..4].copy_from_slice(&0xD800u32.to_le_bytes());
    assert_eq!(
        load(character, &mem, 0),
        Err(CanonError::InvalidChar(0xD800))
    );
    mem[0] = 9;
    assert_eq!(
        load(color, &mem, 0),
        Err(CanonError::InvalidDiscriminant(9))
    );
    mem[0] = 0xff;
    assert_eq!(load(perms, &mem, 0), Ok(Value::Flags(0b111)));
}

#[tuff_core::watchdog]
#[test]
fn hostile_flat_values_are_refused() {
    let schema = test_schema("canon");
    let layouts = params(&schema, "roundtrip");
    let mem = vec![0u8; 256];
    let lift = |layout: &Layout, flat: &[FlatVal]| {
        let mut reader = FlatReader::new(flat);
        canon::lift_flat(
            layout,
            &mut reader,
            &mem,
            &mut Budget::new(Bounds::default()),
        )
        .and_then(|v| reader.finish().map(|()| v))
    };
    let string = &layouts[4];
    assert!(matches!(
        lift(
            string,
            &[FlatVal::I32(1), FlatVal::I32(250), FlatVal::I32(100)]
        ),
        Err(CanonError::OutOfBounds { .. })
    ));
    assert_eq!(
        lift(
            string,
            &[FlatVal::I32(1), FlatVal::I32(0), FlatVal::I32(-1)]
        ),
        Err(CanonError::StringTooLong(u32::MAX))
    );
    assert_eq!(
        lift(string, &[FlatVal::I32(1)]),
        Err(CanonError::TypeMismatch)
    );
    assert_eq!(
        lift(
            string,
            &[
                FlatVal::I32(0),
                FlatVal::I32(0),
                FlatVal::I32(0),
                FlatVal::I32(0)
            ]
        ),
        Err(CanonError::TypeMismatch)
    );
    assert_eq!(
        lift(&layouts[7], &[FlatVal::I32(0x11_0000)]),
        Err(CanonError::InvalidChar(0x11_0000))
    );
    assert_eq!(
        lift(&layouts[8], &[FlatVal::I64(1)]),
        Err(CanonError::TypeMismatch)
    );
}

/// A xorshift generator, so the fuzz test is deterministic.
fn xorshift(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

#[tuff_core::watchdog]
#[test]
fn random_memory_never_panics() {
    let schema = super::schema();
    let layouts: Vec<Arc<Layout>> = schema
        .imports
        .iter()
        .chain(&schema.exports)
        .flat_map(|s| s.params.iter().chain(&s.result).cloned())
        .collect();
    let mut state = 0x9E37_79B9_7F4A_7C15;
    let bounds = Bounds::default();
    let mut decoded = 0;
    for round in 0..4000 {
        let mut mem: Vec<u8> = (0..4096).map(|_| xorshift(&mut state) as u8).collect();
        if round % 2 == 0 {
            for chunk in mem.chunks_mut(4) {
                let small = (xorshift(&mut state) % 8) as u32 * 8;
                chunk.copy_from_slice(&small.to_le_bytes());
            }
        }
        let layout = &layouts[round % layouts.len()];
        let ptr = (xorshift(&mut state) % 4096) as u32 & !(layout.align - 1);
        if canon::load(layout, &mem, ptr, &mut Budget::new(bounds)).is_ok() {
            decoded += 1;
        }
        let flat: Vec<FlatVal> = layout
            .flat
            .iter()
            .map(|ty| {
                let bits = xorshift(&mut state) % 8192;
                match ty {
                    Flat::I32 => FlatVal::I32(bits as i32),
                    Flat::I64 => FlatVal::I64(bits as i64),
                    Flat::F32 => FlatVal::F32(bits as f32),
                    Flat::F64 => FlatVal::F64(bits as f64),
                }
            })
            .collect();
        let mut reader = FlatReader::new(&flat);
        let _ = canon::lift_flat(layout, &mut reader, &mem, &mut Budget::new(bounds));
    }
    assert!(decoded > 0, "some random values should decode");
}

struct Recorder {
    calls: Vec<Vec<Value<'static>>>,
}

impl Host for Recorder {
    fn call(&mut self, _: usize, args: Vec<Value<'_>>) -> value::Result<Option<Box<dyn ToValue>>> {
        let sum: u32 = args
            .iter()
            .map(|a| match a {
                Value::U32(v) => *v,
                _ => 0,
            })
            .sum();
        self.calls
            .push(args.into_iter().map(Value::into_owned).collect());
        Ok(Some(Box::new(format!("sum {sum}"))))
    }
}

#[tuff_core::watchdog]
#[test]
fn many_parameters_pass_through_memory_both_ways() {
    let schema = Arc::new(test_schema("runtime"));
    let runtime_schema = schema.clone();
    let (_, host_many) = schema.import("many").unwrap();
    assert!(host_many.indirect_params && host_many.retptr);
    assert!(schema.export("many").unwrap().1.indirect_params);

    let tuple: String = (1..=17u32).map(|v| escape(&v.to_le_bytes())).collect();
    let loads: String = (0..17)
        .map(|i| format!("(i32.load offset={} (local.get 0))", i * 4))
        .collect::<Vec<_>>()
        .join(" ");
    let adds = "i32.add ".repeat(16);
    let wat = format!(
        r#"(module
  (import "test:canon/host-api" "many" (func $many (param i32 i32)))
  (memory (export "memory") 1)
  (global $heap (mut i32) (i32.const 4096))
  (data (i32.const 256) "{tuple}")
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32)
    (global.set $heap (i32.add (global.get $heap) (i32.const 128)))
    (global.get $heap))
  (func (export "test:canon/guest-api#many") (param i32) (result i32)
    {loads} {adds})
  (func (export "test:canon/guest-api#call-host") (result i32)
    (call $many (i32.const 256) (i32.const 512))
    (i32.load (i32.const 516))))"#
    );
    let wasm = wat::parse_str(&wat).unwrap();
    let mut runtime = Runtime::new(
        &engine(),
        schema,
        &wasm,
        Recorder { calls: Vec::new() },
        Limits::default(),
        Bounds::default(),
    )
    .unwrap();
    let args: Vec<Value> = (1..=17).map(|v| Value::U32(v * 10)).collect();
    let (many, _) = runtime_schema.export("many").unwrap();
    let (call_host, _) = runtime_schema.export("call-host").unwrap();
    let lift = |value: Option<Value<'_>>| u32::from_value(value.ok_or("no result")?);
    assert_eq!(runtime.call(many, &args, lift).unwrap(), 1530);
    assert_eq!(
        runtime.call(call_host, &[], lift).unwrap(),
        "sum 153".len() as u32
    );
    assert_eq!(
        runtime.host().calls,
        [(1..=17).map(Value::U32).collect::<Vec<_>>()]
    );
}

fn escape(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("\\{b:02x}")).collect()
}

fn node(name: &str, kind: NodeKind, children: &[u16], executes: Option<u32>) -> CommandNode {
    CommandNode {
        name: name.into(),
        kind,
        children: children.to_vec(),
        executes,
        redirect: None,
        permission: None,
        suggest: false,
    }
}

fn integer(min: Option<i32>, max: Option<i32>) -> NodeKind {
    NodeKind::Argument(Parser::Integer(IntRange { min, max }))
}

fn spike_tree() -> CommandTree {
    CommandTree {
        nodes: vec![
            node("spike", NodeKind::Literal, &[1, 2], Some(0)),
            node("count", integer(Some(1), Some(9)), &[], Some(1)),
            node(
                "say",
                NodeKind::Argument(Parser::Text(TextKind::Greedy)),
                &[],
                Some(2),
            ),
        ],
    }
}

#[tuff_core::watchdog]
#[test]
fn trees_are_validated() {
    assert_eq!(tree::validate(&spike_tree()), Ok(()));
    let edits: [fn(&mut CommandTree); 11] = [
        |t| t.nodes[0].kind = NodeKind::Argument(Parser::Boolean),
        |t| t.nodes[0].children = vec![1, 1],
        |t| t.nodes[0].children = vec![1, 9],
        |t| {
            t.nodes[0].children = vec![];
            t.nodes[1].children = vec![2];
            t.nodes[2].children = vec![1];
        },
        |t| t.nodes[1].kind = integer(Some(5), Some(1)),
        |t| t.nodes[2].name = "two words".into(),
        |t| t.nodes[2].executes = None,
        |t| {
            t.nodes[1].kind = NodeKind::Argument(Parser::Double(DoubleRange {
                min: Some(f64::NAN),
                max: None,
            }))
        },
        |t| t.nodes[2].redirect = Some(40),
        |t| t.nodes.clear(),
        |t| {
            t.nodes[0].children = vec![2];
            t.nodes[2].children = vec![1];
        },
    ];
    for (i, edit) in edits.iter().enumerate() {
        let mut tree = spike_tree();
        edit(&mut tree);
        assert!(tree::validate(&tree).is_err(), "case {i}");
    }
}

fn open(node: &str) -> bool {
    node != "locked"
}

fn walk(tree: &CommandTree, line: &str) -> Result<(u32, Vec<ArgValue>), String> {
    let cx = Context {
        permitted: &open,
        origin: Some([10.5, 64.0, -3.25]),
        sender: None,
        online: &|| Vec::new(),
    };
    tree::walk(tree, line, &cx).map(|matched| {
        (
            matched.handler,
            matched.args.into_iter().map(|a| a.value).collect(),
        )
    })
}

#[tuff_core::watchdog]
#[test]
fn lines_walk_trees_to_handlers_and_typed_arguments() {
    let tree = spike_tree();
    assert_eq!(walk(&tree, "spike"), Ok((0, vec![])));
    assert_eq!(walk(&tree, "spike 4"), Ok((1, vec![ArgValue::Integer(4)])));
    assert_eq!(
        walk(&tree, "spike hi there  you"),
        Ok((2, vec![ArgValue::Text("hi there  you".into())]))
    );
    assert_eq!(
        walk(&tree, "spike 12"),
        Ok((2, vec![ArgValue::Text("12".into())])),
        "an out of range integer falls through to the greedy sibling"
    );
    assert!(walk(&tree, "other").is_err());
    assert!(walk(&tree, "spike ").is_err());

    let mut tree = CommandTree {
        nodes: vec![
            node("t", NodeKind::Literal, &[1, 2, 4, 6, 7], None),
            node("set", NodeKind::Literal, &[3], None),
            node("pos", NodeKind::Argument(Parser::BlockPos), &[], Some(1)),
            node(
                "state",
                NodeKind::Argument(Parser::BlockState),
                &[],
                Some(2),
            ),
            node("look", NodeKind::Literal, &[5], None),
            node("at", NodeKind::Argument(Parser::Vec3), &[], Some(3)),
            node(
                "say",
                NodeKind::Argument(Parser::Text(TextKind::Phrase)),
                &[],
                Some(4),
            ),
            node("hold", NodeKind::Argument(Parser::Item), &[], Some(5)),
        ],
    };
    tree.nodes[0].children = vec![1, 4, 6];
    tree.nodes[1].children = vec![2];
    tree.nodes[2].children = vec![3];
    tree.nodes[2].executes = None;
    tree.nodes[6].children = vec![7];
    tree.nodes[6].executes = None;
    assert_eq!(tree::validate(&tree), Ok(()));
    assert_eq!(
        walk(&tree, "t set ~1 ~ -5 oak_stairs[facing=east]"),
        Ok((
            2,
            vec![
                ArgValue::BlockPos(BlockPos { x: 11, y: 64, z: -5 }),
                ArgValue::BlockState(
                    "minecraft:oak_stairs[facing=east,half=bottom,shape=straight,waterlogged=false]"
                        .into()
                ),
            ]
        ))
    );
    assert!(walk(&tree, "t set 1 2 3 not_a_block").is_err());
    assert!(walk(&tree, "t set 1.5 2 3 stone").is_err());
    assert_eq!(
        walk(&tree, "t look ~ ~1.5 0.5"),
        Ok((
            3,
            vec![ArgValue::Vec3(Vec3 {
                x: 10.5,
                y: 65.5,
                z: 0.5
            })]
        ))
    );
    assert_eq!(
        walk(&tree, r#"t "a \"quoted\" word" stone"#),
        Ok((
            5,
            vec![
                ArgValue::Text(r#"a "quoted" word"#.into()),
                ArgValue::Item("minecraft:stone".into())
            ]
        ))
    );
    assert!(walk(&tree, r#"t "unclosed stone"#).is_err());
    assert!(walk(&tree, "t word nothing_like_an_item").is_err());

    tree.nodes[4].permission = Some("locked".into());
    assert!(walk(&tree, "t look 1 2 3").is_err());
    tree.nodes[0].permission = Some("locked".into());
    assert!(
        walk(&tree, "t set 1 2 3 stone")
            .unwrap_err()
            .contains("permission")
    );
}

#[tuff_core::watchdog]
#[test]
fn walks_report_where_the_line_went_wrong() {
    let mut tree = spike_tree();
    tree.nodes.pop();
    tree.nodes[0].children.pop();
    let err = walk(&tree, "spike 4 extra").unwrap_err();
    assert!(err.ends_with("spike 4 <--[HERE]"), "{err}");
    let tree = CommandTree {
        nodes: vec![
            node("p", NodeKind::Literal, &[1], None),
            node("who", NodeKind::Argument(Parser::Player), &[], Some(0)),
        ],
    };
    let err = walk(&tree, "p Steve").unwrap_err();
    assert!(err.contains("No player was found"), "{err}");
}

fn named(name: &str, id: u64) -> super::bindings::types::Player {
    super::bindings::types::Player {
        uuid: crate::sandbox::uuid(u128::from(id)),
        name: name.into(),
    }
}

#[tuff_core::watchdog]
#[test]
fn player_arguments_resolve_against_the_roster() {
    let tree = CommandTree {
        nodes: vec![
            node("kit", NodeKind::Literal, &[1, 2], None),
            node("one", NodeKind::Argument(Parser::Player), &[], Some(0)),
            node("all", NodeKind::Argument(Parser::Players), &[], Some(1)),
        ],
    };
    let roster = || vec![(named("Alex", 1), None), (named("Steve", 2), None)];
    let cx = Context {
        permitted: &open,
        origin: None,
        sender: Some(named("Alex", 1)),
        online: &roster,
    };
    let run = |line: &str| tree::walk(&tree, line, &cx).map(|m| m.args[0].value.clone());
    assert_eq!(run("kit steve"), Ok(ArgValue::Player(named("Steve", 2))));
    assert_eq!(run("kit @s"), Ok(ArgValue::Player(named("Alex", 1))));
    assert_eq!(
        run("kit @a"),
        Ok(ArgValue::Players(vec![named("Alex", 1), named("Steve", 2)]))
    );
    assert!(run("kit nobody").is_err());
    assert_eq!(tree::client_parser(&Parser::Players).0, "minecraft:entity");
}

#[tuff_core::watchdog]
#[test]
fn suggestion_targets_follow_the_parsed_prefix() {
    let mut flagged = node(
        "name",
        NodeKind::Argument(Parser::Text(TextKind::Word)),
        &[],
        Some(0),
    );
    flagged.suggest = true;
    let tree = CommandTree {
        nodes: vec![
            node("cmd", NodeKind::Literal, &[1, 3], Some(9)),
            node("set", NodeKind::Literal, &[2], None),
            flagged.clone(),
            node(
                "count",
                NodeKind::Argument(Parser::Integer(IntRange {
                    min: None,
                    max: None,
                })),
                &[4],
                None,
            ),
            flagged,
        ],
    };
    let cx = Context {
        permitted: &open,
        origin: None,
        sender: None,
        online: &|| Vec::new(),
    };
    let at = |line: &str| -> Vec<(String, usize, String)> {
        tree::suggest_targets(&tree, line, &cx)
            .into_iter()
            .map(|t| (t.node, t.args.len(), t.partial))
            .collect()
    };
    assert_eq!(at("cmd set Al"), [("name".into(), 0, "Al".into())]);
    assert_eq!(at("cmd 5 "), [("name".into(), 1, String::new())]);
    assert!(at("cmd").is_empty());
    assert!(at("cmd x ").is_empty());
    assert!(at("other set ").is_empty());
}

#[tuff_core::watchdog]
#[test]
fn nearest_and_random_selectors_resolve_players() {
    let tree = CommandTree {
        nodes: vec![
            node("who", NodeKind::Literal, &[1], None),
            node("one", NodeKind::Argument(Parser::Player), &[], Some(0)),
        ],
    };
    let roster = || {
        vec![
            (named("Far", 1), Some([100.0, 0.0, 0.0])),
            (named("Near", 2), Some([1.0, 0.0, 0.0])),
        ]
    };
    let cx = Context {
        permitted: &open,
        origin: Some([0.0, 0.0, 0.0]),
        sender: None,
        online: &roster,
    };
    let run = |line: &str| tree::walk(&tree, line, &cx).map(|m| m.args[0].value.clone());
    assert_eq!(run("who @p"), Ok(ArgValue::Player(named("Near", 2))));
    assert!(matches!(run("who @r"), Ok(ArgValue::Player(_))));
    assert!(run("who @e").is_err());
}

#[tuff_core::watchdog]
#[test]
fn flags_past_thirty_two_members_use_two_words() {
    let names: Vec<String> = (0..40).map(|i| format!("g{i}")).collect();
    let wit = format!(
        "package t:t; interface i {{ flags wide {{ {} }} f: func(x: wide); }} world w {{ export i; }}",
        names.join(", ")
    );
    let schema = Schema::parse(&wit, "w").unwrap();
    let layouts = params(&schema, "f");
    let value = Value::Flags(1 << 35 | 1 << 3);
    let mut arena = Arena::new(4096, 64);
    let mut flat = Vec::new();
    canon::lower_flat(&layouts[0], &value, &mut arena, &mut flat).unwrap();
    assert_eq!(flat, [FlatVal::I32(8), FlatVal::I32(8)]);
    let ptr = arena.realloc(layouts[0].align, layouts[0].size).unwrap();
    canon::store(&layouts[0], &value, &mut arena, ptr).unwrap();
    assert_eq!(layouts[0].size, 8);
    let loaded = canon::load(
        &layouts[0],
        &arena.mem,
        ptr,
        &mut Budget::new(Bounds::default()),
    );
    assert_eq!(loaded.unwrap(), value);
    assert_eq!(
        canon::store(&layouts[0], &Value::Flags(1 << 40), &mut arena, ptr),
        Err(CanonError::TypeMismatch)
    );
}

fn host_functions(all_features: bool) -> Vec<(String, wit_parser::Stability)> {
    let mut sources = wit_parser::SourceMap::new();
    for (name, text) in super::FILES {
        sources.push_str(name, text);
    }
    let mut resolve = wit_parser::Resolve {
        all_features,
        ..wit_parser::Resolve::default()
    };
    let group = sources.parse().map_err(|(_, err)| err).unwrap();
    let package = resolve.push_group(group).unwrap();
    resolve
        .select_world(&[package], Some(super::WORLD))
        .unwrap();
    let host = resolve.packages[package].interfaces["host"];
    resolve.interfaces[host]
        .functions
        .iter()
        .map(|(name, func)| (name.clone(), func.stability.clone()))
        .collect()
}

#[tuff_core::watchdog]
#[test]
fn experimental_functions_are_gated_and_the_rest_resolves_without_them() {
    let all = host_functions(true);
    let stable = host_functions(false);
    let unstable: Vec<&str> = all
        .iter()
        .filter(|(_, s)| matches!(s, wit_parser::Stability::Unstable { .. }))
        .map(|(name, _)| name.as_str())
        .collect();
    for name in [
        "open-menu",
        "register-panel-page",
        "create-dimension",
        "add-block-rule",
    ] {
        assert!(unstable.contains(&name), "{name}");
    }
    assert_eq!(stable.len() + unstable.len(), all.len());
    assert!(
        stable
            .iter()
            .all(|(name, _)| !unstable.contains(&name.as_str()))
    );
    for name in [
        "log",
        "register-command",
        "config-get",
        "storage-set",
        "schedule",
    ] {
        let (_, stability) = stable.iter().find(|(n, _)| n == name).unwrap();
        assert!(
            matches!(stability, wit_parser::Stability::Stable { .. }),
            "{name}"
        );
    }
}
