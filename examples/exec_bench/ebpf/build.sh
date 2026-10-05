#!/usr/bin/env bash
# Build exec_bench.elf (Compact ELF V0) from exec_bench.c.
# Requires: clang (riscv32), ld.lld, cargo (xpost-compact-elf via xpost-build).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
HERE="$(cd "$(dirname "$0")" && pwd)"
OUT_DIR="${HERE}/../"
# Profile / cold-table name (must match [[xdp]].name). Same image for all workloads.
XDP_NAME="${1:-exec_bench}"
SLOT_ADDR=0x40380200
HELPER_ADDR=0x40380000
MAX_IMAGE=0xE00

# Apple clang has no riscv32 target; Homebrew llvm does.
LLVM_BIN=""
if [[ -x /opt/homebrew/opt/llvm/bin/clang ]]; then
  LLVM_BIN=/opt/homebrew/opt/llvm/bin
  CLANG="$LLVM_BIN/clang"
elif command -v clang >/dev/null 2>&1; then
  CLANG=clang
else
  echo "build.sh: no clang found" >&2
  exit 1
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# -fno-jump-tables: Compact ELF packs .text only; a switch jump table in
# .rodata would sit past the image and `jr` to garbage (instruction fault).
"$CLANG" --target=riscv32 -march=rv32imc -mabi=ilp32 -O2 \
  -ffreestanding -fno-builtin -nostdlib -fno-stack-protector \
  -fno-jump-tables \
  -c -o "$TMP/exec_bench.o" "$HERE/exec_bench.c"

# Prefer Homebrew ld.lld when linking with Homebrew clang.
LLD_FLAG=(-fuse-ld=lld)
if [[ -n "$LLVM_BIN" && -x "$LLVM_BIN/ld.lld" ]]; then
  LLD_FLAG=(-fuse-ld="$LLVM_BIN/ld.lld")
fi

"$CLANG" --target=riscv32 -march=rv32imc -mabi=ilp32 -O2 \
  -ffreestanding -fno-builtin -nostdlib \
  "${LLD_FLAG[@]}" \
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

# Host binary — must not inherit examples/exec_bench/.cargo (build-std) or a
# shared target dir mixed with riscv32 artifacts. Run cargo from xpost-build/.
HOST_TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
HOST_TARGET_DIR="$ROOT/xpost-build/target-host-compact"
HOST_BIN="$HOST_TARGET_DIR/$HOST_TRIPLE/release/xpost-compact-elf"
if [[ ! -x "$HOST_BIN" ]]; then
  (
    cd "$ROOT/xpost-build"
    CARGO_TARGET_DIR="$HOST_TARGET_DIR" \
      cargo build --release --bin xpost-compact-elf --target "$HOST_TRIPLE"
  )
fi
"$HOST_BIN" \
  --image "$TMP/image.bin" \
  --bpf-toml "$HERE/bpf.toml" \
  --name "$XDP_NAME" \
  --slot-address "$SLOT_ADDR" \
  --helper-address "$HELPER_ADDR" \
  --max-image-len "$MAX_IMAGE" \
  -o "$OUT_DIR/${XDP_NAME}.elf"

echo "wrote $OUT_DIR/${XDP_NAME}.elf ($(wc -c < "$OUT_DIR/${XDP_NAME}.elf") bytes)"
