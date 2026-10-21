#!/usr/bin/env python3
"""Appends a tuff-plugin manifest custom section to a WebAssembly module.

Toolchains that cannot emit custom sections from source (clang with wasm-ld) run this after linking:

    python3 add-manifest.py module.wasm manifest.txt
"""

import sys

SECTION = b"tuff-plugin"


def leb(value):
    out = bytearray()
    while True:
        byte = value & 0x7F
        value >>= 7
        out.append(byte | (0x80 if value else 0))
        if not value:
            return bytes(out)


def main():
    module_path, manifest_path = sys.argv[1:3]
    with open(manifest_path, "rb") as f:
        manifest = f.read()
    body = leb(len(SECTION)) + SECTION + manifest
    with open(module_path, "ab") as f:
        f.write(b"\x00" + leb(len(body)) + body)
    print(f"added the manifest to {module_path}")


if __name__ == "__main__":
    main()
