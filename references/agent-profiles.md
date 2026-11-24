# XPOST Agent 配置与产物选择

以下“现状”对应 XPOST #23 的 b5a44072。迁移后的配置示意描述字段含义，提交 PR 时在此位置补上实现实际采用的 TOML 和运行示例。

## 当前如何定位配置

入口是仓库根的 `profiles.toml`（复数）。`xpost-agent/src/state.rs` 优先取 `XPOST_ROOT`，否则从当前目录向上查找含 profiles.toml 的根。默认加载根下的该文件；`XPOST_PROFILES` 可覆盖配置文件路径。构建目录、分区表、固件文件仍按工作区根解释。启动参数 `--bind` 选择 HTTP/MCP 监听地址，`--log-dir` 默认 target/xpost-agent。这些设置与生产端 release 选择相互独立。

| 当前配置 | 含义及调用方 | 迁移要求 |
|---|---|---|
| 顶层 build_root = "../build" | 有 XDP profile 时解析器要求非空，ops::resolve_xdp 用它查旧生产索引 | 换为固定的生产端 release 来源与 tag（正式 nan-mu/libbpf-mcu v0.1.1）；Agent 自行取包及缓存。保留明确的配置来源，避免自动漂移到 latest |
| [[fw]].name、target、partition_table、artifact | 固件 profile 名、芯片、分区表和实际链接固件 ELF。当前支持 esp32c3 | 继续从真实固件取布局和身份；这里的 artifact 是固件路径，删除独立 crate 时保留此字段语义 |
| fw.xdp_image_capacity（#25） | 固件 profile 对单次冷装写入的容量参数。exec 载体冷单元为 16 KiB | 以当前目标固件与分区为准传给串口，和 SRAM 镜像容量分开校验 |
| fw.build.cwd、fw.build.command | Agent 构建固件的工作目录和参数列表 | 继续用于固件；XDP 由 release 提供 |
| fw.xdp | 随固件冷安装的 XDP profile 名列表；HAL 示例为空 | co-flash 使用本次实际构建的固件。预先准备完所有列表成员再开始设备写入 |
| [[xdp]].name、fw | Agent 调用名与所属固件。当前允许相同分区表的其他固件引用此项 | 保留可用调用名；同分区表不证明两份固件的 helper 和槽布局相同 |
| xdp.program、mode、可选 target | program 是上游 stem；mode 为 aot/jit/rbpf；显式 target 须匹配 fw | 只在 Agent 边界把 mode 映射到上游索引。不要求每个 profile 重填归档路径、容器文件名、槽地址或 helper 地址 |

## 用一个实际 profile 读懂命名

迁移后，just-tx-metrics 指向 examples/just_xdp 的低开销采集固件，fw.xdp 包含 just_tx。XDP profile 示例与归档映射如下。

```toml
[[xdp]]
name = "just_tx"
fw = "just-tx-metrics"
program = "just_tx"
mode = "aot"
```

选择顺序为固件与 XDP profile → program 对应 release 附件 → 包根 v3 index.toml → 对应部署 ELF → 按本次固件和目标槽准备装载字节。

| Agent 模式 | v3 mode.name | just_tx.7z 中的部署文件 |
|---|---|---|
| aot | llvm-aot | containers/just_tx-vm-aot.elf |
| jit | jit | containers/just_tx-vm-jit.elf |
| rbpf | ebpf | containers/just_tx-vm-ebpf.elf |
| HAL | 无 | 选择静态固件，零个 XDP 容器 |

case_mark_idle 是本地 profile，program 为 case_mark，应从 case_mark.7z 取件。原始附件、成员路径和索引名称保持生产端拼写。分别核对调用名、容器内部身份和程序标识，避免以本地别名冒充上游程序，或要求生产端为每个别名重发附件。exec 当前配置仍有 incr、square、bitswap 等旧项；按七个 EXB2 程序迁移实际调用。核对各固件实际支持的执行模式，不能仅由 profile 名推断。

## 采集命名迁移

| 当前正式请求中的名称 | 迁移后的名称 |
|---|---|
| `just-tx-cag`、`just-drop-cag`、`firewall-cag` | `just-tx-metrics`、`just-drop-metrics`、`firewall-metrics` |
| `just-tx-t`、`just-drop-t`、`firewall-t` | `just-tx-trace`、`just-drop-trace`、`firewall-trace` |
| `just-tx-cag-lookup`、`firewall-cag-lookup` | `just-tx-metrics-lookup`、`firewall-metrics-lookup` |
| `perf-cag`、`perf-t`（若仍作为正式构建选项） | `perf-metrics`、`perf-trace` |

`case-cag` 当前没有启用 `perf-cag`，去掉该旧后缀。用 `case-xdp` 称呼共用固件。如果新实现确实需要分别构建采集变体，可按实际用途命名为 `case-metrics` 和 `case-trace`。如果执行模式需要不同固件，保留各自身份。固件复用以实际摘要为准，单改名字不要求重新烧录。例程通过配置与自描述数据表明实际采集项，采集名不锁定 Counter、Aggregate、Gauge 的组合。

执行模式继续使用 `hal/aot/jit/rbpf`。中量的新 DB 写入使用 `metrics/trace`。历史配置如需兼容，按已定规则把 `stream` 读为 `metrics`，历史数据仍按原字段查询。libbpf-mcu 的包名、程序名、容器路径和索引模式名沿用正式发布契约。

## 验证入口

HTTP 的 POST /v1/flash/fw 接受 name；POST /v1/flash/xdp 的 name 指定 XDP profile。后者省略 slot 表示冷安装，指定 slot 是板上已安装镜像的热路径。MCP 复用同一操作层。分别演示固件随装、独立冷安装和预装后切换；给出实际配置、release 身份、包内路径、原始与最终镜像摘要和目标槽。独立冷安装使用现存链接固件并核对设备，不隐式重编译。xpost-test 只选择实验和调用 Agent，不需要认识 7z 布局。

实现依据：[profiles.toml](https://github.com/nan-mu/xpost/blob/b5a44072ed57657c7482758412cccf1fd93df6fa/profiles.toml)、[注册与交叉校验](https://github.com/nan-mu/xpost/blob/b5a44072ed57657c7482758412cccf1fd93df6fa/xpost-agent/src/profiles.rs)、[配置发现](https://github.com/nan-mu/xpost/blob/b5a44072ed57657c7482758412cccf1fd93df6fa/xpost-agent/src/state.rs)、[操作入口](https://github.com/nan-mu/xpost/blob/b5a44072ed57657c7482758412cccf1fd93df6fa/xpost-agent/src/main.rs)。
