# MiniMessage conformance fixtures

`adventure.json` holds golden cases for `crates/tuff-core/src/minimessage/conformance.rs`.
Each case has an `input` and the `expected` text component JSON produced by the reference
implementation (Kyori Adventure `adventure-text-minimessage` 5.2.0, MIT licensed; see the
`_attribution` field for the license text), or an `error` when the reference throws.

How they were produced (no Java needed to run the tests, only to regenerate):

1. `python3 -I generator/extract.py <adventure>/text-minimessage/src/test/java/net/kyori/adventure/text/minimessage > inputs.json`
   pulls every input string from Adventure's own tag and parser tests.
2. `generator/extra.json` adds hand-written cases for tag families those tests miss.
3. With a user-local JDK 21 (e.g. an unpacked Temurin tarball) and the 5.2.0 jars from
   Maven Central (adventure-api, -key, -nbt, -text-minimessage, -text-serializer-gson,
   -text-serializer-json, -text-serializer-commons, option 1.1.0, gson 2.13.2, jspecify,
   jetbrains annotations) on the classpath:
   `java -cp '<jars>' generator/Gen.java inputs.json generator/extra.json > out.json`
4. Wrap `out.json` as `{"_attribution": ..., "cases": [...]}`.

The test compares rendered meaning, not tree shape: both trees are flattened into runs of
content with fully resolved style, adjacent equal-style text runs are merged and empty text
runs dropped. Intentional differences are listed in `KNOWN_DIFFERENCES` in the test.

Encodings treated as equal by the comparison (same meaning to the client): named colours and
their hex values, ids with and without `minecraft:`, default fields spelled out or omitted
(`hat: true`, `interpret`/`plain: false`, the `minecraft:blocks` atlas, the `object` type
field), string shorthands for `separator`, hover entity `name` and head `player`, and UUIDs as
int arrays or strings.

Known differences (listed in the test):

- `MiniMessageParserTest.testGH5`, `testGH5Modified`: the original tests rely on a `<pack_url>`
  placeholder; without it Adventure rejects the whole `<click>` tag, we keep the URL text as
  given.
- `NbtTagTest.testParseBlockNbt`: Adventure rewrites `^0 ^0 ^0` as `^0.0 ^0.0 ^0.0`; we keep
  the coordinates as written (the client reads both the same).
- `MiniMessageParserTest.testEscapeIgnoresSectionSigns` (`error` in the fixture): Adventure's
  strict default throws on legacy `§` codes; we keep them as literal text.
