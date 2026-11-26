# XPOST 用户决策

记录维护者已确认的跨任务设计决定。旧实验和部署决定见开发仓库 [PR #23 的 docs/user-decisions.md](https://github.com/nan-mu/xpost/blob/5e435e083292cb7cb5d592c9406fdf76c791ee92/docs/user-decisions.md) 及对应任务书；本文件不修改那些决定。

## agent-http-cli · 2026-11-25

xpost-agent 采用 HTTP + CLI，完全删除 MCP。Daemon 和 CLI 位于同一个现有 Cargo host workspace 中，共用唯一 xpost-agent 二进制。实验 runner 继续通过 HTTP 访问 Agent。来源：PR27 设计会话，用户选择方案 D 并明确不创建新的可执行工作区。

## agent-on-demand · 2026-11-25

Agent 按需启动，实验或调试期间持续运行，结束后退出。Cargo 负责构建和启动前台 HTTP 服务。执行会话维持服务进程；不需要 Agent 自身的后台常驻管理、start/stop/restart/logs、Unix socket 生命周期协议、PID/实例持久化或运行映像一致性校验。保留设备 CLI、HTTP、设备独占及安全退出保护。取代 PR27 初版任务书的后台生命周期要求。理由：频繁重新编译时直接使用 Cargo 即可，不需要独立重启机制；无使用需求时不常驻。来源：PR27 第二轮审核，用户确认生命周期简化。

## agent-health-board-info · 2026-11-25

Agent 启动时通过 espflash Board Info 确认目标板可访问。成功是硬件可访问性的验收标准；失败不开放健康服务。不要在实验期间因 HTTP health 查询重复进入下载模式。不要增加 USB 权限、异常设备、物理重连或烧录等其他硬件探索与验收。来源：PR27 第二轮审核，用户指定 Board Info 并确认简化设计。

## xdp-clear-explicit · 2026-11-25

只有显式传入 "*" 才允许清除整个 XDP 冷存储区。空名称、空白名称、无效列表必须在写入前被拒绝。来源：PR27 审核问题 1，维护者同意修复。

## agent-experiment-boundary · 2026-11-25

按 PR23 的中量任务书核对 Agent 功能与接口。Agent 负责设备访问、部署及采集控制；xpost-test 负责实验调度、流量、数据校验、入库和结果。保留 PR23 已定的 29 格计划及一次 AOT Trace 热切换，不将性能实验实施混入 PR27。本次用户要求功能审核并更新 PR27 任务书；未授权合并 PR 或改写其他任务分支。

## agent-pr27-rebase · 2026-11-25

维护者要求 PR27 先 rebase 到 PR23 的提交，再修复第二轮审核的问题。替代先前 PR27 独立基于 main、将接口整合推迟至后续的任务安排。保留 PR23 已有实验、协议及证据；PR27 的增量限于 Agent 改造及必要的接口适配。本决定只适用于 PR27 的开发起点和整合顺序，不授权合并或改写 PR23，也不授权 PR27 启动正式实验。来源：本轮用户明确要求“先rebase PR23的提交。然后修复这些问题”。

## runner-db-runtime-config · 2026-11-27

用户决定停止历史实验数据迁移；保留 NAS `postgres` 的历史数据，新采集使用 NAS `experiments`。TR3000 上的 `xpost-test` 在运行时从固定 `/root/.db` 读取 PostgreSQL 地址、端口、用户名、密码和数据库名，凭据不再嵌入编译产物。用户已手动配置该文件并通过 psql 连接 `experiments`，角色为 `experiment_rw`。来源：本次数据库迁移与连接重构会话；适用于后续实验 runner 与数据库配置，不授权清空旧数据或启动正式实验。

## runner-schema-reconcile · 2026-11-27

PR41：连接目标由 /root/.db 指定，不追加全局数据库名硬编码。新采集逐表检查 Protocol V1：缺表创建，字段一致保留表和数据，字段不一致删除该表及数据并重建；不设计版本迁移。保留其他兼容表及数据，外键或其他依赖无法安全恢复时回滚并报告，由用户处理。只读 preflight、resume 和 reconnect 不做破坏性调整。来源：PR41 审核会话，用户明确接受上述冲突处理。
