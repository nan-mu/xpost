# ESP32-C3 固件项目

基于 Rust 的 ESP32-C3 嵌入式固件，实现了 WiFi 连接、UDP 通信和动态代码加载功能。

## 项目概述

这是一个运行在 ESP32-C3 (RISC-V 架构) 上的嵌入式固件项目，主要功能包括：

- **WiFi Station 模式**：连接到指定的 WiFi 网络（DHCP）
- **AOT XDP**：WiFi RX 上运行 libbpf-mcu 编出的防火墙（按源 IP + 目的端口 DROP）
- **UDP :8080 → 固定 echo**：过 XDP 到达 userspace 的 UDP 不解析载荷，向 TR3000 发硬编码 echo（`0x42`）
- **动态代码加载**：从 `esp32c3.bin` 加载 AOT 到 `HOT_REGION`
- **Embassy 异步运行时**：使用现代异步编程模型

## 硬件要求

- ESP32-C3 开发板
- USB 数据线（用于烧录和调试）

## 软件依赖

### 必需工具

- Rust nightly 工具链
- `espflash` - ESP32 烧录工具
- `rust-src` 组件

### 安装步骤

```bash
# 安装 Rust (如果尚未安装)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 安装 nightly 工具链和必要组件
rustup toolchain install nightly
rustup component add rust-src --toolchain nightly
rustup target add riscv32imc-unknown-none-elf

# 安装 espflash
cargo install espflash
```

## 项目配置

用户态与 IRAM 布局意图写在 [`config.toml`](config.toml)（模板：[`config.example.toml`](config.example.toml)）。

- **改 `[arena]` / `[helper]` / `[slots]` 后必须** `cargo xtask doctor`，生成只读 [`mem.toml`](mem.toml)。`cargo build` **不会**改写 `mem.toml`；不一致则硬错误。
- **`[[tests]]`**：只保留当前要跑的项；不要堆 `enabled = false`。可省略（Phase A 布局-only）。
- **AOT**：`esp32c3.bin` / `bpf.toml` 仍手拷。

| 段 | 含义 |
|------|------|
| `[wifi]` / `[probe]` / `[dut]` | STA 与探针端口 |
| `[arena]` / `[helper]` | IRAM 窗口意图 |
| `[slots]` | 具名槽：`a = { length, mount }`；键名排序 = IRAM 顺序 |
| `[[tests]]` | 可选；启用项推导 `fw_us_*` |

**mount 规则**：单字符串 `call` \| `rx` \| `tx`；省略 ⇒ `call`。`call` 可多槽；`rx`/`tx` 全局各 ≤1。运行时 remount：停 datapath → 写锁改表 → enable（见 `patch::mount`）。

```toml
[slots]
a = { length = 0xE00, mount = "tx" }
b = { length = 0x1000, mount = "rx" }
```

拓扑约定：主路由只做 DHCP/网关；TR3000 做 AP + 探针。`[probe].ip` 必须是 TR3000（例 `192.168.110.142`），不要写成主路由 `…1`。

### 测试配方（历史）

`xtask` 已知名：`firewall_echo` / `xdp_forward_echo` / `hot_load_ctrl` / `fw_hash_test` / `tail_call_abc` / `tx_xdp_encap` / `hw_selftest`。多数网络测试互斥。与 router-probe `config/fwtest.toml` **同名启用**；改固件配方时必须同步关/开探针侧，否则会出现 `unknown op` / MapCrudAck 超时。

**`tx_xdp_encap`** — Cycle 0.5 Phase B：透明 `FirewallTxToken` 劫持 + HotLoad。推导：`tx_hijack` + `hot_load`（**不**开 map_crud / seeds / cold_boot）。验收约定：[`docs/evidence/cycle05_tx_hijack.md`](../docs/evidence/cycle05_tx_hijack.md)。AOT：`tx_udp_encap`（sport `0x4254`）/ 变体 B（`0x4255`）。

```toml
[[tests]]
name = "tx_xdp_encap"
```

**`tail_call_abc`** — Cycle 0：cold-boot（空 HOT_A、不建 bpf.toml maps）+ 控制面 CREATE/UPLOAD + `bpf_tail_call`。推导：`cold_boot` + `map_crud` + `hot_load` + `xdp_rx`。

```toml
[[tests]]
name = "tail_call_abc"
```

**`fw_hash_test`** — 启动加载 `fw_hash_bounce` AOT；seed `fw_rules`；MapCrud + HotLoad 全流程。与 `tail_call_abc` 互斥。

```toml
[[tests]]
name = "fw_hash_test"

[[tests.seeds]]
map = "fw_rules"
src_ip = "192.168.110.50"
action = 1
```

**`firewall_echo`** — 需 `firewall_ipv4` AOT；seed `firewall_rules` DROP。

```toml
[[tests]]
name = "firewall_echo"

[[tests.seeds]]
map = "firewall_rules"
src_ip = "10.0.0.1"
# dst_port 省略时默认 dut.listen_port
action = 1
```

**`xdp_forward_echo`** — VIP bounce + STA echo；seed `vip_ip`。

```toml
[[tests]]
name = "xdp_forward_echo"

[[tests.seeds]]
map = "vip_ip"
index = 0
ip = "192.168.110.200"
smoke = true
```

**`hot_load_ctrl`** — UDP `:8081` HotLoadOp SWAP（热区→DRAM 槽 1，载荷→HOT）。无 seeds。

```toml
[[tests]]
name = "hot_load_ctrl"
```

**`hw_selftest`** — 板载串口自检，跳过 WiFi；与其它网络测试互斥。

```toml
[[tests]]
name = "hw_selftest"
```

## 构建和烧录

改槽位/arena 后先：

```sh
cargo xtask doctor    # config.toml → mem.toml（校验 mount 唯一性等）
cargo xtask map       # 打印 IBUS/DBUS SRAM 图
cargo xtask check     # 校验 tomls + codegen（不持久写 OUT_DIR）
```

`cargo build` / `cargo check` 消费已有 `mem.toml`（须与 config 一致）。别名见 `.cargo/config.toml`。

`cargo run -p xtask …` 会因默认 riscv target 把 clap/std 当裸机编；请用 **`cargo xtask …`**。

固件烧录监控：`cargo fwrun`。`XTASK_PRINT_MAP=1 cargo build` 可在构建时打印 map。

### 构建项目

```bash
# 开发构建（自动跑 xtask codegen）
cargo build

# 发布构建（优化）
cargo build --release
```

### 烧录到设备

```bash
# 烧录并监控串口输出
cargo run --release

# 或使用 espflash 直接烧录
espflash flash --monitor --no-addresses --chip esp32c3 --partition-table partitions.csv target/riscv32imc-unknown-none-elf/release/firmware
```

## 项目结构

```
firmware/
├── src/                      # 固件源码（include! OUT_DIR 生成物）
├── xtask/                    # codegen 库 + 可选 CLI（build.rs 默认调用 lib）
├── .cargo/config.toml        # 目标平台、runner
├── build.rs                  # 薄封装：linker hint + xtask::generate + rustc-cfg/link-arg
├── config.toml / mem.toml / bpf.toml
├── partitions.csv
├── Cargo.toml                # workspace：firmware + xtask
└── rust-toolchain.toml
```

## 核心功能

### 1. WiFi Station 模式

固件启动后自动连接到配置的 WiFi 网络，使用静态 IP 地址配置。连接管理任务会自动处理断线重连。

### 2. UDP / 防火墙联调

- 监听端口 8080
- XDP（`firewall_ipv4`）按 `(src_ip, dst_port)` 查 map；`[[tests.seeds]]` 对 `firewall_rules` 写 DROP
- 到达 userspace 的任意 UDP（不读载荷）→ 向 `[probe].ip:[probe].reply_port` 发送固定载荷 `0x42`

### 3. 动态代码加载（AOT 热加载）

固件支持加载并执行 AOT 补丁：

- 代码加载到 IRAM 具名热槽（见 `config.toml` `[slots]` → `mem.toml`）
- 槽数 2..=4；mount `rx`/`tx`/`call` 决定入口角色（Phase B 起用于劫持）
- 执行前刷新指令缓存（`fence.i`）
- 辅助函数调度：`helper_dispatcher`（地址见 `mem.toml`）

### 4. 内存布局

- **`config.toml`**：`[arena]` / `[helper]` / `[slots]` 意图（人改）→ `cargo xtask doctor`
- **`mem.toml`**：doctor 生成（含 `name`/`mount`）；禁止手改
- xtask 再生成 `OUT_DIR/mem_map.rs`、`hot_slots.rs`、`custom-sections.x`
- AOT `.ld`：libbpf-mcu 按 `mem.toml` 拼出 `build/.ninja/esp32c3-slot-{N}.ld`
- 默认 2 槽示例：helper@`0x40380000`、`a`@`0x40380200`（tx）、`b`@`0x40381000`（rx）
- `load()` / `check_layout()` 启动时 `assert` 链接地址与 mem 一致；`mount::init` 加载 boot mounts

改槽：编辑 `[slots]` 后 `cargo xtask doctor`，再 `cargo build`。

## Flash 分区表

| 分区名称    | 类型 | 子类型 | 偏移量   | 大小     | 说明           |
|-------------|------|--------|----------|----------|----------------|
| nvs         | data | nvs    | 0x9000   | 0x6000   | 非易失性存储   |
| phy_init    | data | phy    | 0xf000   | 0x1000   | PHY 初始化数据 |
| factory     | app  | factory| 0x10000  | 0x2f0000 | 主应用程序     |
| func_ota    | 0x40 | 0x00   | -        | 0x80000  | OTA 功能区     |
| func_table  | 0x40 | 0x01   | -        | 0x2000   | 函数表         |
| abi_table   | 0x40 | 0x02   | -        | 0x2000   | ABI 表         |

## 编译优化

项目配置了针对嵌入式设备的优化选项：

- **开发模式**：`opt-level = "s"` (优化大小)
- **发布模式**：
  - LTO: `fat` (完整链接时优化)
  - 单编译单元：更好的优化
  - 调试信息：保留以便调试
  - 栈保护：启用 (`-Z stack-protector=all`)
  - 强制帧指针：便于回溯

## 日志和调试

项目使用 `log` 和 `esp-println` 进行日志输出：

```bash
# 设置日志级别（在 .cargo/config.toml 或运行时环境变量）
ESP_LOG="info"  # 可选: trace, debug, info, warn, error

# BPF helper 逐调用 trace（参数 + 返回值），需 debug 级别：
# 板载自检：config.toml 只留 [[tests]] name = "hw_selftest" 后重编
ESP_LOG=debug cargo run --release
```

`ESP_LOG=debug` 时，每次经 `helper_dispatcher` 的调用会输出一行 `DEBUG - bpf map_update_elem (#2) a1=… -> ok=0`（参数为十进制，避免 espflash 把 `0x…` 当地址做 addr2line）。

若单独用 `espflash monitor`，请加 `--no-addresses`，否则 monitor 仍可能对日志里的十六进制做符号解析并插入 `HOT_REGION` / `g_shared_buffers` 等行。

烧录后通过串口监控可以看到：

- WiFi 连接状态
- UDP 数据包接收和发送信息
- 动态代码加载过程
- 系统运行状态

## 安全特性

- 栈溢出保护
- 禁止使用 `mem::forget`（防止资源泄漏）
- 大栈帧检测（阈值 1024 字节）
- Clippy 严格检查

## CI/CD

项目配置了 GitHub Actions 自动化流程：

- 代码格式检查（`cargo fmt`）
- Clippy 静态分析
- 发布版本构建

## 开发建议

1. **修改代码后**：先运行 `cargo fmt` 和 `cargo clippy`
2. **测试网络功能**：使用 `nc` 或其他 UDP 工具发送测试数据
3. **调试**：通过串口监控查看日志输出
4. **性能优化**：注意栈使用，避免大栈帧

## 故障排除

### 编译错误

- 确保已安装 nightly 工具链和 `rust-src`
- 检查 `rust-toolchain.toml` 配置

### 烧录失败

- 确认 ESP32-C3 已正确连接
- 检查串口权限（Linux/macOS 可能需要 `sudo`）
- 尝试按住 BOOT 按钮重新烧录

### WiFi 连接失败

- 检查 SSID 和密码是否正确
- 确认 WiFi 网络支持 2.4GHz（ESP32-C3 不支持 5GHz）
- 检查 IP 地址配置是否与网络冲突

### UDP 通信问题

- 确认防火墙未阻止端口 8080
- 使用 `ping` 测试设备 IP 是否可达
- 检查网关配置是否正确

## 许可证

本项目使用的具体许可证请参考项目根目录的 LICENSE 文件。

## 贡献

欢迎提交 Issue 和 Pull Request！

## 相关资源

- [ESP32-C3 技术文档](https://www.espressif.com/en/products/socs/esp32-c3)
- [esp-hal 文档](https://docs.esp-rs.org/esp-hal/)
- [Embassy 异步框架](https://embassy.dev/)
- [Rust 嵌入式开发](https://docs.rust-embedded.org/)
