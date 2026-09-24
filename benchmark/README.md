# just_drop vs just_drop_hal PPS 基准

对比 **XDP ingress drop**（`examples/just_drop` + cold `just_drop.elf`）与 **HAL 应用层 drop**（`examples/just_drop_hal` + 串口 `drop on`）在同等 UDP 洪泛下的 DUT 可观测丢包吞吐。

主指标是 **pps**：在 blast 窗口内周期性读取串口 `OK counters … drop=`，用差分 / 墙钟时长得到吞吐。

## 拓扑

```
TR3000 (AF_PACKET RawTx, udp_blast ×4, 64B)
    │  UDP → ESP:8080
    │  TCP RPC → Mac:9240
    ▼
Mac xpost-agent (USB 串口独占)
    │  counters / program / drop on
    ▼
ESP32-C3 STA (just_drop 或 just_drop_hal)
```

配置嵌入 [`xpost-test/config/test.toml`](../xpost-test/config/test.toml)：`[agent].endpoint`、`target.esp_ip` / `esp_mac`、`probe.iface`。

## 指标定义

| 符号 | 定义 |
|------|------|
| `overall_pps` | `(drop_end − drop_start) / blast.elapsed_secs` |
| `interval_pps` | 相邻采样点 `(drop_i − drop_{i−1}) / (t_i − t_{i−1})` |
| `host_pps` | TR3000 发送计数 / blast 时长（无线/CPU 上限诊断） |

默认：窗口 **40s**（`--smoke` → **10s**）、采样间隔 **1000ms**、轮次 **3**（smoke → **1**）。

**路径差异（刻意）**：XDP 在 Wi-Fi RX ingress 丢包，`net_rx` 应不变；HAL 包已进入 smoltcp，只在 UDP:8080 记 drop，不强制 `net_rx` 平坦。对比的是同刺激下 **drop 计数器吞吐**，不是同层成本。

## 命令

### Mac

```bash
# HAL 臂
cargo xtask just_drop_hal build --release
espflash flash --chip esp32c3 --port /dev/cu.usbmodem* \
  examples/just_drop_hal/target/riscv32imc-unknown-none-elf/release/just_drop_hal
cargo xtask agent --listen 192.168.110.96:9240 --no-monitor

# XDP 臂（program 可由 probe 触发）
cargo xtask just_drop build --release
espflash flash --chip esp32c3 --port /dev/cu.usbmodem* \
  examples/just_drop/target/riscv32imc-unknown-none-elf/release/just_drop
# agent 同上；ELF: examples/just_drop/just_drop.elf
```

### TR3000

```bash
# 构建并 scp probe（见 xpost-test/README.md）
./probe just-drop-pps --path hal --out /tmp/hal_pps.csv
./probe just-drop-pps --path xdp --out /tmp/xdp_pps.csv

# smoke
./probe just-drop-pps --path hal --smoke --out /tmp/hal_smoke.csv
./probe just-drop-pps --path xdp --smoke --no-program --out /tmp/xdp_smoke.csv
```

可选：`--window-secs N`、`--sample-interval-ms 1000`、`--rounds 3`、`--no-program`（XDP 已 lease 时跳过 program）。

产物：CSV 时间序列 + 同目录 `*_summary.json`。

### 绘图

```bash
# 依赖: pip install matplotlib
python3 benchmark/scripts/plot_pps.py \
  --hal benchmark/results/hal_pps.csv \
  --xdp benchmark/results/xdp_pps.csv \
  --outdir benchmark/results
```

生成 `pps_timeseries.png`、`pps_compare_bar.png`。实验记录见 [`just_drop_pps.md`](just_drop_pps.md)。
