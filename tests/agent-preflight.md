# #23 的 Agent 主机预检与 HTTP 入口

本记录对应 #23 最新审阅 head `9e73d84a6ac248c90c6b1f55cab33335c27cecf5`。该 head 已包含 T3 的实现起点 `b473cf1` 和代码改动 `d803192`。

- PR 作者在 Mac 用 Agent CLI 首次下载并核验 12 个正式归档和 36 个部署容器。缓存重放按旧 profile 集合准备了 39 个槽位目标中的 37 个。 `just_tx` 准备字节摘要与发布件相同。 `bsort_jit` 的 10264 B 载荷能放入 12288 B SRAM 槽，完整镜像能放入 16384 B cold cell。这些是主机准备结果。
- `case_mark_idle`（AOT）和 `case_mark_idle_jit` 都在打开设备会话前被拒绝。v0.1.1 发布件链接地址为 `0x40380200`，B 槽为 `0x40381000`，无可用于证明准确改写的重定位信息。该拒绝是基线 Agent 的已知阻塞。之后的 [v0.1.1 case 双槽对照](case-v0.1.1.md) 确认 `case_mark` 的 AOT 载荷可原字节放到 B 地址。仍需接通 Agent 并在板上核对。39 是旧计划中的准备目标数，36 是已核验的发布容器数。
- 本轮按固定 head 静态核对了服务和客户端。Agent 的 `--bind` 运行 Axum 服务，`http.rs` 注册 `/health`、`/v1/preflight/medium` 与装载和采集路由。TR3000 的 `AgentClient::medium_preflight` 通过 reqwest 发 HTTP GET 并检查 JSON `ok`。服务启动时打开 USB，CLI `preflight` 不打开 USB。尚未从 TR3000 实测 HTTP 连通或板端装载。实际路径验收见 [任务书](../taskbook.md)。

[PR #23 实测报告](https://github.com/nan-mu/xpost/pull/23) · [固定审计记录](https://github.com/nan-mu/xpost/blob/9e73d84a6ac248c90c6b1f55cab33335c27cecf5/.audit/taskbook/mcp/xpost/a2f0eb1e847f1552bb58a0febeaa5877ea1ac3f1.md)


## 5e435e0 复验

PR 作者在 Mac 上用 Agent CLI 核对了 36/36 发布部署。中量主机预检核对了 32/32 所需程序×模式、41/41 XDP ELF 目标和四份 HAL 固件。两份 case AOT 的载荷摘要都匹配双槽对照。B 的 `case_mark_idle` 容器目标地址为 `0x40381000`。Linux runner 的 `medium-plan` 预览输出 29 格。合成 PostgreSQL 夹具查到了同一 run 的一条切换、八个 Trace 事件和两份原始证据。

此次复验期间 Mac Agent 未监听。TR3000 HTTP、真机 A/B 和中量采集待本轮验证。复用主机和夹具结果，不将其计入 29 格实测。[PR 作者的复验记录](https://github.com/nan-mu/xpost/blob/5e435e083292cb7cb5d592c9406fdf76c791ee92/.audit/taskbook/mcp/xpost/20ab29f20b9d1fae43055af58b6bd5fe7eb6a890.md)
