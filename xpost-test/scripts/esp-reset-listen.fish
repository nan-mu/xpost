#!/usr/bin/env fish
# Owns the USB-Serial-JTAG port: UART dump + TCP reset (DTR/RTS).
# Do NOT also run `espflash monitor` — that exclusive-opens the same device
# and `espflash reset` would kick it. This script replaces monitor.
#
#   fish test/scripts/esp-reset-listen.fish
#   ESPFLASH_PORT=/dev/cu.usbmodem21101 fish test/scripts/esp-reset-listen.fish

set -l here (dirname (status filename))
if not command -q python3
    echo "need python3" >&2
    exit 1
end
exec python3 $here/esp-reset-listen.py $argv
