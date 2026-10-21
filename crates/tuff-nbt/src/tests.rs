use super::*;
use flate2::write::ZlibEncoder;

#[tuff_core::watchdog]
#[test]
fn reused_deflate_state_matches_a_fresh_encoder() {
    let mut rng = 0x1234_5678_9ABC_DEF1u64;
    for len in [0usize, 1, 7, 100, 4096, 70_000, 300_000] {
        for noise in [0u64, 3, 255] {
            let raw: Vec<u8> = (0..len)
                .map(|i| {
                    rng ^= rng << 13;
                    rng ^= rng >> 7;
                    rng ^= rng << 17;
                    if noise == 0 {
                        (i % 13) as u8
                    } else {
                        (rng % (noise + 1)) as u8
                    }
                })
                .collect();
            let mut fresh = ZlibEncoder::new(Vec::new(), Level::default());
            fresh.write_all(&raw).unwrap();
            let fresh = fresh.finish().unwrap();
            let mut reused = vec![9u8, 9];
            zlib_compress_append(&raw, &mut reused).unwrap();
            assert_eq!(&reused[..2], &[9, 9]);
            let decoded = Compression::Zlib
                .decompress(&reused[2..], raw.len() + 1)
                .unwrap();
            assert_eq!(&*decoded, &raw[..]);
            assert_eq!(&reused[2..], &fresh[..], "len {len} noise {noise}");
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn numeric_getters_coerce_across_numeric_tags() {
    let c = Compound::new()
        .with("l", 0x1_0000_0105i64)
        .with("d", -1.5f64)
        .with("s", 7i16)
        .with("str", "x");
    assert_eq!(c.get_int("l"), Some(0x105));
    assert_eq!(c.get_byte("l"), Some(5));
    assert_eq!(c.get_int("d"), Some(-2));
    assert_eq!(c.get_long("d"), Some(-2));
    assert_eq!(c.get_double("s"), Some(7.0));
    assert_eq!(c.get_float("l"), Some(0x1_0000_0105i64 as f32));
    assert_eq!(c.get_bool("s"), Some(true));
    assert_eq!(c.get_int("str"), None);
}

#[tuff_core::watchdog]
#[test]
fn snbt_sorts_keys_and_quotes_like_vanilla() {
    let c = Compound::new()
        .with("b", 1i8)
        .with("1a", 1i8)
        .with("true", 1i8)
        .with("a.b", 1i8);
    assert_eq!(c.to_string(), r#"{"1a":1b,a.b:1b,b:1b,"true":1b}"#);
    assert_eq!(Tag::from("a\"b'c").to_string(), r#"'a"b\'c'"#);
    assert_eq!(Tag::from("a'b\"c").to_string(), r#""a'b\"c""#);
    assert_eq!(Tag::Double(1.0e10).to_string(), "1.0E10d");
    assert_eq!(Tag::Float(0.0001).to_string(), "1.0E-4f");
    assert_eq!(Tag::Double(2.5).to_string(), "2.5d");
}

const HELLO_WORLD: &[u8] = b"\x0a\x00\x0bhello world\x08\x00\x04name\x00\x09Bananrama\x00";

fn sample() -> Compound {
    Compound::new()
        .with("byte", 1i8)
        .with("short", -2i16)
        .with("int", 3)
        .with("long", i64::MIN)
        .with("float", 1.5f32)
        .with("double", -0.25)
        .with("bytes", vec![1i8, -1, 0])
        .with("string", "a\0\u{1F600}")
        .with("list", vec![Tag::Int(1), Tag::Int(2)])
        .with("empty", Vec::<Tag>::new())
        .with(
            "compounds",
            vec![Compound::new().with("x", 1), Compound::new()],
        )
        .with("nested", Compound::new().with("inner", Compound::new()))
        .with("ints", vec![i32::MIN, 0, i32::MAX])
        .with("longs", vec![1i64, -1])
}

fn snbt_err(input: &str) -> &'static str {
    match Tag::from_snbt(input) {
        Err(NbtError::Snbt { reason, .. }) => reason,
        other => panic!("expected SNBT error for {input:?}, got {other:?}"),
    }
}

#[tuff_core::watchdog]
#[test]
fn selected_keys_are_decoded_and_the_rest_skipped() {
    let full = NamedTag::new(
        "root",
        Compound::new()
            .with("big", vec![Tag::from(Compound::new().with("x", 1i32)); 4])
            .with("longs", Tag::LongArray(vec![7; 100]))
            .with("want", "yes")
            .with("tail", 5i8),
    );
    let mut bytes = Vec::new();
    full.write(&mut bytes).unwrap();
    let picked = NamedTag::read_selected(&bytes, NbtLimits::TRUSTED, &["want", "absent"]).unwrap();
    let compound = picked.tag.as_compound().unwrap();
    assert_eq!(compound.get_str("want"), Some("yes"));
    assert_eq!(compound.len(), 1);

    let both = NamedTag::read_selected(&bytes, NbtLimits::TRUSTED, &["tail", "want"]).unwrap();
    assert_eq!(both.tag.as_compound().unwrap().len(), 2);
}

#[tuff_core::watchdog]
#[test]
fn skipping_is_bounded_in_depth_and_input() {
    let mut deep = vec![10u8, 0, 0];
    for _ in 0..(MAX_DEPTH + 8) {
        deep.extend_from_slice(&[10, 0, 1, b'a']);
    }
    assert!(matches!(
        NamedTag::read_selected(&deep, NbtLimits::TRUSTED, &["zzz"]),
        Err(NbtError::DepthLimit(_))
    ));

    let truncated = [10u8, 0, 0, 7, 0, 1, b'a', 0x7f, 0xff, 0xff, 0xff, 1];
    assert!(matches!(
        NamedTag::read_selected(&truncated, NbtLimits::TRUSTED, &["zzz"]),
        Err(NbtError::UnexpectedEof)
    ));

    let huge_list = [10u8, 0, 0, 9, 0, 1, b'a', 3, 0x7f, 0xff, 0xff, 0xff];
    assert!(matches!(
        NamedTag::read_selected(&huge_list, NbtLimits::TRUSTED, &["zzz"]),
        Err(NbtError::UnexpectedEof)
    ));
}

#[tuff_core::watchdog]
#[test]
fn decodes_hello_world() {
    let mut input = HELLO_WORLD;
    let named = NamedTag::read(&mut input, NbtLimits::TRUSTED).unwrap();
    assert!(input.is_empty());
    assert_eq!(named.name, "hello world");
    assert_eq!(
        named.tag.as_compound().unwrap().get_str("name"),
        Some("Bananrama")
    );

    let mut out = Vec::new();
    named.write(&mut out).unwrap();
    assert_eq!(out, HELLO_WORLD);
}

#[tuff_core::watchdog]
#[test]
fn named_round_trip_all_types() {
    let named = NamedTag::new("root", sample());
    let mut out = Vec::new();
    named.write(&mut out).unwrap();
    assert_eq!(
        NamedTag::read(&mut &out[..], NbtLimits::TRUSTED).unwrap(),
        named
    );
}

#[tuff_core::watchdog]
#[test]
fn network_round_trip_and_advance() {
    let tag = Tag::Compound(sample());
    let mut bytes = tag.to_network_bytes().unwrap();
    bytes.extend_from_slice(&[0xAA, 0xBB]);
    let mut input = &bytes[..];
    assert_eq!(
        Tag::read_network(&mut input, NbtLimits::NETWORK).unwrap(),
        Some(tag)
    );
    assert_eq!(input, [0xAA, 0xBB]);
}

#[tuff_core::watchdog]
#[test]
fn network_root_can_be_any_type_or_end() {
    assert_eq!(
        Tag::from("hi").to_network_bytes().unwrap(),
        b"\x08\x00\x02hi"
    );
    let mut input: &[u8] = &[0x00, 0x01];
    assert_eq!(
        Tag::read_network(&mut input, NbtLimits::NETWORK).unwrap(),
        None
    );
    assert_eq!(input, [0x01]);
}

#[tuff_core::watchdog]
#[test]
fn modified_utf8_encoding() {
    let bytes = Tag::from("a\0\u{1F600}").to_network_bytes().unwrap();
    assert_eq!(
        bytes,
        [
            0x08, 0x00, 0x09, 0x61, 0xC0, 0x80, 0xED, 0xA0, 0xBD, 0xED, 0xB8, 0x80
        ]
    );
    let decoded = Tag::read_network(&mut &bytes[..], NbtLimits::NETWORK).unwrap();
    assert_eq!(decoded, Some(Tag::from("a\0\u{1F600}")));
}

#[tuff_core::watchdog]
#[test]
fn unpaired_surrogate_is_replaced() {
    let bytes: &[u8] = &[0x08, 0x00, 0x03, 0xED, 0xA0, 0xBD];
    let decoded = Tag::read_network(&mut &bytes[..], NbtLimits::NETWORK).unwrap();
    assert_eq!(decoded, Some(Tag::from("\u{FFFD}")));
}

#[tuff_core::watchdog]
#[test]
fn malformed_string_is_rejected() {
    let bytes: &[u8] = &[0x08, 0x00, 0x02, 0xC0, 0x41];
    assert!(matches!(
        Tag::read_network(&mut &bytes[..], NbtLimits::NETWORK),
        Err(NbtError::InvalidString)
    ));
}

#[tuff_core::watchdog]
#[test]
fn overlong_string_fails_to_encode() {
    let tag = Tag::String("x".repeat(65536));
    assert!(matches!(
        tag.to_network_bytes(),
        Err(NbtError::StringTooLong(65536))
    ));
    assert!(Tag::String("x".repeat(65535)).to_network_bytes().is_ok());
}

#[tuff_core::watchdog]
#[test]
fn heterogeneous_list_is_wrapped_and_unwrapped() {
    let tag = Tag::List(vec![Tag::Int(1), Tag::from("a")]);
    let bytes = tag.to_network_bytes().unwrap();
    assert_eq!(
        bytes,
        [
            0x09, 0x0A, 0, 0, 0, 2, //
            0x03, 0, 0, 0, 0, 0, 1, 0x00, //
            0x08, 0, 0, 0, 1, b'a', 0x00,
        ]
    );
    assert_eq!(
        Tag::read_network(&mut &bytes[..], NbtLimits::NETWORK).unwrap(),
        Some(tag)
    );
}

#[tuff_core::watchdog]
#[test]
fn wrapper_shaped_compounds_survive_round_trip() {
    let wrapper = Compound::new().with("", 1);
    for tag in [
        Tag::List(vec![Tag::Compound(wrapper.clone())]),
        Tag::List(vec![
            Tag::Compound(wrapper),
            Tag::Compound(Compound::new()),
            Tag::Int(5),
        ]),
    ] {
        let bytes = tag.to_network_bytes().unwrap();
        assert_eq!(
            Tag::read_network(&mut &bytes[..], NbtLimits::NETWORK).unwrap(),
            Some(tag)
        );
    }
}

#[tuff_core::watchdog]
#[test]
fn rejects_malformed_binary() {
    type TestCase = (&'static [u8], fn(&NbtError) -> bool);
    let cases: &[TestCase] = &[
        (&[0x0D], |e| matches!(e, NbtError::InvalidTagType(13))),
        (&[0x03, 0x00], |e| matches!(e, NbtError::UnexpectedEof)),
        (&[0x09, 0x00, 0, 0, 0, 1], |e| {
            matches!(e, NbtError::UntypedList)
        }),
        (&[0x0B, 0xFF, 0xFF, 0xFF, 0xFF], |e| {
            matches!(e, NbtError::NegativeLength(-1))
        }),
        (&[0x0C, 0x7F, 0xFF, 0xFF, 0xFF], |e| {
            matches!(e, NbtError::UnexpectedEof)
        }),
        (&[0x09, 0x04, 0x7F, 0xFF, 0xFF, 0xFF], |e| {
            matches!(e, NbtError::UnexpectedEof)
        }),
        (&[0x0A, 0x01, 0x00, 0x00], |e| {
            matches!(e, NbtError::UnexpectedEof)
        }),
    ];
    for (bytes, check) in cases {
        let err = Tag::read_network(&mut &bytes[..], NbtLimits::NETWORK).unwrap_err();
        assert!(check(&err), "{bytes:02x?} gave {err:?}");
    }
    assert!(matches!(
        NamedTag::read(&mut &[0x00][..], NbtLimits::TRUSTED),
        Err(NbtError::UnexpectedEnd)
    ));
}

#[tuff_core::watchdog]
#[test]
fn enforces_depth_limit() {
    let nest = |levels: usize| (0..levels).fold(Tag::List(vec![]), |t, _| Tag::List(vec![t]));
    let ok = nest(MAX_DEPTH - 1).to_network_bytes().unwrap();
    assert!(Tag::read_network(&mut &ok[..], NbtLimits::NETWORK).is_ok());
    let deep = nest(MAX_DEPTH).to_network_bytes().unwrap();
    assert!(matches!(
        Tag::read_network(&mut &deep[..], NbtLimits::NETWORK),
        Err(NbtError::DepthLimit(MAX_DEPTH))
    ));
}

/// Builds nested-List payload bytes iteratively (never recursing), so the
/// test itself cannot overflow while producing adversarial input that is
/// far deeper than any recursive decoder could survive.
fn nested_list_bytes(levels: usize) -> Vec<u8> {
    // Each level contributes a 5-byte [element type][length] header; the
    // list-of-list chain closes with an empty (element = End) list.
    let mut bytes = Vec::with_capacity(1 + 5 * (levels + 1));
    bytes.push(TagType::List.id());
    for _ in 0..levels {
        bytes.push(TagType::List.id());
        bytes.extend_from_slice(&1i32.to_be_bytes());
    }
    bytes.push(TagType::End.id());
    bytes.extend_from_slice(&0i32.to_be_bytes());
    bytes
}

#[tuff_core::watchdog]
#[test]
fn decode_rejects_extreme_nesting_without_stack_overflow() {
    // Two orders of magnitude past NbtLimits::NETWORK's max_depth. A
    // recursive decoder would blow the native stack walking down to the
    // depth check; the iterative decoder must bail out cleanly instead.
    let bytes = nested_list_bytes(1_000_000);
    assert!(matches!(
        Tag::read_network(&mut &bytes[..], NbtLimits::NETWORK),
        Err(NbtError::DepthLimit(MAX_DEPTH))
    ));
}

#[tuff_core::watchdog]
#[test]
fn enforces_size_limit() {
    let tag = Tag::from(vec![Compound::new(); 1000]);
    let bytes = tag.to_network_bytes().unwrap();
    let limits = NbtLimits {
        max_bytes: 1000,
        ..NbtLimits::NETWORK
    };
    assert!(matches!(
        Tag::read_network(&mut &bytes[..], limits),
        Err(NbtError::SizeLimit)
    ));
    assert!(Tag::read_network(&mut &bytes[..], NbtLimits::NETWORK).is_ok());
}

#[tuff_core::watchdog]
#[test]
fn file_round_trip_with_each_compression() {
    let named = NamedTag::new("", sample());
    for compression in [Compression::None, Compression::Gzip, Compression::Zlib] {
        let bytes = named.to_bytes(compression).unwrap();
        assert_eq!(Compression::detect(&bytes), compression);
        let (decoded, detected) = NamedTag::from_bytes(&bytes, NbtLimits::TRUSTED).unwrap();
        assert_eq!(detected, compression);
        assert_eq!(decoded, named);
    }
}

#[tuff_core::watchdog]
#[test]
fn uncompressed_roots_are_never_detected_as_compressed() {
    for id in 0..=12u8 {
        for second in 0..=255u8 {
            assert_eq!(Compression::detect(&[id, second]), Compression::None);
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn decompression_is_bounded() {
    let bytes = Compression::Gzip.compress(vec![0; 10_000]).unwrap();
    assert!(matches!(
        Compression::Gzip.decompress(&bytes, 9_999),
        Err(NbtError::SizeLimit)
    ));
    assert_eq!(
        Compression::Gzip.decompress(&bytes, 10_000).unwrap().len(),
        10_000
    );
}

#[tuff_core::watchdog]
#[test]
fn uuid_helpers() {
    let uuid = 0xf81d4fae_7dec_11d0_a765_00a0c91e6bf6u128;
    let tag = Tag::uuid(uuid);
    assert_eq!(
        tag,
        Tag::IntArray(vec![
            0xf81d4faeu32 as i32,
            0x7dec11d0,
            0xa76500a0u32 as i32,
            0xc91e6bf6u32 as i32
        ])
    );
    assert_eq!(tag.as_uuid(), Some(uuid));
    assert_eq!(
        Tag::from_snbt("uuid(\"f81d4fae-7dec-11d0-a765-00a0c91e6bf6\")").unwrap(),
        tag
    );
}

#[tuff_core::watchdog]
#[test]
fn snbt_numbers() {
    let cases = [
        ("1b", Tag::Byte(1)),
        ("-128B", Tag::Byte(-128)),
        ("255ub", Tag::Byte(-1)),
        ("12s", Tag::Short(12)),
        ("-5sS", Tag::Short(-5)),
        ("7", Tag::Int(7)),
        ("+7i", Tag::Int(7)),
        ("1_000", Tag::Int(1000)),
        ("9000000000L", Tag::Long(9_000_000_000)),
        ("18446744073709551615ul", Tag::Long(-1)),
        ("0xFF", Tag::Int(255)),
        ("0x1b", Tag::Int(27)),
        ("0xFFub", Tag::Byte(-1)),
        ("0xFFFFFFFF", Tag::Int(-1)),
        ("0b101", Tag::Int(5)),
        ("0b", Tag::Byte(0)),
        ("1.5", Tag::Double(1.5)),
        ("1.5f", Tag::Float(1.5)),
        ("2d", Tag::Double(2.0)),
        ("3F", Tag::Float(3.0)),
        (".5", Tag::Double(0.5)),
        ("1.", Tag::Double(1.0)),
        ("1e3", Tag::Double(1000.0)),
        ("-1.5e-2f", Tag::Float(-0.015)),
        ("1_0.2_5", Tag::Double(10.25)),
        ("true", Tag::Byte(1)),
        ("false", Tag::Byte(0)),
        ("bool(0)", Tag::Byte(0)),
        ("bool(2.5)", Tag::Byte(1)),
        ("bool(true)", Tag::Byte(1)),
    ];
    for (input, expected) in cases {
        assert_eq!(Tag::from_snbt(input).unwrap(), expected, "{input}");
    }
}

#[tuff_core::watchdog]
#[test]
fn snbt_unquoted_strings() {
    for word in [
        "abc",
        "1abc",
        "1e",
        "minecraft.stone",
        "_x-y+z",
        "-",
        ".",
        "1__",
    ] {
        assert_eq!(Tag::from_snbt(word).unwrap(), Tag::from(word), "{word}");
    }
}

#[tuff_core::watchdog]
#[test]
fn snbt_quoted_strings() {
    let cases = [
        (r#""plain""#, "plain"),
        (r#"'single "quoted"'"#, "single \"quoted\""),
        (
            r#""esc \\ \" \' \n\t\r\b\f\s""#,
            "esc \\ \" ' \n\t\r\u{8}\u{c} ",
        ),
        (r#""\x41\u00e9\U0001F600""#, "A\u{e9}\u{1F600}"),
        (r#""""#, ""),
    ];
    for (input, expected) in cases {
        assert_eq!(
            Tag::from_snbt(input).unwrap(),
            Tag::from(expected),
            "{input}"
        );
    }
}

#[tuff_core::watchdog]
#[test]
fn snbt_structures() {
    let tag = Tag::from_snbt(
            " { a : 1b , 'b c' : [ 1 , 2 , ] , \"d\":{}, e:[B; 1b, 255ub, true], f:[I;], g:[L; 1L, 2], h:[1, \"x\", {}], } ",
        )
        .unwrap();
    let expected = Compound::new()
        .with("a", 1i8)
        .with("b c", vec![Tag::Int(1), Tag::Int(2)])
        .with("d", Compound::new())
        .with("e", vec![1i8, -1, 1])
        .with("f", Vec::<i32>::new())
        .with("g", vec![1i64, 2])
        .with(
            "h",
            vec![Tag::Int(1), Tag::from("x"), Tag::Compound(Compound::new())],
        );
    assert_eq!(tag, Tag::Compound(expected));
    assert_eq!(
        Tag::from_snbt("[B]").unwrap(),
        Tag::List(vec![Tag::from("B")])
    );
}

#[tuff_core::watchdog]
#[test]
fn snbt_errors() {
    let cases = [
        ("128b", "integer out of range"),
        ("-1ub", "integer out of range"),
        ("3000000000", "integer out of range"),
        ("99999999999999999999L", "integer out of range"),
        ("{a:1", "expected ',' or '}'"),
        ("{a 1}", "expected ':'"),
        ("{,}", "expected key"),
        ("[1,2", "expected ',' or ']'"),
        ("\"abc", "unterminated string"),
        ("\"\\q\"", "invalid escape sequence"),
        ("\"\\ud800\"", "invalid escape sequence"),
        ("[B; 1.5]", "expected integer array element"),
        ("[B; 300]", "array element out of range"),
        ("uuid(\"nope\")", "invalid UUID"),
        ("uuid(1)", "uuid() expects a string"),
        ("bool(\"x\")", "bool() expects a number"),
        ("foo(1)", "unknown operation"),
        ("1 2", "trailing characters"),
        ("minecraft:stone", "trailing characters"),
        ("", "expected value"),
    ];
    for (input, reason) in cases {
        assert_eq!(snbt_err(input), reason, "{input}");
    }
    let deep = format!("{}{}", "[".repeat(MAX_DEPTH + 1), "]".repeat(MAX_DEPTH + 1));
    assert!(matches!(
        Tag::from_snbt(&deep),
        Err(NbtError::DepthLimit(MAX_DEPTH))
    ));
}

#[tuff_core::watchdog]
#[test]
fn snbt_compact_output() {
    let tag = Tag::Compound(
        Compound::new()
            .with("name", "Steve")
            .with("health", 20.0f32)
            .with(
                "pos",
                vec![Tag::Double(1.5), Tag::Double(1e300), Tag::Double(-3.0)],
            )
            .with("flags", vec![1i8, 0])
            .with("odd key", 1i64)
            .with("", Compound::new())
            .with("n", vec![Tag::Short(1), Tag::from("x")]),
    );
    let text = tag.to_string();
    assert_eq!(
        text,
        r#"{"":{},flags:[B;1b,0b],health:20.0f,n:[1s,"x"],name:"Steve","odd key":1L,pos:[1.5d,1.0E300d,-3.0d]}"#
    );
    assert_eq!(Tag::from_snbt(&text).unwrap(), tag);
}

#[tuff_core::watchdog]
#[test]
fn snbt_pretty_output() {
    let tag = Tag::Compound(
        Compound::new()
            .with("a", 1i8)
            .with("l", vec![Tag::Int(1), Tag::Int(2)])
            .with("c", Compound::new())
            .with("i", vec![1, 2]),
    );
    let expected =
        "{\n    a: 1b,\n    c: {},\n    i: [I; 1, 2],\n    l: [\n        1,\n        2\n    ]\n}";
    assert_eq!(format!("{tag:#}"), expected);
    assert_eq!(Tag::from_snbt(expected).unwrap(), tag);
}

#[tuff_core::watchdog]
#[test]
fn snbt_string_quoting() {
    let cases = [
        ("say \"hi\"", r#"'say "hi"'"#),
        ("it's \"x\"", r#""it's \"x\"""#),
        ("a\nb\\\u{1}", r#""a\nb\\\x01""#),
    ];
    for (value, expected) in cases {
        let text = Tag::from(value).to_string();
        assert_eq!(text, expected);
        assert_eq!(Tag::from_snbt(&text).unwrap(), Tag::from(value));
    }
}

#[tuff_core::watchdog]
#[test]
fn snbt_round_trips_sample() {
    let tag = Tag::Compound(sample());
    assert_eq!(Tag::from_snbt(&tag.to_string()).unwrap(), tag);
    assert_eq!(Tag::from_snbt(&format!("{tag:#}")).unwrap(), tag);
}

#[tuff_core::watchdog]
#[test]
fn compound_accessors() {
    let mut c = sample();
    assert_eq!(c.get_byte("byte"), Some(1));
    assert_eq!(c.get_bool("byte"), Some(true));
    assert_eq!(c.get_int("byte"), Some(1));
    assert_eq!(c.get_int("name"), None);
    assert_eq!(c.get_long_array("longs"), Some(&[1i64, -1][..]));
    c.get_compound_mut("nested").unwrap().insert("k", "v");
    assert_eq!(c.get_compound("nested").unwrap().get_str("k"), Some("v"));
    c.get_list_mut("list").unwrap().push(Tag::Int(3));
    assert_eq!(c.get_list("list").unwrap().len(), 3);
    assert_eq!(c.remove("byte"), Some(Tag::Byte(1)));
    assert_eq!(c.keys().next(), Some("short"));
}
