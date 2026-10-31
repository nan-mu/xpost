# xpost

xpost 是裸机 MCU 上的 XDP 运行时，包含板级示例和主机工具。固件使用 Rust `core` 和 esp-rs 裸机栈，不使用 ESP-IDF。已实现的目标是 ESP32-C3，ISA 为 RV32IMC，ABI 为 `ilp32`，没有 FPU。

XDP 程序由 libbpf-mcu 从同一份 C 分别编译出 eBPF 对象和 RV32IMC 的 XDP ELF。安装到板上的是 RV32IMC 镜像。运行时按链接脚本里的绝对地址调用入口，不在设备上解析固件 ELF，也不做动态重定位。

论文 coming soon。

许可证是 [MIT](LICENSE)。

xpost is an XDP runtime for bare-metal MCU firmware, with board examples and host tools. Firmware uses Rust `core` and the esp-rs bare-metal stack. It does not use ESP-IDF. The implemented target is the ESP32-C3, the ISA is RV32IMC, the ABI is `ilp32`, and there is no FPU.

libbpf-mcu compiles one C source into an eBPF object and an RV32IMC XDP ELF. The image installed on the board is the RV32IMC image. The runtime calls the entry at the absolute address in the linker script. It does not parse a firmware ELF on the device, and it does not apply dynamic relocations.

Paper: coming soon.

The license is [MIT](LICENSE).

## 现状

代码还在改。这次按现状发布，不把实验室配置改成通用部署。`xpost-test/rust-toolchain.toml` 写着本机绝对路径 `/Users/nan/bs/...`。`xpost-test/config/default.toml` 写着实验室地址和数据库设置。脚本里的 SSH 主机名是 `tr3000`。这些以后再删。换一台机器运行，需要自己读代码，再改这些位置。下文的构建步骤是当前机器上的用法，不是一份移植说明。

## Status

The code is still changing. This publication keeps the tree as it is. `xpost-test/rust-toolchain.toml` contains the absolute path `/Users/nan/bs/...`. `xpost-test/config/default.toml` contains lab addresses and database settings. Scripts use the SSH host name `tr3000`. Those will be removed later. Running this on another machine means reading the code and editing those spots. The build steps below describe the current machine. They are not a porting guide.

## 名称

三份文件不要混用。

固件 ELF 是板级程序，路径通常是 `examples/<app>/target/riscv32imc-unknown-none-elf/release/<app>`。带 XDP 的固件包含 `XdpRt`。HAL 固件没有这套运行时。

XDP ELF 也写作 `xdp.elf`。它是冷区容器，带 `.xpost.meta` 和 `.xpost.image`。路径通常是 `../build/<stem>/<stem>.elf`。冷安装使用这一份。

`esp32c3.elf` 是 libbpf-mcu 的 AOT 调试 ELF，路径通常是 `../build/<stem>/esp32c3.elf`。不要用它做冷安装。

Keep the three files distinct.

The firmware ELF is the board application. Its usual path is `examples/<app>/target/riscv32imc-unknown-none-elf/release/<app>`. XDP firmware includes `XdpRt`. HAL firmware does not include this runtime.

The XDP ELF, also written `xdp.elf`, is the cold-region container. It has `.xpost.meta` and `.xpost.image`. Its usual path is `../build/<stem>/<stem>.elf`. Cold installation uses this file.

`esp32c3.elf` is the libbpf-mcu AOT debug ELF. Its usual path is `../build/<stem>/esp32c3.elf`. Do not use it for cold installation.

## 工具链

`rust-toolchain.toml` 使用 nightly，并包含目标 `riscv32imc-unknown-none-elf`。根包的 `rust-version` 是 1.88。

根 Cargo 工作区成员是根包、`esp-radio`、`xpost-macros`、`xpost-elf` 和 `xpost-type`。`xpost-build`、`xpost-agent`、`xpost-serial`、`xpost-test` 和 `examples/` 各自有工作区。

`rust-toolchain.toml` selects nightly and includes the `riscv32imc-unknown-none-elf` target. The root package sets `rust-version` to 1.88.

The root Cargo workspace contains the root package, `esp-radio`, `xpost-macros`, `xpost-elf`, and `xpost-type`. `xpost-build`, `xpost-agent`, `xpost-serial`, `xpost-test`, and `examples/` each have their own workspace.

## 构建固件

在示例目录里构建。网络示例还要在该目录的 `.env` 里提供 `WIFI_SSID` 和 `WIFI_PASS`。`WIFI_PASSWORD` 是 `WIFI_PASS` 的别名。

```bash
cargo build --release --features perf-cag
```

上面的命令针对 `examples/just_xdp`。`perf-cag` 打开 counter、aggregate 和 gauge。`perf-t` 打开 counter 和 trace。没有默认 feature。

`examples/exec_bench` 通过函数调用运行 XDP 镜像，不初始化 Wi-Fi。`examples/exec_bench_hal` 是不带 xpost 运行时的原生对照。HAL 网络示例是 `examples/just_tx_hal`、`examples/just_drop_hal` 和 `examples/firewall_hal`。

Build from the example directory. A network example also needs `WIFI_SSID` and `WIFI_PASS` in that directory's `.env`. `WIFI_PASSWORD` is an alias for `WIFI_PASS`.

The command above is for `examples/just_xdp`. `perf-cag` enables the counter, the aggregate, and the gauge. `perf-t` enables the counter and trace. There is no default feature.

`examples/exec_bench` runs an XDP image through a function call and does not start Wi-Fi. `examples/exec_bench_hal` is the native comparison without the xpost runtime. The HAL network examples are `examples/just_tx_hal`, `examples/just_drop_hal`, and `examples/firewall_hal`.

## 主机代理

`xpost-agent` 占用 USB 串口。它在同一端口提供 HTTP `/health`、`/v1/*` 和 MCP `/mcp`。进程启动时只打开 USB 会话。烧录和切换应用发生在请求到达之后。

在 `xpost-agent/` 里运行：

```bash
cargo run --release -- --bind 127.0.0.1:9240
```

`xpost-agent` owns the USB serial port. It serves HTTP `/health`, `/v1/*`, and MCP `/mcp` on one port. Startup only opens the USB session. Flashing and application changes happen when a request arrives.

Run the command from `xpost-agent/`.

## 目录

```text
src/             runtime: maps, helpers, slots, RX, optional exec-call
esp-radio/       local radio driver and XDP ingress
xpost-type/      shared XDP actions and traits
xpost-elf/       XDP ELF codec and the XPM1 memory-map contract
xpost-macros/    map derives and flash_layout!
examples/        board firmware, one Cargo workspace each
xpost-serial/    serial protocol library
xpost-agent/     host daemon
xpost-build/     xpost-link and the XDP ELF packer
xpost-test/      host experiment runner
```

`src/` 是运行时，包含 map、helper、槽位、RX，以及可选的 `exec-call`。`esp-radio/` 是本地无线驱动和 XDP 入口。`xpost-type/` 是共用的 XDP 动作和 trait。`xpost-elf/` 负责 XDP ELF 编解码和 XPM1 内存图。`xpost-macros/` 提供 map 派生和 `flash_layout!`。`examples/` 里每个固件是单独的 Cargo 工作区。`xpost-serial/` 是串口协议库。`xpost-agent/` 是主机守护进程。`xpost-build/` 提供 `xpost-link` 和 XDP ELF 打包。`xpost-test/` 是主机上的实验运行器。

当前代码使用 `xpost-type`。`xpost-trait/` 是旧接口，工作区成员不引用它。

The current code uses `xpost-type`. `xpost-trait/` is an older interface. Workspace members do not reference it.
