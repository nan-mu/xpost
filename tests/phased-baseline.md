# 分阶段复测前的证据

## 前台 Agent

GitHub PR #27 的 `540d1d4` 已合入 PR #23 的 `b824c58734fc4e1585ab195c22a196f0debbb10e`。用提交父节点及合并说明核对。#27 最终增量已基于 #23 的 `22f19e2`。合入树保留 HTTP 预检、部署身份、EXB2 与 Perf 路由，服务入口为 `cargo run --release --locked -- serve`。[合并提交](https://github.com/nan-mu/xpost/commit/b824c58734fc4e1585ab195c22a196f0debbb10e) · [合入树路由](https://github.com/nan-mu/xpost/blob/b824c58/xpost-agent/src/http.rs#L254-L293)

#27 的记录覆盖真机 Board Info、恢复 App 模式、前台 HTTP 健康检查和 SIGINT 退出。主机夹具覆盖固定 v0.1.1 产物、36 个部署、EXB2 8258 B、两类 Perf 分块和写入前摘要拒绝。复用这些入口及独立期望。该记录中的部署与采集验证是夹具；本轮需经正式 runner 采集和按 run_id 查库。[#27 验收记录](https://github.com/nan-mu/xpost/blob/b824c58/.audit/taskbook/mcp/xpost/aa8b7b2a159d0eb8fb977f91018544a9e1770a3e.md)

## 问题证据

| 已观察的问题 | 已有验证及边界 |
| --- | --- |
| [#28](https://github.com/nan-mu/xpost/issues/28)、[#29](https://github.com/nan-mu/xpost/issues/29) | 400 pps 原窗口 `9ade0352-3971-4c3d-a297-d20e48ad4a27` 只有主机发包。补齐 Wi-Fi 输入后重试 `75088f39-6265-4d86-a803-cc27b1fd8c7f` 得到 199/200 合法交付和 1790/1800 目标键命中。已补两类流量均无 DUT 证据的判据；缺构建输入仍须在写入前识别。 |
| [#30](https://github.com/nan-mu/xpost/issues/30) | 原 AOT 窗口目标键命中 1800，其他键 5 次。已用原快照重分析合格的 AOT/JIT 窗口。 |
| [#31](https://github.com/nan-mu/xpost/issues/31)、[#32](https://github.com/nan-mu/xpost/issues/32) | AOT/JIT 有归属到探针的终点 Trace。rBPF DROP 有 20000 次标签入口、20000 次正常 DROP、0 次故障的板端记录。 |
| [#33](https://github.com/nan-mu/xpost/issues/33)–[#35](https://github.com/nan-mu/xpost/issues/35) | rBPF 冷加载先修 map 索引，再修实际 map value 访问。已有 10 个发布程序的冷加载主机核对和合法 value 对照。Firewall `c6d8a254-f53c-4a8b-827e-a2dcf6a42082` 是带 1800 次标签故障的旧观察。Just TX `b54bc345-dfc8-4c5b-9a59-c0b8f7222d5d` 也需重判。 |
| [#36](https://github.com/nan-mu/xpost/issues/36) | 延长抓包导出等待后，AOT 和 JIT 的 1400、4000 pps 及单探针重跑有合格证据。原抓包缺失的窗口保持不可判定。 |
| [#38](https://github.com/nan-mu/xpost/issues/38)、[#39](https://github.com/nan-mu/xpost/issues/39) | 串口回归覆盖同会话读回、损坏表和多镜像表的拒绝。#23 原 campaign 有 AOT/JIT/rBPF 冷安装记录。 |

以上数据来自 issue 与 PR 记录，可按运行 ID 复查。旧版结果留作诊断，当前两阶段的实测使用合入后的 Agent。[原 campaign](https://github.com/nan-mu/xpost/pull/23)

## HAL Just TX

- 400 pps `273b6511-3974-44e3-b407-c0bc41018570`。2000 条提交与 TX 记录，1 条校验失败的 RX。
- 1400 pps `c63a96c5-8ff0-4249-8655-101aac068b26`。6971 条提交与 TX 记录，8 条校验失败的 RX。
- 400 pps `cc223cd6-7291-4c9b-a6e7-f80be4bc4fca`。2000 条有效回复，作为原始对照。

异常窗口的 tcpdump 内核丢包为零。静态核对显示 HAL 回送收到的 payload，runner 同时检查全帧长度和 payload。因此长度差可令两项判据同时失败。根因尚待原始 PCAP 对照。使用协议库 `medium_run.sql` 获取每个 run 的原始 PCAP artifact，再按标签和序号比较帧。此处沿用 [#37 的观测](https://github.com/nan-mu/xpost/issues/37)，未直接获取生产库 PCAP。[HAL 回送](https://github.com/nan-mu/xpost/blob/b824c58/examples/just_tx_hal/src/bin/main.rs#L170-L205) · [接收校验](https://github.com/nan-mu/xpost/blob/b824c58/xpost-test/common/src/exam/medium_network_capture.rs#L237-L268)
