// Encodes and decodes the shared wire fixture (crates/plugin-sdk-gen/fixtures/demo)
// with the generated C++ header, so tests can compare its bytes with the Rust
// codec's.
//
// - /iface encode <name> replies with the hex of the encoded value
// - /iface decode <name> <hex> replies "ok <hex>" (decoded, then encoded again) or "bad"
#include "tuff.hpp"

#include "fixtures/iface_demo.hpp"

using namespace tuff;

namespace {

String hex(const Bytes &bytes) {
    static const char digits[] = "0123456789abcdef";
    char out[1024];
    size_t n = 0;
    for (size_t i = 0; i < bytes.len() && n + 2 < sizeof(out); i++) {
        out[n++] = digits[bytes.data()[i] >> 4];
        out[n++] = digits[bytes.data()[i] & 15];
    }
    return String::copy(Str{reinterpret_cast<uint8_t *>(out), n});
}

bool unhex(Str text, List<uint8_t> &out) {
    if (text.len % 2) {
        return false;
    }
    for (size_t i = 0; i < text.len; i += 2) {
        uint8_t value = 0;
        for (size_t k = 0; k < 2; k++) {
            uint8_t c = text.ptr[i + k];
            uint8_t digit;
            if (c >= '0' && c <= '9') {
                digit = c - '0';
            } else if (c >= 'a' && c <= 'f') {
                digit = c - 'a' + 10;
            } else {
                return false;
            }
            value = (value << 4) | digit;
        }
        out.push(value);
    }
    return true;
}

control::Point point(int32_t x, int32_t y) {
    control::Point p;
    p.x = x;
    p.y = y;
    return p;
}

control::Named named() {
    control::Named n;
    n.name = String::copy("ab");
    n.tags.push(String::copy("x"));
    n.tags.push(String::copy("yz"));
    n.at = Option<control::Point>(point(-1, 2));
    return n;
}

List<control::Shape> shapes() {
    List<control::Shape> list;
    list.push(control::Shape::of_dot());
    list.push(control::Shape::of_circle(1.5));
    List<control::Point> poly;
    poly.push(point(1, 2));
    poly.push(point(3, 4));
    list.push(control::Shape::of_poly(move(poly)));
    Tuple<uint8_t, String> pair;
    pair.f0 = 7;
    pair.f1 = String::copy("q");
    list.push(control::Shape::of_pair(move(pair)));
    return list;
}

control::Every every() {
    control::Every e;
    e.a = 0xab;
    e.b = 60000;
    e.c = 0xdeadbeef;
    e.d = 0x0102030405060708ull;
    e.e = -5;
    e.f = -3;
    e.g = -9;
    e.h = static_cast<int64_t>(0x8000000000000000ull);
    e.i = 1.5f;
    e.j = -2.25;
    e.k = true;
    e.l = 0x1f600;
    e.m = String::copy("h\xc3\xa9llo");
    return e;
}

Bytes encode_named(Str name, bool &known) {
    known = true;
    if (name == str("point")) {
        return wire::encoded(point(3, -4));
    } else if (name == str("named")) {
        return wire::encoded(named());
    } else if (name == str("shapes")) {
        return wire::encoded(shapes());
    } else if (name == str("color")) {
        return wire::encoded(control::Color::Blue);
    } else if (name == str("perms")) {
        control::Perms p;
        p.bits = control::Perms::READ | control::Perms::ADMIN;
        return wire::encoded(p);
    } else if (name == str("ids")) {
        control::Ids ids;
        ids.push(1);
        ids.push(0xffffffffffffffffull);
        return wire::encoded(ids);
    } else if (name == str("outcome-err")) {
        return wire::encoded(control::Outcome::make_err(String::copy("no")));
    } else if (name == str("outcome-ok")) {
        return wire::encoded(control::Outcome::make_ok(9));
    } else if (name == str("every")) {
        return wire::encoded(every());
    } else if (name == str("pick-some")) {
        Tuple<uint8_t, String> t;
        t.f0 = 9;
        t.f1 = String::copy("z");
        return wire::encoded(Option<Tuple<uint8_t, String>>(move(t)));
    } else if (name == str("pick-none")) {
        return wire::encoded(Option<Tuple<uint8_t, String>>());
    } else if (name == str("find-err")) {
        return wire::encoded(WireResult<control::Point, String>::make_err(String::copy("nowhere")));
    }
    known = false;
    return Bytes();
}

template <class T> String redecode(const List<uint8_t> &bytes) {
    T value{};
    if (!wire::decode_all(bytes.begin(), bytes.size(), value)) {
        return String::copy("bad");
    }
    String text = hex(wire::encoded(value));
    Text out;
    out << "ok " << text.view();
    return String::copy(out.view());
}

Result encode(Call &call) {
    Str name;
    call.get("name", name);
    bool known;
    Bytes bytes = encode_named(name, known);
    if (!known) {
        return Result::fail("unknown value");
    }
    String text = hex(bytes);
    call.reply(text.view());
    return 1;
}

Result decode_cmd(Call &call) {
    Str name;
    Str digits;
    call.get("name", name);
    call.get("hex", digits);
    List<uint8_t> bytes;
    if (!unhex(digits, bytes)) {
        return Result::fail("not hex");
    }
    String out;
    if (name == str("point")) {
        out = redecode<control::Point>(bytes);
    } else if (name == str("named")) {
        out = redecode<control::Named>(bytes);
    } else if (name == str("shapes")) {
        out = redecode<List<control::Shape>>(bytes);
    } else if (name == str("color")) {
        out = redecode<control::Color>(bytes);
    } else if (name == str("perms")) {
        out = redecode<control::Perms>(bytes);
    } else if (name == str("ids")) {
        out = redecode<control::Ids>(bytes);
    } else if (name == str("every")) {
        out = redecode<control::Every>(bytes);
    } else if (name == str("outcome")) {
        out = redecode<control::Outcome>(bytes);
    } else {
        return Result::fail("unknown type");
    }
    call.reply(out.view());
    return 1;
}

} // namespace

struct IfaceProbe {
    static Status on_register() {
        Tree tree("iface");
        Node root = tree.root();
        root.literal("encode").argument("name", parser::word()).executes(encode);
        root.literal("decode")
            .argument("name", parser::word())
            .argument("hex", parser::word())
            .executes(decode_cmd);
        return tree.register_command();
    }
};

TUFF_PLUGIN(IfaceProbe, "iface", "0.1.0", "Encodes the wire fixture");
