# EXB2 HAL：共享协议、固件构建与 Agent 交互

HAL 代表直接开发 MCU 固件的合理最佳实现。八种任务的语义、设计和正确性向量分别见第 2 节的同名文档；前七项来自原代码，matmul 是新增任务。本文及各 workload 文档共同作为实施与独立验证的技术依据；任务书只确定范围、交付和授权。本文件只说明共享设计。每个固件在编译时选定一个 workload，Agent 选择要部署的固件而非在运行时选择其算法。本文是参考设计，不是可执行算法代码。

## 1. 原代码及为什么替换

原代码固定在 XPost PR41 合入提交 [bb6b906f0b39abe440107c3831da88d825bd325c](https://github.com/nan-mu/xpost/tree/bb6b906f0b39abe440107c3831da88d825bd325c)。原始 C 文件在 [examples/exec_bench_hal/exb2/](https://github.com/nan-mu/xpost/tree/bb6b906f0b39abe440107c3831da88d825bd325c/examples/exec_bench_hal/exb2)。[build.rs](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/examples/exec_bench_hal/build.rs) 注明其来自 libbpf-mcu 07a4ea9a81a7fedf7bef2d6ced8d7fcfcccfd113。

| 原文件 | 原职责 |
| --- | --- |
| [exec_protocol.h](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/examples/exec_bench_hal/exb2/exec_protocol.h) | 固定 EXB2 布局、状态码、范围和读写辅助函数 |
| [exec_program.h](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/examples/exec_bench_hal/exb2/exec_program.h) | 选择内核、检查输入、执行内部 rounds、更新摘要并发布结果 |
| [build.rs](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/examples/exec_bench_hal/build.rs) | 以 EB_TASK_ID=0..6、EB_HOST_ENTRY、Clang rv32imc/ilp32/-Os 构建七个 C 对象，链接到一个固件 |
| [src/bin/exb2/mod.rs](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/examples/exec_bench_hal/src/bin/exb2/mod.rs) | 读取 task ID，通过 ENTRIES 表选择 exb2_hal_0..6，记录一次入口调用的时间 |
| [src/bin/main.rs](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/examples/exec_bench_hal/src/bin/main.rs) | 初始化 ESP32-C3、USB Serial/JTAG 和 Metrics；通过 ExecBatch 执行入口 |

原设计使 HAL 和 eBPF 共享任务代码，方便字节对照。但它限制了 HAL 固件可使用的原生实现方式，并保留了不必要的运行时算法分发。维护者选择任务语义等价，而非源代码或指令等价。保留原始代码的 Git 历史及本文中的链接，删除 C 编译路径；保留开发仓库 examples/exec_bench_hal/exb2/ 的目录名，移除 C 编译输入并放入八份同名 Markdown 文档。公共协议、构建与通用 Agent 交互写入 examples/exec_bench_hal/README.md；不建立 docs/ 目录，也不将八项任务合并成一份文档。开发仓库中新增或修改的 README、八份 workload Markdown、其他设计文档及审计报告全部使用英文。Gitea 中文 Reference 供审核，实施者须准确转换为英文并保留公式、代码引用、任务行为及期望；不要求翻译与本轮无关的历史文件。

## 2. 八份独立 workload 文档

任务书中的 references/exb2/ 对应开发仓库的 examples/exec_bench_hal/exb2/。将这八份文档分别以英文交付到原目录。每份 Markdown 与 src/workloads/ 中同名 Rust 文件一一对应。不得使用 docs/ 替代 exb2/，不得合并八个 workload 的任务设计。

| 文档 | Rust 文件（相对 examples/exec_bench_hal） |
| --- | --- |
| [noop.md](exb2/noop.md) | src/workloads/noop.rs |
| [prime.md](exb2/prime.md) | src/workloads/prime.rs |
| [log2.md](exb2/log2.md) | src/workloads/log2.rs |
| [copy.md](exb2/copy.md) | src/workloads/copy.rs |
| [fletcher32.md](exb2/fletcher32.md) | src/workloads/fletcher32.rs |
| [bsort.md](exb2/bsort.md) | src/workloads/bsort.rs |
| [strcmp.md](exb2/strcmp.md) | src/workloads/strcmp.rs |
| [matmul.md](exb2/matmul.md) | src/workloads/matmul.rs |

旧七项的文档包含原 C 代码链接、任务语义、原生实现思路、Agent 输入和固件行为、已有正确性向量。新增 matmul 无旧 C 代码，记录研究来源、任务契约、新独立期望和 MCU 实现选择。共享接口与构建规则见本文，不要求在各文件中重复完整协议。

## 3. 现有 EXB2 任务契约

以下记录当前 ABI，供本轮兼容构建和正确性检查使用。通信压缩、正式输入规模、采样次数、计时精度另行设计；本节不将旧方案冻结为长期最优设计。

### 输入与输出

总缓冲区 8258 字节，头部 64 字节，little-endian。其后是 Input、Scratch 和剩余 Padding。n 是任务参数，不是 rounds。协议检查长度、magic、version、header length、task、reserved、variant、n 和 rounds（1..65536），以及 Input/Scratch 长度是否符合任务。对当前单 workload 固件，task 只能是身份校验项，不能选择算法。

| Offset | Size (B) | 字段 |
| --- | ---: | --- |
| 0 | 4 | magic：0x32425845，字节 EXB2 |
| 4 | 2 | version：2 |
| 6 | 2 | header size：64 |
| 8 | 4 | task ID |
| 12 | 4 | n |
| 16 | 4 | rounds |
| 20 | 4 | variant（仅 log2 支持 0 或 1，其他为 0） |
| 24 | 4 | seed |
| 28 | 4 | Input bytes |
| 32 | 4 | Scratch bytes |
| 36 | 4 | status |
| 40 | 4 | completed rounds |
| 44 | 4 | result.lo |
| 48 | 4 | result.hi |
| 52 | 4 | digest |
| 56、60 | 各 4 | reserved，必须为 0 |

成功状态为 0；原协议错误码 1..7 依次为 BAD_MAGIC、BAD_VERSION、BAD_TASK、BAD_RANGE、BAD_LAYOUT、BAD_VARIANT、SHORT_BUFFER。请求状态初始为 0xffffffff。成功后 completed=rounds。结果 lo/hi 描述最后一个完整 round，digest 覆盖全部 rounds。原 C 每轮按 u32 环绕运算：

~~~text
d = seed XOR 0x811c9dc5
for r in 0..rounds:
    执行本轮任务并得到 lo, hi
    d = (rotl32(d, 5) XOR lo) + (hi XOR r XOR 0x9e3779b9)
发布 status、completed、lo、hi、d
~~~

以上是语义描述，不规定 Rust 代码结构。除定义的结果字段及可写 Scratch 外，原 Input 和 Padding 不应改变。原 C 使用 volatile n 与编译器屏障避免循环中的真实工作被提出或消除。Rust 允许有效优化，但必须保留采样所声明的实际任务工作，特别是 copy 与 bsort 的多轮行为。

### 已有正确性向量

旧七组独立期望分别位于第 2 节的对应文档。新增 matmul 的 task ID 为 7，参数、矩阵编码及期望见 [matmul.md](exb2/matmul.md)。固定来源为 [xpost-test/common/src/exam/exb2.rs](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/xpost-test/common/src/exam/exb2.rs)；旧向量均为 rounds=3、seed=1、variant=0，Scratch 初始 0xa5，Padding 为 0xcc。它们是正确性参照，不是新正式性能网格。

## 4. 编译时选择 workload 与独立产物

现状：[Cargo.toml](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/examples/exec_bench_hal/Cargo.toml) 只有一个 bin exec_bench_hal 和 diag Feature；[.cargo/config.toml](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/examples/exec_bench_hal/.cargo/config.toml) 默认目标 riscv32imc-unknown-none-elf。[profiles.toml](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/profiles.toml) 的 exec-bench-hal 也仅指定一个固件路径。单一 bin 切换 Feature 会重用同一个 release 可执行文件路径。

用户已确定：八种 workload 各自一个 Cargo Feature，单固件只含一个 workload。可以共享协议、计时、串口控制等 Rust 模块，但禁止共享一个运行时算法选择器。删除 C workload 的构建和链接。若保留 build.rs，只保留确有需要的固件链接逻辑。现有 diag 及 src/lib.rs 是另一套历史 workload sweep；不得使其成为新正式 HAL 固件的第二条算法执行路径。评估其调用者，隔离或退役不适用的旧入口。

建议的命名和构建接口如下，均为待实现设计，不是已经存在的命令：

| Feature | Firmware ELF 文件名（建议） | Agent firmware profile（建议） |
| --- | --- | --- |
| noop | exec_bench_hal_noop | exec-bench-hal-noop |
| prime | exec_bench_hal_prime | exec-bench-hal-prime |
| log2 | exec_bench_hal_log2 | exec-bench-hal-log2 |
| copy | exec_bench_hal_copy | exec-bench-hal-copy |
| fletcher32 | exec_bench_hal_fletcher32 | exec-bench-hal-fletcher32 |
| bsort | exec_bench_hal_bsort | exec-bench-hal-bsort |
| strcmp | exec_bench_hal_strcmp | exec-bench-hal-strcmp |
| matmul | exec_bench_hal_matmul | exec-bench-hal-matmul |

从 examples/exec_bench_hal 执行一个 Cargo 命令，例如 cargo build-hal-all。该命令内部逐个构建，等价于为每个 workload 单独选择 Feature 和 bin。一个可选实现是 Cargo alias 调用 Rust 构建协调器（xtask），每个 workload 使用独立 bin 或独立产物路径。另一合理实现也可采用独立输出目录。编译器实际使用的 Feature 必须互斥；required-features 可以约束 bin 是否可构建，但不能单独保证 Feature 互斥。不能以 --all-features 生成一个通用固件。

~~~text
cargo build-hal-all
  -> build noop only       -> release/exec_bench_hal_noop
  -> build prime only      -> release/exec_bench_hal_prime
  -> build log2 only       -> release/exec_bench_hal_log2
  -> build copy only       -> release/exec_bench_hal_copy
  -> build fletcher32 only -> release/exec_bench_hal_fletcher32
  -> build bsort only      -> release/exec_bench_hal_bsort
  -> build strcmp only     -> release/exec_bench_hal_strcmp
  -> build matmul only     -> release/exec_bench_hal_matmul
~~~

默认产物区域为 examples/exec_bench_hal/target/riscv32imc-unknown-none-elf/release/。若 Cargo target directory 被配置覆盖，使用实际构建路径；不要猜测并使用旧文件。[scripts/cargo_artifact.py](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/scripts/cargo_artifact.py) 已说明此类路径问题。新的 Cargo 命令及各 firmware profile 的 build/artifact 必须一致。

单 workload 构建只更新其对应 release 文件。八份产物应同时可用于部署，不需手动重命名。完整构建命令遇到失败应返回失败；不得把旧 ELF 计为本次成功。构建只生成产物，不自动烧录。实际 alias、bin 名称和内部构建方法由实施者选择，并更新 README 与 profiles.toml。

## 5. Agent 与固件的交互

### 职责与固件选择

正常调用链为：

~~~text
xpost-test：选择 workload 与采样计划
  -> xpost-agent：选择对应 firmware profile、构建/烧录 Firmware ELF
     -> ESP32-C3：启动唯一的 workload
  -> xpost-agent：转发实验控制和缓冲区
     -> 固件：验证输入、执行已编译 workload、记录计时、返回结果
  -> xpost-test：校验结果、记录实际 firmware identity、入库
~~~

原 [exec_bench.rs](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/xpost-test/common/src/exam/exec_bench.rs) 中 Mode::firmware() 对 HAL 返回一个 profile；run_selected 按 mode 烧录，run_mode 遍历七项任务。正式 [medium.rs](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/xpost-test/common/src/exam/medium.rs) 的 run_group 也按 Firmware SHA-256 分组，随后调用 run_mode。

新设计按 (HAL, workload) 选择 Firmware ELF。standalone、medium/all、预检查、计划及 resume 均须使用对应身份，不把八项任务交给同一 HAL 固件。切换 workload 才需要选择其他固件；同一 workload 的重复采样复用当前正确固件。Agent 保持设备访问、构建、部署与采集控制职责，runner 保持实验调度、验证和入库职责。Firmware ELF 本身不接收 XDP ELF 安装，也不初始化 XDP 或 map runtime。

本轮 HAL 计划包含八个 workload；AOT、JIT、rBPF 仍为已有七项。当前 runner 的 PROGRAMS、conditions、正式 plan/identity/resume、预检查和部署索引需按 mode 生成，不能因全局追加 task 7 就请求不存在的 matmul XDP ELF。正常 HAL 入口须能选择、运行和记录 matmul；其他 mode 的新增项待 libbpf-mcu 发布对应产物并完成消费后加入。PR42 不构造假的 XDP 产物，不宣称 matmul 四模式对比完成。

现有 [Protocol V1 DDL](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/xpost-test/sql/protocol_v1.sql#L531) 的 hal_exec_bench_config.workload CHECK 仅允许原七项。将 matmul 加入 HAL 的合法 workload，并核对正式入库、校验及读取路径。只修改此新任务所需的持久化契约；在隔离 PostgreSQL fixture 中验证。不得自动删除或重建 NAS 中已有数据以容纳新任务。若现有数据库的 CHECK 或 PR41 初始化逻辑不能无损接收新值，报告具体阻塞，等待维护者决定，不以成功的主机测试冒充生产入库。

### 现有 HTTP 和串口命令

Runner 的 AgentClient::exb2 发送 POST /v1/exec/batch，JSON 字段 input_hex。Agent 在同一设备锁内处理完整事务，返回 boot_id、begin_tick、end_tick、execution_return、base64 output。来源：[agent.rs](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/xpost-test/common/src/agent.rs#L178-L203)、[ops.rs](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/xpost-agent/src/ops.rs#L760-L823)、[ExecBatch](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/xpost-serial/src/server/exec_batch.rs)。

| Agent 发给固件的命令 | 固件当前行为及新 HAL 的兼容契约 |
| --- | --- |
| 启动后等待 OK init（get_init 是 Host 等待，不是 wire 请求） | 初始化设备、串口和 Metrics，报告 ready；无需 Wi-Fi |
| exec batch reset | 状态切为 Uploading(0)，开始接收新输入；不是切换 workload |
| exec batch write offset hex | 校验顺序、偏移、容量，解码并写入暂存区；每块最多 256 B |
| exec batch invoke | 完整输入就绪时执行一次；新 HAL 只调用编译选定的 workload。task ID 不匹配返回错误状态，不能选择另一任务 |
| exec batch read offset max_bytes | 从已执行的结果缓冲区返回指定偏移的原始字节，每块最多 512 B；读操作不再次执行任务 |
| perf prepare uuid metrics profile | 建立 Metrics 会话，关联 run UUID 和 profile，返回 epoch、boot ID |
| perf arm epoch | 记录 DUT 初始计数器和时间边界 |
| perf freeze epoch | 冻结结束快照，返回 tick、flags、长度和 digest |
| perf read epoch offset max_bytes | 按偏移读取冻结 Metrics 数据 |
| perf release epoch | 释放会话资源 |

当前 Metrics 由 [exec_metrics.rs](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/examples/exec_bench/src/exec_metrics.rs) 共享实现。HAL invoke 返回原生 status（成功 0），AOT/JIT/rBPF 返回 XDP action（PASS 2）；runner 结合 mode 区分，不能将二者混用。

当前时钟为 DUT 自启动的整数微秒，64 位 tick。HAL 原计时区间包含原生 C 入口；非 HAL 原计时区间包含 XPost invoke 路径。输入串口传输、结果导出及 Metrics 边界动作不进入单次执行耗时。新 Rust HAL 应明确实际测量边界，保持所比较的任务工作及正确性；是否将协议解析移出计时等问题需要后续统一决定。不要为了让 HAL 更慢而添加 XPost 运行时功能。

### 当前通信问题及待定改进

每个 Batch 需要一次 reset、33 次 write、一次 invoke、17 次 read，共 52 个串口请求。noop 实际没有 Input/Scratch，仍传输全部 8258 B；这是通信开销，不是任务执行开销。历史采集曾有分块上传和读取超时，包括 HAL fletcher32；旧诊断记录留在 Git 历史，不作为本轮验收输入。不能把它认作 HAL 原生算法错误。

客户端 [uart.rs](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/xpost-serial/src/client/port/uart.rs#L144-L155) 在写完并 flush 后获取 SystemTime；[record/mod.rs](https://github.com/nan-mu/xpost/blob/bb6b906f0b39abe440107c3831da88d825bd325c/xpost-serial/src/client/record/mod.rs#L261-L266) 只匹配时间更晚的记录。若回复先被记录，存在过滤有效回复的可能。尚未实机复现，不能确认为历史根因。普通串口请求 5s 超时，Invoke 使用 30s 期限。读取失败应与执行失败区分；当前 Agent 读取失败时整个 HTTP 调用返回错误，不能假设 Invoke 未执行。

后续可讨论一次准备输入、DUT 本地多次采样、冻结结果后安全分块读取、请求 ID 和回执关联、高精度时钟与独立 warmup。本次 HAL 构建设计不批准这些改版或新采样数量。当前正式 EXB2 由 5 runs × 100 batches × 3 internal rounds 构成，旧项目决定要求 batch loop 位于测量程序内部；改变其含义前需重新确认比较目标。

## 6. 独立验证的技术判据

本节是执行端验收的技术依据。独立模型直接读取本设计及第 2 节的八份 workload 文档，沿正常入口和真实产物核对以下行为，不以实现者摘要、文件名或测试通过代替证据。

### HAL-01：原生固件及文档

- 检查原 C 编译路径、旧运行时分发及正式固件中的其他 workload 已移除。以真实 Cargo Feature、编译输入和 Firmware ELF 为证据；仅改名或将旧算法隐藏在运行时分发中均失败。
- 检查原 exb2/ 目录内八份英文 Markdown 与 src/workloads/ 中同名 Rust 文件的对应关系。原七份保留固定源码引用，新 matmul 保留研究引用、任务契约和独立期望。共享英文说明位于 README。目录改名、合并各任务文档或把中文 Reference 原样提交至开发仓库均不满足要求。
- 用各文档独立期望和代表性输入核对完整输出及未修改字节。对错误 task ID，固件拒绝或返回协议错误，不执行其他 workload。不得使用固定答案替代计算，或将应计时工作移到准备阶段。允许任务等价的优化，不设任意速度门槛。
- 核对 matmul 的完整矩阵结果、尺寸边界、输入及 Scratch 约束、算法真实复杂度和研究来源。未实现 9/4 算法时，不得将常规算法标为 9/4 或从小规模数据推导该指数。

### HAL-02：聚合构建与产物独立性

- 从无旧产物的构建目录执行聚合 Cargo 命令。核对八份有效 Firmware ELF、每项唯一 Feature、实际路径和 SHA-256。未选或同时选择多个 workload 不得生成身份模糊的正式固件。检查编译失败不使用旧 ELF 冒充成功。
- 重建任一 workload，另外七份文件保持存在且摘要不变。所有产物可由正常 Agent profile 直接消费，不能靠人工重命名。一个通用固件复制成八个文件或最后一次构建覆盖其他产物均失败。

### HAL-03：正常入口与数据

- 从 standalone exec_bench 和 medium/all 的计划、preflight、部署、resume 追到 Agent 构建与烧录以及 DUT 执行。核对每项 HAL workload 与实际固件身份。相同 workload 的重复不隐式重烧；HAL 不安装 XDP ELF。
- 新 matmul 必须进入 HAL 的正常实验入口并产生可验证的实验数据。非 HAL 保持原七项，不请求尚无发布产物的 matmul。仅实现辅助脚本或单独手工烧录不算接通。
- 使用隔离数据库验证 matmul 的合法 workload 值、输入、结果、时钟、固件身份及读回。现有 NAS 数据不得删除或覆盖；无法无损入库时报告阻塞。
- 记录实际 Firmware ELF SHA-256、workload、Feature、优化设置和计时边界。旧 C 基线和新 Rust 基线不得无标记混用。缺少真实硬件结果标未验证，不要求全量 campaign。只在任务、输入和计时边界可比时解释性能。

本文根据固定版本源码和用户决定形成，未运行构建或硬件测试。八个 workload 的文件对应布局已确认；共享 Rust 模块布局、Cargo 聚合命令内部实现及算法优化细节由实施者选择。
