# 任务书 · PR41 数据库结构同步与模块清理

开发仓库：nan-mu/xpost；继续 [PR #41](https://github.com/nan-mu/xpost/pull/41) 的现有 head `agent/runner-db-runtime-config`，base 为 main。评审基线及当前已核实 head：`07ffaffb54c260ec9574682b44fa1cf3b75f5c15`。使用独立 worktree，交付同一个开放 PR，不合并。

任务书仓库：mcp/xpost，pr41；共享记录 main SHA：`a98e1bb60d2625d25c7d317708d87014800933dd`，决策 runner-db-runtime-config、runner-schema-reconcile。任务书自包含，不依赖 pr40 分支。

## 事实与边界

- F1. 运行时连接配置固定为 /root/.db，含 PGHOST、PGPORT、PGUSER、PGPASSWORD、PGDATABASE；保留 root 所有、0600、字面值解析及密码不进入编译产物和日志的现有约定。用户明确撤回额外的数据库名限制要求：连接器不硬编码目标库，沿用 PR41 既有连接与目标选择行为。NAS postgres 历史库不在本轮操作范围内。
- F2. 新实验的 Protocol V1 目标为 release schema（通常是 experiments.release），当前 DDL 定义 12 张 bench_ 表及每种 hal/aot/jit/rbpf 的 30 张表。唯一结构定义来源是 xpost-test/sql/protocol_v1.sql；其中包含 domain、函数、外键、索引、触发器。用户决定缺表创建、字段一致保留、字段不一致删除该表及其数据后重建；不设计版本迁移。其他表的数据保留；依赖冲突无法安全处理时回滚并报告，由用户解决。
- F3. 评审基线的 common/src/db/protocol_v1.rs::initialize_if_empty 通过名称/数量识别完整安装，并拒绝部分初始化；db/protocol.rs::check 检查存在性及权限，未核对字段结构。common/src/exam/medium.rs::protocol_tables、exec_bench.rs、case_store.rs 又分别维护表清单。medium.rs::preflight 的 initialize_database 布尔参数使检查与写入混在一起（约 871–875、956–965 行）；campaign 锁仅保护同一 campaign，不能自动保护整个 schema。
- F4. db/protocol.rs、exam/case_store.rs、exam/exec_bench.rs 存在 Artifact 创建、关联及摘要计算的重复实现。各实验的专用写入语义不同，不要求统一为通用 writer。
- F5. [NAS 权限观察](tests/nas-release-acl.md) 已确认 experiment_rw 对 release 有 USAGE、CREATE，未来由其创建的表自动授予 experiment_ro、backup_release SELECT。PR41 的[审计记录](https://github.com/nan-mu/xpost/blob/07ffaffb54c260ec9574682b44fa1cf3b75f5c15/.audit/taskbook/mcp/xpost/721cbccaa74d8805c6e9a833fd2397449c32c162.md) 报告运行时连接、38 项常规测试、5 项数据库 fixture、实际 TR3000 132 表创建并回滚成功；这些是既有执行报告，不是任务书作者重新运行的结果。完整硬件 preflight 未通过 Agent 连接检查，正式实验未运行。

## Goal 1 — 逐表结构同步

使用 Matt implement + tdd。将当前全有或全无初始化替换为统一的数据库准备入口。按 DDL 管理的限定表名逐表比较实际结构与期望：缺表创建；名称、类型（含 domain/类型参数）、顺序、可空性一致时保留原表及数据；存在差异时仅删除不兼容表并重建。保留规范 DDL 的默认值、约束、索引及触发器语义；若发现其他结构对象冲突，不能安全恢复则报告。不得仅凭表数量或版本号判定一致；不得新增版本表、迁移链或第二份人工维护的字段定义。

一次准备在事务内完成，按真实依赖顺序处理表及相关对象。可以临时处理受影响的外键并恢复，但不能通过无界 CASCADE 删除其他表/对象，也不能清空结构一致的表来规避冲突。若保留行引用了被重建表的旧记录、依赖定义不兼容、所有权不足或存在其他无法安全恢复的对象，整个事务回滚，明确报告表和冲突原因并退出，交给用户处理。未知名称的表及非管理对象不修改。保留已有 schema 授权和新建表的默认授权；不要求扩大 rw 权限至整个数据库。

验收（隔离 PostgreSQL fixture，走正常数据库准备入口）：
- 空 release 初始化完整 132 张表；部分缺表时只补缺失表，保留其他表的标记行及非管理对象；重复准备不改变兼容表的数据。
- 构造已存在的同名但字段名、类型、顺序或可空性不符的表，确认能识别；无依赖冲突时仅重建目标表、目标旧数据消失，其他兼容表的标记行保留，新表可正常写入且结构与规范一致。
- 构造依赖外键且保留数据仍可满足约束的场景，检查重建成功后约束仍存在且有效；构造保留行引用目标旧数据的场景，确认准备失败、事务回滚、双方数据及约束不变。不兼容依赖不得被静默移除。
- 验证已有 domain、函数等公共对象不会因补建一张表而发生重复创建错误；出现不可处理冲突时保留原状态并给出可行动诊断。检查新建对象在实际授权模型等价的 fixture 中能被只读角色 SELECT，而不能写入。

## Goal 2 — 分离准备、只读检查、连接与运行期互斥

使用 Matt implement + tdd。数据库结构管理由 db 模块承担；新采集 all、medium、standalone exec_bench 显式调用准备入口，随后进入采集。db-preflight、all/medium-preflight 只检查结构和权限；resume 只检查并恢复原记录，reconnect 只重新建立连接，不得自动删除、重建或补建表。plan/dev 不访问数据库。删除 preflight 中控制数据库写入的布尔参数。保留现有硬件检查、计划及 resume 判据，不改变实验语义。

新采集与 resume 在数据库使用期间持有统一的 schema 使用保护，覆盖全部采集、状态写入、Case 独立连接及重连路径；结构调整与活动采集互斥，并消除准备结束到采集保护建立之间的竞态。保留原 campaign 独占语义。保护连接失效时必须停止或在安全恢复保护且重新确认条件之后继续，不能在失去保护后写入。具体锁实现由执行者选定。

验收：用两个独立数据库连接/runner fixture 验证首次并发准备无重复对象；活动采集保护存在时另一个准备者不得删除或重建表，可明确拒绝或安全等待。保护释放后可完成准备。构造锁连接断开场景确认不会继续无保护写入。对缺表或字段不一致的数据库运行只读检查与 resume，确认失败、目录结构与原数据不变；重连不调用准备。保留 plan/dev 无 .db 的现有行为。

## Goal 3 — 清理重复职责

使用 Matt implement 和 code-review。先核对真实调用方再重构：
- 统一 Protocol V1 表清单及结构检查，消除 medium.rs::protocol_tables、exec_bench.rs、case_store.rs 中重复维护的 schema 清单；统一校验规范定义的表，不要求各实验负责数据库初始化。
- 将连接、结构准备、只读结构检查及公共持久化操作集中到 db 模块的明确接口。不要以同名新旧接口长期并存；删除被替代的实现及调用。
- 复用 Artifact 创建、run 关联及 SHA-256 编码操作；保留 Case 的 metadata/media_type、EXB2 在 run 创建前保存输入的顺序和现有事务边界，不为统一而改写数据格式。专用 case_store、medium_network_store 可保留。
- 审计本次数据库改动的剩余重复代码和无效模块：只处理与本轮数据库职责直接相关且可证实的冗余；其他问题报告给维护者，不扩张到整个 runner 重构。

验收：列出删除的旧入口和最终调用关系，核对所有写入路径只使用一套结构定义与准备入口；核对 Artifact 数据、关联、摘要、metadata 和失败回滚行为与原语义一致。运行受影响的现有数据库 fixture（含 EXB2 四模式、Case、网络写入与恢复及 medium_run.sql），补齐本轮缺失覆盖；正常编译、cargo test、相关 rustfmt 与 diff 检查通过。更新数据库操作文档及开发仓库用户决策记录中的过时行为说明，包括 SQL 文件头和部分 schema 拒绝规则。

## 证据复用与交付

已记录的真实 NAS 权限观察仅复用其权限结论；既有凭据解析、TR3000 连接和未受影响的实验逻辑证据按适用条件复用，不重复实机烧录或正式采集。若版本、输入或环境变化使证据失效，注明具体差异和受影响验收，未授权实机操作不执行。本轮所有破坏性验收必须使用可丢弃的数据库，禁止对 NAS 实际 experiments、postgres 执行 DROP、TRUNCATE 或写入验证；不要读取、输出真实密码。不得修改真实 DB 权限或固件。

按 task-brief-to-pr 在既有 PR head 继续，先读仓库规则；若当前 head 已变化，核对新提交并保留他人改动，发现事实冲突报告。主代理运行本轮剩余验收，使用 Matt code-review 核对规格及模块职责，使用 pr 更新同一开放 PR 的英文摘要、实际测试结果、未验收项与风险（明确字段不一致会丢失目标表数据）。使用指定 GitHub MCP 交付，禁止自动合并；等待用户审核。
