# TR3000 / ESP32-C3 防火墙联调（`xpost-test`）

在 **TR3000** 上用 **aya TC**（`bpf_ktime_get_ns`）打纳秒时间戳，并用 **AF_PACKET RAW** 向 ESP32-C3 STA 发送可伪造源 IP 的刺激包。配置在编译期嵌入 [`config/test.toml`](config/test.toml)。产物二进制名为 **`xpost-test`**。

- 默认：`./xpost-test` → `[default_bench]`（现为 `just-tx-pps`）：多轮经 Mac agent `FlashProfile` 烧录 HAL/XDP，写入 Postgres schema `formal` 的 `experiment_*`（开跑前只清本实验 `source_note`）。
- `./xpost-test suite` 跑 `[[tests]]`；`./xpost-test --help-benches` 看全部子命令。
- `./xpost-test serve` 供 `fwctl.sh` 远程提交。

人类表（`test-report`）：若 case 形如 `T0/a`，先打一行 `peers a=… b=… c=…`，再按 **STAGE × a|b|c** 汇总（列序固定字母序，裸 IP 不当主列）；否则回退旧的一行一 case。

## 拓扑

```
主路由 (DHCP/网关, e.g. .1)
        │
Mac Mini / TR3000 / ESP 同网段
        │
   TR3000 (.142, AP + test) ──2.4G── ESP32-C3 (STA, DHCP 自主流)
              │
       RAW 发包 + TC 打戳；echo 目的 = TR3000 本机 IP
```

- 本仓库产物下发到 **TR3000**（例 `root@192.168.110.142`），不要 ssh 到主路由。
- Mac Mini 负责烧录 ESP 固件；不参与发包。
- **正式 bench**（`drop-xdp` / `drop-hal` 等）：换实验 = **整包烧录固件 profile**（Mac `xpost-agent`），TR3000 `executor` 调度 `test`；**不在 round 内** HotLoad SWAP。
- **`./probe suite`** / test0：仍用 UDP 控制面（`:8081` HotLoad / MapCrud），与正式 bench 分离。
- 主路由只提供 DHCP；联调流量以 TR3000↔ESP 为准。
- DUT 是 ESP32-C3 上的防火墙 / XDP 运行时，不是 ImmortalWrt 本机防火墙。
- 固件 `FW_TR3000_IP` 须填 TR3000 LAN IP（`.142`），不是网关 `.1`。

## 协议契约（ESP 固件需对齐）

| 项 | 约定 |
|----|------|
| 刺激 | UDP `dst = ESP_IP` 或 `VIP_IP`，`dport=echo_port`（默认 8080） |
| 载荷 | `FWSTIM` + `u64` LE `seq`（探针打 T0；STA echo **原样回这段头** 以便 T1 按 seq 配对） |
| 防火墙维度 | **源 IPv4**（探针 RAW 伪造 `src_ip`；`firewall_echo`） |
| VIP 转发 | dst=VIP 时 XDP MAC swap + `XDP_TX`，载荷原样回流（`xdp_forward_echo`） |
| Echo | STA 路径：回 UDP `dst = TR3000:reply_port`，载荷 = 刺激的 `FWSTIM`+seq（非固定 `0x42`） |
| 时间戳 | TC egress 见刺激 → T0；ingress 见同 `FWSTIM`+seq 的 STA echo 或 VIP bounce → T1（均按 seq 配对） |

## `firewall_echo` 判定

| case | 行为 | 通过条件 |
|------|------|----------|
| `blocked` | 发应被拦的 `src_ip` | 有 T0，超时无 echo |
| `allowed` | 发应放行的 `src_ip` | 有 T0 + 同 seq 的 STA echo |

## `xdp_forward_echo` 判定

| case | 行为 | 通过条件 |
|------|------|----------|
| `vip_bounce` | RAW `dst=VIP`，`FWSTIM`+seq | Ingress 见同载荷 bounce；不得见 STA echo |
| `sta_echo` | RAW `dst=ESP_IP`，同载荷 | 见同 seq 的 STA echo；不得见 bounce |

`target.vip_ip` 须与固件 VIP seed 一致（仅 `xdp_forward_echo`）。

## `fw_hash_test` 判定

日志主键为 `[S0]`…`[S7]`。启动时打印 a/b/c/ctrl/dut；每阶段操作后按 a→b→c 输出回包（`E`/`B`/`D`）。

| 阶段 | 操作 | a | b | c |
|------|------|---|---|---|
| S0 | boot（bounce + map={b}） | E | B | E |
| S1 | MapCrud UPDATE c | E | B | B |
| S2 | MapCrud DELETE b | E | E | B |
| S3 | UPLOAD drop → 1号位 | E | E | B |
| S4 | COPY A→2号位; 1号位→A | E | E | D |
| S5 | MapCrud UPDATE b | E | D | D |
| S6 | COPY 2号位→A | E | B | B |
| S7 | READBACK 1/2 号位 | 长度对齐嵌入 AOT | | |

`b_ip` 来自 `[[tests.cases]].src_ip`（须与固件 `fw_rules` seed 一致）；`a_ip`/`c_ip`/`ctrl_src` 运行时同网段生成。schema：[`proto/map_crud_op.proto`](../proto/map_crud_op.proto)、[`proto/hot_load_op.proto`](../proto/hot_load_op.proto)。

## `hot_load_ctrl` 判定

| 步骤 | 行为 | 通过条件 |
|------|------|----------|
| S1 | UDP `:8081` `HotLoadOp` SWAP，`region=A`，`dram_slot=1`，payload=嵌入 DROP | ACK `status=0`，`loaded_len=len(DROP)` |
| S2 | 刺激 `dst=esp_ip` | 有 T0，超时无同 seq echo |
| S3 | 再 SWAP，payload=嵌入 PASS | ACK ok |
| S4 | 再刺激 | 有 T0 + 同 seq echo |

只热换 **HOT_A**（不测 B / `tail_call`）。默认 `enabled = false`。

### 嵌入 AOT（编译期）

`test/assets/hot_a/{bounce,drop,pass}/` 需已有 `esp32c3.bin` + `bpf.toml`（由他人/`pixi run build` 后手工放入；test **不**负责编译或拷贝）。

可选：改 `bpf.toml` 后跑 `cargo xtask gen-hot-meta`，刷新 [`test/src/generated/hot_a_bpf.rs`](test/src/generated/hot_a_bpf.rs) 里的 map/size 元数据。`build.rs` 只 `include_bytes!` 嵌入 bin。

## Mac agent + TR3000 `test serve`（正式 bench）

TR3000 只部署**单个** `test` 可执行文件；Mac 用脚本作客户端。

```
Mac scripts/fwctl.sh  --HTTP-->  TR3000 ./probe serve (:19081)
                                        |
                                        +-- re-exec ./probe <bench> (tee → 终端 + API log)
                                        |
                                        +-- FlashProfile --> Mac xpost-agent (:9240) --> ESP32-C3
```

| 组件 | 位置 | 启动 |
|------|------|------|
| `xpost-agent` | Mac（USB 接 ESP） | `cd xpost && cargo xtask agent`（或 `fwctl.sh` 自动拉起） |
| `test serve` | TR3000 | `./probe serve`（HTTP `[serve].http_listen`，默认 `0.0.0.0:19081`） |
| `test <bench>` | TR3000（serve 子进程或手工） | `./probe all\|drop-xdp\|… [--smoke] [--formal]` |

Mac 提交任务：`./scripts/fwctl.sh all --smoke --formal`（`TEST_HTTP` 指向 TR3000）。

Profile 注册表：[`xpost/profiles.toml`](../profiles.toml)（`firmware` / `drop-hal`）。MCU API：[`docs/test-experiments.md`](../../docs/test-experiments.md) §5。

`config/test.toml`：`[agent].endpoint`、`[serve].http_listen`、`[profiles]`。

HTTP API：`POST /api/jobs`、`GET /api/jobs/:id`、`GET /api/jobs/:id/log?offset=N`、`GET /healthz`。

## 正式实验 CLI

| 命令 | 主题 | 扫什么 | Postgres |
|------|------|--------|----------|
| `./probe drop-xdp\|drop-hal [--smoke] [--formal]` | 实验 1：丢包吞吐 | 满速 ICMP × 窗口 | `xdp_drop_*`（[`sql/xdp_drop.sql`](../sql/xdp_drop.sql)） |
| `./probe echo [--smoke] [--formal]` | XDP full-swap 回包 | 探针间隔网格 | `xdp_echo_*`（[`sql/xdp_echo.sql`](../sql/xdp_echo.sql)） |
| `./probe wl-rtt [--smoke] [--formal]` | 白名单 · 用户态 RTT | 仅 α（含 0）；间隔固定 2200µs | `xdp_ddos_*` |
| `./probe wl-loss [--smoke] [--formal]` | 白名单 · 字节丢包 | 仅 α（含 0）；事后 collect→dump | `xdp_loss_*` |
| `./probe rate-rtt [--smoke] [--formal]` | 速率黑名单 · RTT | peak% × mit | `xdp_rate_rtt_*`（[`sql/xdp_rate_rtt.sql`](../sql/xdp_rate_rtt.sql)） |
| `./probe rate-loss [--smoke] [--formal]` | 速率黑名单 · 丢包 | peak% × mit；事后 collect→dump | `xdp_rate_loss_*`（[`sql/xdp_rate_loss.sql`](../sql/xdp_rate_loss.sql)） |

| `./probe just-drop [--formal] [--no-pg]` | just_drop ACK：clear vs drop | 两次刺激 | `xdp_ack_*` |
| `./xpost-test` / `just-drop-pps` | just_drop / just_drop_hal **PPS** | agent flash + 周期读 `drop` × UDP blast | `formal.experiment_*`（[`../benchmark/just_drop.ipynb`](../benchmark/just_drop.ipynb)） |
| `./xpost-test`（`[default_bench]=just-tx-pps`） | just_tx / just_tx_hal **PPS** | agent flash + 周期读 `rxcnt 17` × UDP blast | `formal.experiment_*` `source_note=xpost-test just-tx-pps`（[`../benchmark/just_tx.ipynb`](../benchmark/just_tx.ipynb)） |

- **`--formal`**：仍连 `dbname=postgres`，写入 **schema `formal`**，开跑前 `TRUNCATE` **本命令**表族。默认写 `public`。
- 别名：`bench` → `rate-rtt`；`bench-loss` → `wl-loss`。无子命令时打印帮助（不再默认跑 rate）。
- 对外称「速率黑名单」，不称 Tolay。旧表 `xdp_tolay_*` 不再写入。
- Loss 一律事后 dump，不用流中 echo 算 loss%。
- Drop：`assets/xdp_drop/`（`bpf.toml` 含 `rxcnt`；AOT 与 `examples/drop_xdp` 同源）。

```sh
# TR3000（常驻，只上传 test 一个文件）
./probe serve

# Mac 远程提交（自动 ensure agent + 流式拉日志）
TEST_HTTP=http://<tr3000-ip>:19081 ./scripts/fwctl.sh all --smoke --formal

# 或 TR3000 本地直接跑
./probe all --smoke --formal
./probe drop-hal --smoke --formal
./probe echo --smoke --formal
./probe wl-rtt --smoke --formal
./probe wl-loss --smoke --formal
./probe rate-rtt --smoke --formal
./probe rate-loss --smoke --formal
./probe suite
```

## `test rate-rtt` / `rate-loss`（速率黑名单）

AOT：`assets/ddos_rate_bl/`。探针/合法源写入 **`ctrl_wl`**。网格：`peak% ∈ {0,25,50,75,100}` × `mit` 开/关。

- **rate-rtt**：攻击中低频 `FWSTIM` → U-echo RTT → `xdp_rate_rtt_*`。
- **rate-loss**：合法流 `FWCOL1` collect → 停攻 → dump → 字节 loss% → `xdp_rate_loss_*`。

```sh
./probe rate-rtt --smoke --formal
./probe rate-loss --smoke --formal
./probe bench              # 别名 → rate-rtt
```

`P_peak` / `thresh` / 网格 / `CELL_REPEATS` 写死在 `bench/rate_common`；不要在 CLI 调参。

## `test drop-xdp` / `drop-hal`（实验 1 丢包吞吐）

见 [`docs/test-experiments.md`](../docs/test-experiments.md) §4.1–4.2 与 [`sql/xdp_drop.sql`](../sql/xdp_drop.sql)。

```sh
./probe drop-xdp --smoke --formal
./probe drop-hal --smoke --formal
```

## `test wl-loss`（白名单丢包；合法流间隔固定 2.2 ms）

Probe **只发合法流**：预生成固定随机字节流（`FWCOL1` + index + byte），MCU 写入收集缓冲；结束经 **`:8081`** 发 `FWDMP1` 拉回缓冲（XDP 恒 PASS，避免 dump 被 datapath 饿死），按字节比对算丢包率。干扰用开源 **`hping3`**。只扫 **α**（含 0 无攻基准），**不**扫发包间隔。

写入 `xdp_loss_config` / `xdp_loss_data`（见 [`sql/xdp_loss.sql`](../sql/xdp_loss.sql)）。

| 条件 | XDP |
|------|-----|
| UDP `:8081` | PASS（控制面：HotLoad / MapCrud / dump / rst） |
| src ∈ `ddos_wl` | PASS → `:8080` collect |
| miss | DROP |

```sh
./probe wl-loss --smoke          # α=0 + α=100，短窗
./probe wl-loss --formal         # 正式：α 网格，写 schema formal
./probe bench-loss               # 兼容别名 → wl-loss
```

## `test wl-rtt`（白名单攻中用户态 RTT）

同 `ddos_wl`；合法源按 2.2 ms 发 `FWSTIM`，边攻边测 U-echo RTT。α 网格含 0。写入 `xdp_ddos_*`。

```sh
./probe wl-rtt --smoke --formal
```

## 路由器依赖（opkg）

```sh
opkg update
opkg install kmod-sched-bpf kmod-sched-core ip-full
```

确认 iface（常见 `br-lan`）：

```sh
ip link
```

## 配置（编译期）

编辑 [`config/test.toml`](config/test.toml)：`probe.iface`、`target.esp_ip`、`target.esp_mac`、以及要跑的 `[[tests]]` / `cases`（`enabled = false` 可跳过）。改完后重新编译再 scp。

## 编译

容器 `quirky_villani` 挂载 `/Users/nan/bs`。`rust-toolchain.toml` 使用绝对 path
toolchain [`.cargo/docker-toolchain`](.cargo/docker-toolchain)（rustup **不支持**相对 path）：
在本目录敲 `cargo …` 即 `docker exec` 进容器。`.cargo/config.toml` **不能**把整个
cargo 进程挪进 Docker（只有 `rustc-wrapper`，build.rs 仍在宿主机），所以用 path toolchain。

```sh
cargo build -p test --release
# → target/aarch64-unknown-linux-musl/release/probe
```

容器未启动时先：`docker start quirky_villani`。

## 手动部署与运行

在 Mac 上把二进制拷到 TR3000 后：

```sh
scp target/aarch64-unknown-linux-musl/release/probe root@192.168.110.142:/tmp/test
ssh root@192.168.110.142
chmod +x /tmp/test
/tmp/test
# 退出码 0 = 全部通过；JSON 写入 probe.report_path 并打印到 stdout
```

看报告：

```sh
cat /tmp/test-report.json
```

## 精度说明

- Delta 只用 TC 的 `bpf_ktime_get_ns`，不用 wall clock。
- 对比不同 ESP 防火墙实现时，保持同一 `iface`、同一空口环境，只换固件。
- 当前测的是 TR3000 观测到的往返（含 WiFi），用于相对比较，不是片内周期计数。

## 扩展测试

1. 在 `test/src/tests/` 新增实现 `ProbeTest` 的类型。
2. 在 `tests/mod.rs` 的 `registry()` 注册。
3. 在 `config/test.toml` 增加 `[[tests]]` 与 cases，然后重编。
