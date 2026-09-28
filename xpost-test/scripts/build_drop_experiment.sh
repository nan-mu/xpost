#!/usr/bin/env bash
# 实验 1：构建丢包固件 AOT + 两套 profile 固件 + test 探针。
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
MEM="$ROOT/xpost/examples/esp32c3-drop-xdp/mem.toml"

echo "== libbpf-mcu: xdp_drop AOT =="
BS_FIRMWARE_MEM_MAP="$MEM" pixi run build bpf -i tests/xdp/xdp_drop.c
BS_FIRMWARE_MEM_MAP="$MEM" pixi run build esp32c3 -i tests/xdp/xdp_drop.c
cp "$ROOT/build/xdp_drop/esp32c3.bin" "$ROOT/test/test/assets/xdp_drop/"
cp "$ROOT/build/xdp_drop/esp32c3.bin" "$ROOT/xpost/examples/esp32c3-drop-xdp/"
# Refresh only `aot_addr` for maps present in the AOT ELF; keep wdt_feed + full schema.
python3 - "$ROOT" <<'PY'
import re, sys
from pathlib import Path

root = Path(sys.argv[1])
elf = root / "build/xdp_drop/esp32c3.elf"
bpf = root / "xpost/examples/esp32c3-drop-xdp/bpf.toml"
sys.path.insert(0, str(root / "libbpf-mcu/make"))
from obj_report import parse_maps_aot_addrs

addrs = parse_maps_aot_addrs("llvm-objdump-mp-22", elf)
text = bpf.read_text()
for name, addr in addrs.items():
    block = re.search(rf"(\[maps\.{re.escape(name)}\][\s\S]*?)(?=\n\[|\Z)", text)
    if not block:
        raise SystemExit(f"missing [maps.{name}] in {bpf}")
    part = block.group(1)
    new_part = re.sub(
        r"^aot_addr\s*=.*$",
        f"aot_addr = {addr:#x}",
        part,
        count=1,
        flags=re.M,
    )
    if new_part == part:
        raise SystemExit(f"[maps.{name}] missing aot_addr in {bpf}")
    text = text.replace(part, new_part, 1)
bpf.write_text(text)
PY
cp "$ROOT/xpost/examples/esp32c3-drop-xdp/bpf.toml" "$ROOT/test/test/assets/xdp_drop/bpf.toml"

echo "== xpost: drop-hal / drop-xdp firmware =="
export PATH="${HOME}/.cargo/bin:${PATH}"
for ex in esp32c3-drop-hal esp32c3-drop-xdp; do
  echo "-- $ex"
  (cd "$ROOT/xpost/examples/$ex" && RUSTUP_TOOLCHAIN=nightly cargo +nightly build --release)
done

echo "== test probe (aarch64 musl via docker) =="
"$ROOT/test/scripts/build_in_devcontainer.sh"

echo "OK. Flash: cd xpost && cargo xtask agent"
echo "TR3000: ./probe drop-hal --smoke --no-pg   # 或 --formal 写 formal schema"
