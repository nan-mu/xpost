#!/usr/bin/env bash
# Build exec_bench.elf (Compact ELF V0) from exec_bench.c.
# Requires: clang (riscv32), ld.lld, cargo (xpost-compact-elf via xpost-build).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
HERE="$(cd "$(dirname "$0")" && pwd)"
OUT_DIR="${HERE}/../"
SLOT_ADDR=0x40380200
HELPER_ADDR=0x40380000
MAX_IMAGE=0xE00

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

clang --target=riscv32 -march=rv32imc -mabi=ilp32 -O2 \
  -ffreestanding -fno-builtin -nostdlib -fno-stack-protector \
  -c -o "$TMP/exec_bench.o" "$HERE/exec_bench.c"

clang --target=riscv32 -march=rv32imc -mabi=ilp32 -O2 \
  -ffreestanding -fno-builtin -nostdlib \
  -fuse-ld=lld \
  -Wl,--section-start=.text=0x40380200 \
  -Wl,--section-start=.text.xdp=0x40380200 \
  -Wl,-e,xdp_exec_bench \
  -o "$TMP/exec_bench.linked.elf" "$TMP/exec_bench.o"

# Prefer .text.xdp; fall back to whole allocated PROGBITS load image.
python3 - <<PY
import struct, sys
from pathlib import Path
elf = Path("$TMP/exec_bench.linked.elf").read_bytes()
assert elf[:4] == b"\\x7fELF"
e_phoff, e_shoff = struct.unpack_from("<II", elf, 28)
e_phentsize, e_phnum = struct.unpack_from("<HH", elf, 42)
e_shentsize, e_shnum, e_shstrndx = struct.unpack_from("<HHH", elf, 46)
shstr_hdr = e_shoff + e_shstrndx * e_shentsize
shstr_off, shstr_size = struct.unpack_from("<II", elf, shstr_hdr + 16)
shstr = elf[shstr_off:shstr_off + shstr_size]
image = None
for i in range(e_shnum):
    off = e_shoff + i * e_shentsize
    name_off, typ, flags, addr, soff, size = struct.unpack_from("<IIIIII", elf, off)
    name = shstr[name_off:].split(b"\\0", 1)[0].decode()
    if name == ".text.xdp" and size:
        image = elf[soff:soff + size]
        print(f"section {name} addr={addr:#x} size={size:#x}", file=sys.stderr)
        break
if image is None:
    # Concatenate executable PROGBITS at/after slot base.
    chunks = []
    for i in range(e_shnum):
        off = e_shoff + i * e_shentsize
        name_off, typ, flags, addr, soff, size = struct.unpack_from("<IIIIII", elf, off)
        name = shstr[name_off:].split(b"\\0", 1)[0].decode()
        if typ == 1 and (flags & 0x4) and size and addr >= 0x40380200:
            chunks.append((addr, elf[soff:soff + size], name))
    if not chunks:
        sys.exit("no executable image found")
    chunks.sort()
    base = chunks[0][0]
    end = max(a + len(b) for a, b, _ in chunks)
    image = bytearray(end - base)
    for a, b, name in chunks:
        print(f"section {name} addr={a:#x} size={len(b):#x}", file=sys.stderr)
        rel = a - base
        image[rel:rel + len(b)] = b
    image = bytes(image)
Path("$TMP/image.bin").write_bytes(image)
print(f"image_len={len(image)}", file=sys.stderr)
PY

# Host binary — must not inherit workspace RV32 `build.target`.
HOST_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/xpost-build/target}" \
  cargo run --manifest-path "$ROOT/xpost-build/Cargo.toml" \
  --bin xpost-compact-elf --release --target "$HOST_TRIPLE" -- \
  --image "$TMP/image.bin" \
  --bpf-toml "$HERE/bpf.toml" \
  --name exec_bench \
  --slot-address "$SLOT_ADDR" \
  --helper-address "$HELPER_ADDR" \
  --max-image-len "$MAX_IMAGE" \
  -o "$OUT_DIR/exec_bench.elf"

echo "wrote $OUT_DIR/exec_bench.elf ($(wc -c < "$OUT_DIR/exec_bench.elf") bytes)"
