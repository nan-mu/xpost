# xpost-serial 会话恢复记录

2026-10-03，仅恢复 `xpost-serial`。基线提交：`c0ac045`。

依据 `~/bs/.recovery/session_catalog.md`、`mutations.jsonl` 和它们索引的原始会话：

- `7e273dda-2f74-45fc-a97d-c6355e3aaa4b`：确定 `type/client/server`、互斥 features、新 map/perf 协议、文件作为日志权威来源；淘汰旧命令和顶层模块。
- `42fb8f7c-4133-4849-b496-d67a2f02684a`：实施重构；07:25 要求命令枚举放 `type`、`client/command.rs` 只放 impl、采用 Tokio/futures、统一共享常量、暂时删除测试。
- `d0656fdb-f4ce-4c53-9a94-3e88252e932e`：分区表采用 `esp_idf_part::PartitionTable`，XDP 区间从 CSV 读取，改名 `XdpTable`；10:28 改为共享 `client/port::Port`、按操作打开/关闭、同步原子 App 请求；10:37 去掉 reader 线程，`Ok(0)` 立即报 EOF。

后一轮同步串口约定覆盖早期“三通道串口线程”计划。Flash 仍使用 Tokio task；生命周期只记录日志，不向 App UART 发送，也不恢复旧握手通道。

按顺序重放两次实现会话的 Write/StrReplace，22 个现行文件与已有恢复结果一致。本次补回自动恢复遗漏的 shell 删除：

- 删除旧 `session/sink/event/table/partitions/perf_report/error` 顶层模块。
- 删除残留的 log/perf 单元测试；没有向库中增加测试。
- 清除目标库内 `*.pre_session_restore` 旧副本；原始内容仍可从基线提交取回。
- 更新 `Cargo.lock`，补齐 Tokio/futures，移除旧测试依赖；同步架构说明并修复 rustdoc 链接。

验证在 `/tmp/xpost-serial-recovery-validation` 完成：复制目标库及未修改的 `xpost-elf`、`xpost-macros`，只为临时副本提供最小 workspace。

- client：Apple Silicon host 编译。
- server 和无 feature 线协议：`riscv32imc-unknown-none-elf` 编译。
- 同时开启 client/server：按约定编译失败。
- 临时 smoke：命令/应答往返、旧命令拒绝、server map/perf 生成、16 种 perf mask 解码、CSV 分区读取、日志从文件查询时间窗。
- client/server 文档：以 `RUSTDOCFLAGS="-D warnings"` 构建。

当前仓库直接构建仍被根 `Cargo.toml:78` 的重复 `xpost-elf` 键阻止。它不属于本次范围，未修改；其他 crate、固件和 agent 未调整。未进行 USB 真机收发或烧录验证。
