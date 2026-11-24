# libbpf-mcu v0.1.1 正式附件检查

任务书编写端在 Linux 下载 GitHub Release 的 12 个原始附件。Git tag 解析到 PR6 合入提交 `07a4ea9a81a7fedf7bef2d6ced8d7fcfcccfd113`，PR6 验收 head 为 `0af35050d9951bac0231e2f54a5577bcbfd2be55`。以 GitHub asset digest 为独立预期执行 `sha256sum -c`，12 项均 OK。用 libarchive 7z reader 解包，按独立程序矩阵比对每个包的成员列表和八个常规文件。程序为 firewall、just_drop、just_tx、noop、prime、log2、copy、fletcher32、bsort、strcmp、case_swap、case_mark。目录项不计入文件数。

从该 tag 的 `make/layout.py::load_index` 检查程序、角色、成员路径、文件长度、SHA-256 与 Compact ELF 格式。与独立的 `llvm-aot/ebpf/jit` 集合比对，每包确有三种模式。再用 `make/xpost_elf.py::parse_container` 核对程序名、地址和镜像，用系统 `readelf -SW` 查看 ELF section。实际 **12/12 × 3，36/36**，每个容器槽地址均为 0x40380200，helper 为 0x40380000，无 ELF relocation section。与所附 ET_REL 对象不同。正式 `v0.1.0` 的 0/36 仅是历史缺件反例。2026-11-25 复核 Release 仍指向 v0.1.1，12 项 GitHub asset digest 与下方记录逐项一致。使用最新 digest 重新校验已有 7z 字节，全数通过。此次复核复用原归档结构和 ELF 检测结果。

## 镜像与容量

| JIT | 实际 payload B | 实际容器 B | 对 12,288 B exec SRAM 的余量 B |
| --- | ---: | ---: | ---: |
| log2 | 9300 | 9596 | 2988 |
| bsort | 10264 | 10560 | 2024 |
| strcmp | 9348 | 9644 | 2940 |

容量依据是 [XPOST #25 的实际链接固件记录](main-integration.md#容量与编码)。三者完整文件均小于该固件的 16,384 B cold cell。该比较覆盖发布字节与该已测布局的尺寸约束。当前 PR #23 的设备身份、Agent 正常下载和刷写、EXB2 板端行为、原生异址定址及热切换仍按正文验收。

代码审查核对 `release_inputs` 读取调用者提供的八个固件 ELF，`verify_batch` 检查独立矩阵。生产端 `retarget` 从归档对象重建 AOT/JIT，作为直接构建对照。已检查的正式附件摘要如下，以这些摘要复用本次下载与清点结果。无需再次为归档清点单独下载或重跑旧检查器。Agent 的空缓存场景仍从正常生产入口执行。[正式 Release](https://github.com/nan-mu/libbpf-mcu/releases/tag/v0.1.1) · [PR6](https://github.com/nan-mu/libbpf-mcu/pull/6)

```text
e674f13bd6f2726922334e3058a5f106032d62bd94b040c2dfe0f3ca02119d5c  bsort.7z
a94b68dd0461ad49e491756c56479c06a537b9e37904185dfaba8df1001c4881  case_mark.7z
f6244ceb52afb1bdfff818ce49c6e77c3f8fa881831c95da1d0ed2a8b7d29406  case_swap.7z
6b352e225d68af4ab04334ac7d666913ac5b396429027735af3ab4a6d9e0900e  copy.7z
72980d4d48058dd6209805ecdf3f3135baa39fff342e35b026ca8f007cdd2ea3  firewall.7z
aef8d3499bb45b5b29ae9d1c1b60745c1e7a8dfde835128cdf163da71c4e817e  fletcher32.7z
b6273506fa2e3e90bb4d9b4a610930f863212aa5aceae16c7621f45e9dbfb209  just_drop.7z
4288a9611cd12b3759c9d1256304a68d855ce3df7e13ad9ffb564fe8a3fe34a8  just_tx.7z
c33c0e5dfaf5437f9abde138f900235dfc78fde538e1c26e8d5ea348f14bd950  log2.7z
e288c29d5300357d87a5a5d8706a4eda750cf08a66b47de442fe4c17fadae30c  noop.7z
69102a43933888e96a7722e355475b14ac30bd70f718adf688efbef5ac055beb  prime.7z
710e1a699c1c3c984df16f939d23c9f182e808a4c95660a6fc194999c6986730  strcmp.7z
```
