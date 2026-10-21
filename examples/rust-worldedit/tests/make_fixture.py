import gzip, struct, sys
def s(x): return struct.pack('>H', len(x)) + x.encode()
def tag(t, n, p): return bytes([t]) + s(n) + p
def comp(items): return b''.join(items) + b'\0'
def vi(v):
    o = b''
    while v >= 0x80:
        o += bytes([(v & 0x7f) | 0x80]); v >>= 7
    return o + bytes([v])
pal = comp([tag(3, "minecraft:stone", struct.pack('>i', 0)),
            tag(3, "minecraft:air", struct.pack('>i', 1)),
            tag(3, "minecraft:oak_stairs[facing=east,half=bottom]", struct.pack('>i', 2))])
cells = [0,2,0, 1,1,1, 0,0,0, 1,1,2]
data = b''.join(vi(c) for c in cells)
blocks = comp([tag(10, "Palette", pal), tag(7, "Data", struct.pack('>i', len(data)) + data),
               tag(9, "BlockEntities", bytes([0]) + struct.pack('>i', 0))])
meta = comp([tag(8, "Name", s("fixture")), tag(3, "Date", struct.pack('>i', 1))])
schem = comp([tag(3, "Version", struct.pack('>i', 3)), tag(3, "DataVersion", struct.pack('>i', 3700)),
              tag(2, "Width", struct.pack('>h', 3)), tag(2, "Height", struct.pack('>h', 2)),
              tag(2, "Length", struct.pack('>h', 2)),
              tag(11, "Offset", struct.pack('>i', 3) + struct.pack('>3i', -1, 0, 1)),
              tag(10, "Metadata", meta), tag(10, "Blocks", blocks)])
root = bytes([10]) + s("") + comp([tag(10, "Schematic", schem)])
open(sys.argv[1], 'wb').write(gzip.compress(root, 9))
