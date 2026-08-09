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

联调相关变量写在 `.env`（可参考 `.env.example`）或 `.cargo/config.toml` 的 `[env]`：

| 变量 | 含义 |
|------|------|
| `WIFI_SSID` / `WIFI_PASSWORD` | STA 凭据 |
| `XDP_ENABLE` | `1`/`true` 启用 RX 上 AOT XDP |
| `FW_BLOCKED_SRC_IP` | seed 进 `firewall_rules` 的 DROP 源 IP（对齐探针 `blocked`） |
| `FW_TR3000_IP` | 固定 echo 目的 IP（**TR3000 本机 LAN IP**，不是主路由/网关） |
| `FW_ECHO_PORT` | 固定 echo 目的端口（默认 `18080`） |

拓扑约定：主路由只做 DHCP/网关；TR3000 做 AP + 探针。`FW_TR3000_IP` 必须是 TR3000（例 `192.168.110.142`），不要写成主路由 `…1`。

监听端口固定为 `8080`。探针伪造源 IP 发包；未 seed 的源过 XDP 后触发 echo。

### 网络参数（代码常量）

```rust
const FW_LISTEN_PORT: u16 = 8080;  // UDP 监听端口
```

## 构建和烧录

### 构建项目

```bash
# 开发构建
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
├── src/
│   ├── bin/
│   │   └── main.rs           # 主程序入口
│   ├── patch/
│   │   ├── mod.rs            # 补丁模块导出
│   │   ├── part.rs           # 动态代码加载实现
│   │   └── helper.rs         # 辅助函数调度器
│   ├── connection.rs         # WiFi 连接管理
│   ├── net_task.rs           # 网络任务
│   ├── sta.rs                # WiFi Station 初始化和 UDP 处理
│   └── lib.rs                # 库入口
├── .cargo/
│   └── config.toml           # Cargo 配置（目标平台、运行器）
├── .github/
│   └── workflows/
│       └── rust_ci.yml       # CI/CD 配置
├── build.rs                  # 构建脚本
├── custom-sections.x         # 自定义链接器段定义
├── partitions.csv            # Flash 分区表
├── payload.bin               # 动态加载的代码载荷
├── rust-toolchain.toml       # Rust 工具链配置
├── Cargo.toml                # 项目依赖配置
└── .clippy.toml              # Clippy 配置
```

## 核心功能

### 1. WiFi Station 模式

固件启动后自动连接到配置的 WiFi 网络，使用静态 IP 地址配置。连接管理任务会自动处理断线重连。

### 2. UDP / 防火墙联调

- 监听端口 8080
- XDP（`firewall_ipv4`）按 `(src_ip, dst_port)` 查 map；seed 对 `FW_BLOCKED_SRC_IP:8080` 写 DROP
- 到达 userspace 的任意 UDP（不读载荷）→ 向 `FW_TR3000_IP:FW_ECHO_PORT` 发送固定载荷 `0x42`

### 3. 动态代码加载（AOT 热加载）

固件支持从 Flash 的 `payload.bin` 加载并执行外部代码：

- 代码加载到 IRAM 的特定区域（`0x403A1000`，与 AOT `HOT_REGION` 一致）
- 支持 BSS 段初始化
- 执行前刷新指令缓存（`fence.i`）
- 提供辅助函数调度机制（`helper_jump`）

### 4. 内存布局

自定义链接器脚本定义了特殊的内存段：

- `mem_map.toml`: 固件侧地址契约（生成 `OUT_DIR/mem_map.rs` + `custom-sections.x`）
- AOT `esp32c3.ld` 仅由 **libbpf-mcu** 维护，固件构建不会改写
- `0x40380000`: `helper_dispatcher`（4KB，DRAM dummy 防双映射清零）
- `0x40381000`: `HOT_REGION` 占位（4KB）
- `load()` 启动时 `assert` 链接地址与 `mem_map.toml` 一致

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
ESP_LOG=debug cargo run --release --features hw-selftest
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
