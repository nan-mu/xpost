# 任务书：XPOST 数据库连接重构

开发仓库：`nan-mu/xpost`。从最新远端 `main` 创建独立 worktree 与 `agent/<name>` 分支，交付新的 PR，base 为 `main`。PR #23 已合并；本轮调查基线为 `246d0427124f5b543ff2c536ec10e3270cbe67b5`。开始前核对最新代码与 `AGENTS.md`。不自动合并。

Gitea 共享决策基线：`0422409bfbdf41cc884d475731acefb236d5edac`（`main` 的 `decisions.md`，条目 `runner-db-runtime-config`）。

## 事实与约束

- F1. 用户已决定停止历史数据迁移，NAS 的 `postgres` 保留历史数据，新采集写入 NAS 的 `experiments`。本轮不访问、清空或迁移旧数据，不关闭 Mac 数据库服务。
- F2. TR3000（ImmortalWrt）已由用户手动配置 `/root/.db`，并使用 psql 成功连接 `experiments`，登录角色为 `experiment_rw`，服务端报告 `172.16.1.3:5432`。详见 [连接测试](tests/tr3000-db-connect.md)。复用该证据，不重复安装客户端或验证旧连通性；文件权限及应用层读取仍待验收。
- F3. 当前 `xpost-test/config/default.toml` 的 `[postgres]` 通过 `common/src/config.rs` 的 `include_str!` 嵌入二进制；包含连接参数和 `dbname`/`protocol_dbname` 两套目标。`common/src/db/pg.rs` 连接历史路径，`common/src/db/protocol.rs` 连接 Protocol V1 并拒绝两个数据库同名。不要复述旧密码。
- F4. `common/src/main.rs` 将 `all`、`medium` 及相应 `*-preflight` 调度到 `exam/medium.rs`；网络实验由 `exam/medium_network_store.rs` 写入，case 由 `exam/case_store.rs` 写入，exec_bench 由 `exam/exec_bench.rs` 的 `run_mode` 调度。新路径采用 `sql/protocol_v1.sql`，定义 `bench`（12 表）及 `hal`、`aot`、`jit`、`rbpf`（各 30 表）。case 当前实际采集为 AOT，DDL 中其他模式的表不等于已支持采集。
- F5. `common/src/db/protocol.rs::check` 以只读事务检查表存在、Schema USAGE、表 SELECT/INSERT 和 `bench.run` UPDATE。旧 `db/mod.rs::ensure`/`schema.rs::ensure` 会自动建旧表，旧 `formal-clear` 可 TRUNCATE；两套模式表存在同名但不兼容的定义，不能将旧写入路径直接重定向至新库。
- F6. 用户未授权正式实验、固件烧录、修改数据库权限体系或清理历史数据库。本轮只做数据库配置/写入路径改动与相关验证。现有固件、Agent 协议、采集参数、实验顺序、resume 语义及 Protocol V1 结果判据不变。

## Goal 1 — 运行时连接配置

使用 Matt `implement` 和 `tdd`。将数据库连接集中为一个可复用入口。所有保留的数据库访问路径（包括 preflight、重连、独立数据库连接和 resume）使用同一运行时配置，不得继续读取嵌入凭据或使用旧数据库地址作为默认值。

TR3000 正常入口保持 `./xpost-test`，不增加启动参数，不要求用户先 source 文件。数据库相关入口在实际需要数据库时读取固定路径 `/root/.db`。文件采用 dotenv `KEY=VALUE` 格式：

```dotenv
PGHOST=<NAS 可达地址>
PGPORT=5432
PGUSER=experiment_rw
PGPASSWORD='<由用户填写>'
PGDATABASE=experiments
```

五项均必填；支持用户已采用的单引号密码形式，解析为字面值而非执行 Shell；明确处理双引号、特殊字符、空白和错误行。端口须为有效的非零 u16。不得把实际凭据复制到源码、配置模板、日志、PR、审计或测试报告，不得读取/展示真实文件内容用于审查。用 driver 的结构化配置传递字段，不拼接未转义 DSN；敏感字段的 Debug、错误、重连日志不得泄露密码。保持文件由 root 持有、0600；验收通过元数据检查而非输出内容。若权限不满足，报告用户处理，不擅自修改凭据。

去除编译时 `[postgres]` 凭据和旧目标名称，不保留编译时连接回退。仅计划与无需数据库的 `dev` 等入口不得因缺少 `.db` 而失败；当前 `NoTls` 的传输约束保持不变，本轮不引入 TLS 架构改动，仅在可信实验内网使用。

验收：用合成凭据文件和隔离 PostgreSQL 验证正常加载、引号/特殊字符、缺文件/缺字段/非法端口的失败行为、无秘密日志，以及重连和正常入口确实访问配置指定的数据库。用无 `.db` 的场景验证计划入口不连接数据库；复用现有无 DB 诊断测试，不为此触发硬件操作。检查编译配置/产物不再依赖真实凭据；不搜索或输出真实旧密码。实际 TR3000 验收在 Goal 3。

## Goal 2 — Protocol V1 写入边界

使用 Matt `implement`、`tdd`。新采集及保留的数据库写入入口只能使用 Protocol V1：公共记录、证据和状态写入 `bench`；各模式数据写入相应 `hal`、`aot`、`jit`、`rbpf`。保留原有 schema、表、约束、事务与证据恢复机制，不另设 `run`/`dev`/`release` 存储布局，不改变既有 dev/release Schema 的内容或权限。

沿 `main.rs` dispatch 审计旧独立实验、`lookup`、`exec_bench`、`formal-clear` 的真实调用路径；列出哪些入口仍调用 legacy writer、旧 DDL 和历史配置。对于已经被 Protocol V1 替代的旧数据库写入/初始化路径，移除可达入口和无调用代码；不把旧 DDL、旧 writer 或 `formal-clear` 接到新数据库。仍有用途的独立诊断保留其诊断语义，若需落库则接入已存在的 Protocol V1 正常写入路径；如果无法等价迁移且涉及实验语义改变，报告具体入口和冲突，等待维护者决定，不擅自重写实验或静默删掉有效功能。旧 SQL 文件若仅被明确保留为历史参考，可保留但不得成为运行时初始化入口。

验收：在隔离的 PostgreSQL 上用仓库 `sql/protocol_v1.sql` 初始化，再从现有可测试入口验证公共行、四模式代表性写入与查询、状态更新和事务失败行为；复用现有数据库 fixture，核对它们实际使用新协议，避免运行会 DROP/TRUNCATE 的 legacy fixture 对真实数据库操作。核对所有保留的数据库入口均无历史写入/清空路径。四模式的 fixture 结果不等价于四模式硬件实验成功。

## Goal 3 — NAS 初始化与 TR3000 就绪验收

使用 Matt `implement` 与 `tdd`。先确认目标为 `experiments`，且 Protocol V1 对象尚不存在或与当前 DDL 一致。对空目标使用仓库 `xpost-test/sql/protocol_v1.sql` 一次性初始化；遇到同名旧表、部分初始化或结构冲突时停止并报告，不 DROP/TRUNCATE、不重复盲目应用 DDL。初始化需要管理员授权：若实施者没有已经授权的适用数据库管理能力，提供维护者可在 pgAdmin 执行的准确 SQL/步骤，等待确认；不要取得管理员密码或变更现有角色定义。由管理员授予既有 `experiment_rw` 必要的 Schema USAGE、表 SELECT/INSERT 和 `bench.run` UPDATE（及实际 SQL 需要的其他最小权限），不授予任意建 Schema/删表权限。保留 `experiment_ro` 等角色已有政策；本轮不扩大其权限。

在真实 TR3000 上部署本轮 runner，利用既有 `all-preflight` 或等价的数据库只读验收入口，确认运行时读取 `/root/.db`，连接实际 `experiments`，核对 Protocol V1 的表、权限。若完整 preflight 另有非数据库前提阻塞，单独报告数据库验收与完整 preflight 状态，不能把缺少固件/Agent 等同于数据库失败，也不得通过烧录绕过。必要时在维护者授权下用回滚事务测试最小 INSERT/UPDATE/SELECT（不留下虚构实验数据）；隔离 fixture 已覆盖的写入逻辑不重复跑硬件。检查凭据文件元数据（不显示内容），记录数据库名称、登录角色、表数、缺失权限、测试退出状态和相关 commit。

[已完成连接测试](tests/tr3000-db-connect.md)仅覆盖 psql 连接及 SQL 执行，不能替代上述应用验收。若网络/账号/目标变化使旧证据失效，报告差异；不要求用户重新提供密码。

## 交付

按 `task-brief-to-pr` 的独立 worktree 流程实施；主代理亲自运行未覆盖的测试。采用 Matt `code-review` 核对规格和安全边界，采用 `pr` 在英文 PR 正文中记录实际检查结果、复用证据、未完成事项、旧入口处理清单、初始化与 preflight 状态。通过指定 GitHub MCP 创建开放 PR（不合并），不要用 gh 直连。没有真实运行证据时不宣称 NAS 已初始化、应用已连通或写入成功。待用户审核合入后，再由用户授权启动新数据采集。

## 澄清记录

- 2026-11-27：`lookup` 的开关对照仍使用旧 writer，Protocol V1 没有等价的写入入口。询问是否改为本地诊断文件，或退役入口。维护者答复：「退役 lookup 入口」。

- 2026-11-27：维护者要求 `xpost-test` 在 Protocol V1 表尚未部署时自动创建。仅在专用 `experiments` 数据库为空时初始化。只读预检和恢复采集不执行初始化。发布继续使用本地 Magpie MCP。
