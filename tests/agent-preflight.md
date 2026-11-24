# #23 的 Agent 主机预检与 HTTP 入口

本记录对应 #23 最新审阅 head `9e73d84a6ac248c90c6b1f55cab33335c27cecf5`。该 head 已包含 T3 的实现起点 `b473cf1` 和代码改动 `d803192`。

- PR 作者在 Mac 用 Agent CLI 首次下载并核验 12 个正式归档和 36 个部署容器。缓存重放按旧 profile 集合准备了 39 个槽位目标中的 37 个。 `just_tx` 准备字节摘要与发布件相同。 `bsort_jit` 的 10264 B 载荷能放入 12288 B SRAM 槽，完整镜像能放入 16384 B cold cell。这些是主机准备结果。
- `case_mark_idle`（AOT）和 `case_mark_idle_jit` 都在打开设备会话前被拒绝。v0.1.1 发布件链接地址为 `0x40380200`，B 槽为 `0x40381000`，无可用于证明准确改写的重定位信息。该拒绝是基线 Agent 的已知阻塞。之后的 [v0.1.1 case 双槽对照](case-v0.1.1.md) 确认 `case_mark` 的 AOT 载荷可原字节放到 B 地址。仍需接通 Agent 并在板上核对。39 是旧计划中的准备目标数，36 是已核验的发布容器数。
- 本轮按固定 head 静态核对了服务和客户端。Agent 的 `--bind` 运行 Axum 服务，`http.rs` 注册 `/health`、`/v1/preflight/medium` 与装载和采集路由。TR3000 的 `AgentClient::medium_preflight` 通过 reqwest 发 HTTP GET 并检查 JSON `ok`。服务启动时打开 USB，CLI `preflight` 不打开 USB。尚未从 TR3000 实测 HTTP 连通或板端装载。实际路径验收见 [任务书](../taskbook.md)。

[PR #23 实测报告](https://github.com/nan-mu/xpost/pull/23) · [固定审计记录](https://github.com/nan-mu/xpost/blob/9e73d84a6ac248c90c6b1f55cab33335c27cecf5/.audit/taskbook/mcp/xpost/a2f0eb1e847f1552bb58a0febeaa5877ea1ac3f1.md)
