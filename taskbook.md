# XPOST #23 · 清理旧实验路径，交付一小时中量测试入口

在 `nan-mu/xpost#23` 最新 head 的独立 worktree 继续，沿用 `agent/experiment-matrix-v1 → agent/rbpf-vm`。审阅基线为 `08c9df6be278eaeacecccb0b7755c3b8974458ab`，#24 已合入。通过指定的 GitHub MCP 更新同一条开放 PR，交用户审核。本轮完成条件是交付可以立即开测的状态。整套中量测试由用户审核后在 T3 明确发令启动。

## 包内导读

从根目录的 `taskbook.md` 开始。下面按使用顺序排列；设计文件名均相对于 `references/design/XPOST_EXPERIMENTS_V1/`。

| 什么时候打开 | 文件或目录 | 用它做什么 |
|---|---|---|
| 实施或运行时首先读 | 包根下的 `procedures/medium.md` | 逐项测试伪代码与各自的“数据处理”（指标、分母、单位及结果资格）。沿用 xpost-test 的模块注释格式；T3 查库、修复、重试的操作约定见第五节。 |
| 追溯原矩阵时 | `03_MATRIX.csv` → `04_COMBINATIONS.md` → `experiments/` | 先确定 40 个组合及必测范围，再按程序核对伪代码、输入输出和触发点。旧 ABI 与烧录顺序以本任务书为准。 |
| 对接指标和数据库时 | `02_MEASUREMENT.md`、`05_EVENT_DICTIONARY.csv`、`sql/` | 对齐采集窗口、事件含义和表字段。用 `sql/queries.sql` 对照分析端需要读回的结果。 |
| 审核 A/B 定址时 | 包根下的 `references/rv32/README.md` → `results.txt`（同目录） | 先看复现条件和已测结果。需要复核时再用同目录的 `compare.py`、`rebase.py` 和三个样例目录；其中 ELF 是主机实验对照件。 |
| 追溯旧决定时 | 包根下的 `references/design/taskbook.md`，以及设计目录中的 `XPOST_EXPERIMENTS_INDEX.md`、`AGENT_TASK.md`、`VALIDATION.md` 和 `references/` | 查 #24 交接、历史基线和原验收依据。本轮实施以根目录任务书为入口。 |

## 一、沿调用链迁移并删除旧实现

先从正常入口正向追踪，再从设备写入和数据库写入反向核对。按当前 head 记录文件和符号；下表给出审阅起点。先把有效调用方接到新路径，再在同一 PR 中删除失去用途的入口、实现、配置及回退分支。同步更新默认配置、实验伪代码、分析查询和仓库说明。

| 从哪里查 | 本轮删除或迁移什么 | 应保留的行为 |
|---|---|---|
| `profiles.toml` → `xpost-agent/src/ops.rs` 的 `resolve_xdp` → `xpost-artifact/src/lib.rs` → 串口安装 | 删除 v1 索引→manifest→报告解析、旧 build_root 取件约定、跨 profile 的旧缓存回退及失效配置 | v3 索引解析、按身份验证的缓存、固件核对，以及 #24 从已验证字节到实际写入的链路 |
| `xpost-test/config/default.toml` → `common/src/main.rs` → `common/src/exam/exec_bench.rs` → 固件入口 | 删除旧计算程序列表、32 B workload map 请求及读回路径。核实 88 B workload_v2 是否进入代码；仅存在于历史设计的，标记已取代 | 七个 EXB2 程序的输入、调用、结果解释和记录 |
| 各实验 runner、`exam/repeat.rs` → agent 的固件烧录、冷安装与切槽操作 | 删除由轮次、速率、样本重复或导出隐式触发的重烧调用，合并仍在使用的重复安装流程 | 设备身份变化时的必要烧录，及实验专门测量的冷安装、预装和切换 |
| `xpost-test/common/src/db/`、`xpost-test/sql/` → 分析查询 | 删除被取代的三模式 SQL 生成、旧 workload 写入及仍把新样本导向旧表的路由 | 新库的四模式记录、现存历史数据及有实际调用方的历史只读查询 |

PR 按“旧符号及入口 → 全部调用方 → 新入口 → 已删除位置”交代结果。复核配置字符串、命令分派、默认值、错误处理和重试分支。做到两端能对上：正常入口只到达新实现，每一处写操作都有预期上游。缺件时流程结束于首次设备写入之前。针对仍有合法用途的共用函数写明调用方；历史数据清理不属于本轮。

## 二、用实际发布的 ELF 判定能否开跑

以 `libbpf-mcu v0.1.0`（`ef1771d9c949771c707b93fd9da5c3e2602efee0`）为核对基线。展开全部必测程序和模式，按 release 附件摘要、v3 `index.toml`、部署 ELF 的大小及 SHA-256 对件。HAL 走固件；AOT、JIT、rBPF 分别核对部署方式。对需要定址的原生模式，先让 xpost-agent 消费一份真实发布的 ELF，确认能取得所需重定位信息、生成目标槽镜像，再逐项核对槽容量。部署输入使用发布的 ELF，禁止用包内 .o、本地重编结果或旧缓存补位。

已检查的 v0.1.0 just_drop、just_tx、firewall 三个包缺少部署 ELF；其余包逐件核查。JIT 的 log2、bsort、strcmp 曾超过 8192 B 合成槽，以实际固件容量判定。任一必测项缺件、无法定址或超槽，记录“阻塞，中量未就绪，本轮未完成”，列出程序、模式、产物和上游缺口。采用后续发布修复时记录实际 tag、commit 和摘要。

## 三、按程序约定更新实验、伪代码和数据库

计算组使用 noop、prime、log2、copy、fletcher32、bsort、strcmp。EXB2 使用 8258 B 可写缓冲和 64 B 小端头；data/data_end 覆盖整块缓冲，每个样本调用一次，程序内部完成 rounds。分别解释 XDP 动作和直接入口返回的 EXB2 状态，记录 status、completed、result、digest；copy 和 bsort 同时核对完整 scratch。沿用已知 copy 输入 `44 20 82 3c`、n=4、rounds=3、seed=1，期望 status=0、completed=3、result=4、digest=`0x91efe859`、scratch 等于输入、填充保持原值。[完整向量及生产端交接](https://github.com/nan-mu/xpost/pull/23#issuecomment-7099824645)

网络组沿用 firewall 命中/未命中与计数、just_drop 动作、just_tx 完整输出帧，以及 case_swap/case_mark 切换前后的独立期望。以附件 `references/design/XPOST_EXPERIMENTS_V1/03_MATRIX.csv` 的 40 个组合及 required/optional 定义为基线。把 hot_swap 与 case 的对应关系写入伪代码和表映射，避免遗漏或重复计数。新 PostgreSQL 库应能按 run 查回条件、程序/ABI、模式、固件和部署摘要、槽位、输入、正确性结果、boot/epoch 及指标或 trace；使用现有查询核对一次完整读写。

## 四、审核前展开流程，明确每次设备写入的位置

在审阅基线，exam/mod.rs 声明八个子模块。默认 run 包含 just_tx、just_drop、firewall、exec_bench、case、hot_swap。dev 是 HAL smoke，repeat 是重复执行辅助模块。main.rs 另有 lookup 诊断组合与 formal-clear 清库命令。中量按五个实验族、四种模式、两种采集变体核对 40 格，其中 36 格必测、4 格 exec_bench Trace 为可选诊断。case 与 hot_swap 共用 case 族，保留各自观测；dev、lookup 按需调用，不计入正式矩阵。formal-clear 不进入测试队列。

逐模块流程见 `procedures/medium.md`。模式复用同一固件时连续执行；HAL 使用静态入口。

按实验标出负载、触发时刻和窗口终点。专测预装或冷安装时将相应动作放入测量窗口。重连、重启和重试先重新核对状态。分别统计固件烧录、XDP 冷安装、SRAM 预装、入口切换四种操作。审核者应能从计划直接看出，同固件同程序从一个速率点增至十个，或增加样本重复，固件烧录次数不变。

xpost-agent 根据目标固件的槽地址、容量及 helper 地址预先定址，同步 map_vma 和摘要。XPOST 的 warmup 准备空闲槽，成功后发布入口，失败时当前程序继续工作。分别记录预装耗时和切换停顿。三个 LLVM AOT 样例已有单槽对照和 A→B→A 往返证据；JIT、解释执行和真机结果分别注明。PR 简述可用于论文的结论边界。单槽复现材料见 `references/rv32/README.md`。

沿用 `references/design/XPOST_EXPERIMENTS_V1/02_MEASUREMENT.md` 的 embeprom 0.3.0 整数指标与 Metrics/Trace 独立采集约定。核对 RV32IMC 原子适配、任务/中断更新、静态开销和连续 epoch；接通固件五个 perf 回调。prepare 取得 S0，arm 开窗，freeze 等待在途观察结束并取 S1，read 导出，release 释放。Counter/Histogram 仅在同一 boot、epoch 内作差，Gauge 读取快照值。exec 在批调用外观察，case 在切换边界外观察。

## 五、就绪交付与用户启动

实现并对照包根下的 `procedures/medium.md`。该文件是本轮逐实验执行依据；历史设计中的旧八项负载、旧 ABI 和重刷顺序只供追溯。提交可直接执行的中量命令、计划、数据库连接配置位置与逐项查询入口，确认必测产物、设备、数据库、回调和分析链均就绪。已知阻塞继续修复或标出外部依赖；“可以审核”不能代替“可以测试”。复用已有短样本和检查，不增设测试框架。

开放 #23 交用户审核并等待用户在 T3 发令。启动后每完成一项，T3 自行连接数据库按 run_id 分析，再推进队列。发现问题就定位、修复、同步证据与提交到 #23，把受影响条件重新入队；重试后同步结果，保留原始尝试。用实际计划与数据库逐项对账。

完整中量计划预算 ≤60 分钟，包含正常部署、等待、采集、逐项分析和收尾。缩减采样密度时保留全部必测组合和七个计算程序。分别报告实际总墙钟、额外修复/重试耗时和四类设备动作。[PR #23](https://github.com/nan-mu/xpost/pull/23)


## 六、在英文 PR 中呈现实测结果

#23 的标题、正文、本轮进度评论、修复与重试说明均用英文。就绪时在正文给出启动命令、预计时长和待测矩阵。用户启动后，每项从数据库查回并分析结果，在同一 PR 更新。保留 run_id、条件、资格及失败或重试记录。

在 PR 正文直接嵌入由已完成 run 生成的图，使用英文图题、坐标轴（含单位）、图例与图注。just_tx 按实际 offered pps 展示回包损失与 RTT；just_drop 对照 offered、ingress 与 completed-drop pps；firewall 分面展示合法交付率与非法命中比；exec_bench 按七项 workload 展示四模式的合格批次 ticks/round；case 给出各策略正确数与发送数；hot_swap 分开画 DUT 切换耗时和主机回复间隔。可组合成多面板。可选 lookup 有结果时再画 hit/miss 桶。Trace 用独立时间线或标注呈现。

图注给出实际重复数、有效样本数、统计口径和对应 run_id 或查询入口；重复较少时画原始点与范围。失败、缺测和不合格样本注明原因，不填零。优先复用现有分析工具，把图随 PR 提交或上传，核对正文能直接看到图。开测前保留待测状态，获得真实数据后再发布相应图。
