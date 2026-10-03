# just_drop vs just_drop_hal PPS

对比 **XDP ingress drop**（`examples/just_drop`）与 **HAL 应用层 drop**（`examples/just_drop_hal`）在同等 UDP 洪泛下的 DUT 处理 PPS。

数据一律经 `utils.db` **现查 Postgres**（`benchmark/.env`），不落本地结果缓存。分析方法见仓库根目录 [`xpost_hal_xdp_experiment_plan.md`](../xpost_hal_xdp_experiment_plan.md)。

---

## 复现实验流程

### 1. 准备

```bash
cd benchmark
cp .env.example .env   # 填 PG* 账号
pixi install
pixi run ensure-schema
```

固件与 agent（见 [`AGENTS.md`](../AGENTS.md)）：

```bash
# 终端 A：agent RPC
cargo xtask agent --listen 0.0.0.0:9240 --no-monitor

# 烧录 just_drop / just_drop_hal（按轮次切换）；Wi-Fi 凭据在各自 examples/*/.env
cargo xtask just_drop build --release
cargo xtask just_drop_hal build --release
```

### 2. 采集（临时 CSV，不进仓库）

在 `xpost-test` 侧（TR3000 / 已配置的 probe 环境）：

```bash
./probe just-drop-pps --path hal --out /tmp/hal_pps.csv
./probe just-drop-pps --path xdp --out /tmp/xdp_pps.csv
```

探针会旁路写出 `/tmp/hal_pps_summary.json`、`/tmp/xdp_pps_summary.json`。

> 当前探针是等间隔短窗口采样，入库后记为 `run_kind=pilot`，**不得并入正式置信区间**。规范要求的随机三点 `τ`、擦除重烧配对块、15 分钟窗口需后续采集升级。

### 3. 入库

```bash
cd benchmark
pixi run ingest-pps -- /tmp/hal_pps.csv /tmp/hal_pps_summary.json
pixi run ingest-pps -- /tmp/xdp_pps.csv /tmp/xdp_pps_summary.json
```

同一 CSV 再导入会按 `source_note` 替换先前的 pilot 行。

### 4. 分析

打开本笔记本（`pixi run lab`），向下执行：只读 `experiment_*`，画图 1 / 图 2，并标明证据边界。
