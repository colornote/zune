# zune — Microsoft Zune (Keel / Pyxis) 逆向 + 同步工具集

面向 Microsoft Zune 30（型号 1089，固件 03.30）与 Zune HD 固件的逆向工程与同步工具集合。

- **设备**：`VID 0x045e / PID 0x0710 / Rev 1.00`，厂商私有类 `0xFF`，不走 UMS，macOS 走 PTP（Image Capture）
- **平台**：Windows CE 6.0（平台代号 **Pyxis**，目标 **Keel**）
- **状态**（2026-09-12）：`zune-cli` 已在真机 Zune 30 上端到端验证（音乐 / 专辑 / 照片 / 视频）

---

## 目录结构

| 路径 | 说明 |
|---|---|
| `RE_NOTES.md` | ★ ZNet / MTP 逆向主笔记（WiFi 控制通路、命令码、链路闭合） |
| `KeelBaseline.Cab` | 官方固件容器（3.3，35,832,824 B，MS CAB，含 4 个文件） |
| `fw/` | 从 CAB 解出的固件（`EBoot.bin` / `nk.bin` / `recovery.bin` / `Games.cab` / `nk.rom` / `modules/`） |
| `zfw/` | Zune HD 固件相关（CE6 `nk` 等） |
| `zunextract/` | Rust 工具：CAB → B000FF BIN → NB0 → CE XIP ROM 解包器（含 TUI 浏览器） |
| `zune-cli/` | 纯 Python：从 macOS 同步音乐/视频/照片/播放列表到 Zune 30 |
| `lytools/` | CE ROMHDR 解析 / 解压缩辅助脚本 |
| `ptp.c` / `mtpsvc_dis.txt` | MTP/PTP 与 MtpSvc.dll 的反汇编与分析资料 |
| `mtpz.c` / `mtpz.h` / `mtpz_test*.py` | MTPZ 安全握手实现与测试 |
| `znet_dis.txt` / `znet_exports.txt` / `znet_tramp.json` | znet.dll 反汇编、导出表、trampoline 地址映射 |
| `mtp_demo.py` / `mtp_probe.py` | MTP 协议探测脚本 |
| `Dockerfile` | 分析用容器环境 |

> 打包/归档时排除 `zunextract/target/`（Rust 构建产物，约 128M）、各子仓库的 `.git/`、`.DS_Store`。

---

## 固件解包流水线

```
KeelBaseline.Cab  --cabextract-->  EBoot.bin / nk.bin / recovery.bin / Games.cab
nk.bin (B000FF)   --zunextract-->  128 modules + 26 files
```

- CE6 ROMHDR：virt_base `0x88200000`，romhdr `0xd81a60`，CPU `ARM Thumb (0x1c2)`
- PDB 泄露构建路径：`E:\pyxis\v3.3\platform\Keel\target\ARMV4I\retail\`

`zunextract`（Rust）用法：

```bash
zunextract extract firmware.cab -o output/   # 自动识别 CAB/BIN/NB0，全量解包
zunextract info firmware.cab                 # 显示固件信息
zunextract explore firmware.cab              # 交互式 TUI 浏览器
```

---

## 核心发现：通过 USB 配置 Zune WiFi（通路闭合）

Wi-Fi 由 **ZNet** API 统一管理（`znet.dll` 导出 98 个 `ZNet*` 符号）。

```
USB MTP: opcode 0x9227 + profile blob
  → MtpSvc.dll  MTP_IOControl(0x200) → 0x3610828 → 0x3611428(opcode 0x9227) → 0x3623834
  → ZNet client znet.dll: CreateFile("ZSV0:") + DeviceIoControl(cmd 43/44/45)
  → zserv.dll  ZSV_IOControl → 命令分派 0x3257740 → (WZC 层)
  → wzcsapi.dll WZCSetInterface / WZCPassword2Key
  → wlclient.dll (NDIS 802.11 OID) → ndis/ndisuio → ks7010sd.dll → KS7010 SDIO WiFi
```

**WiFi 控制块：MTP 操作码 `0x9220–0x9229`**

| Opcode | 处理函数 | 动作（ZNet） |
|---|---|---|
| `0x9225` | 0x3623550 | 扫描网络 `ZNetScanAndWaitForNetworks` |
| `0x9227` | 0x3623834 | 设置/新增 WiFi profile `ZNetSetWifiProfile` + `ZNetAddWifiProfile` |
| `0x9228` | 0x36238e0 | 连接测试 `ZNetTestConnectivity`（+`ZNetPassthroughConnect`） |

- profile blob 放在 `MTP ctx+0x50`，长度 ≤ 4096，由 opcode `0x9227` 写入并触发
- profile 格式 = **WZC `WZC_WLAN_CONFIG` / `NDIS_802_11` 结构**（SSID、认证、WEP/WPA 密钥、BSS 类型…），字段偏移待从 zserv cmd-43 路径逐字段确认
- `ZNet` 函数是极薄 RPC 桩，统一分发器 `0x0332112c` → `DeviceIoControl`

详见 `RE_NOTES.md`。

---

## zune-cli — macOS 同步工具

纯 Python，无需 Electron / npm / Windows VM。

> **状态（2026-09-12）**：真机 Zune 30（型号 1089，固件 03.30）端到端验证通过
> - ✅ 音乐 — MP3（WAV/FLAC/M4A 自动转 MP3），设备显示 title/artist/track
> - ✅ 专辑 — 抽象专辑 + 播放列表，出现在 Music → Albums
> - ✅ 照片 — JPEG/PNG/HEIC，适配 320×240 屏
> - ✅ 视频 — MP4/MOV/MKV/AVI 转 320×240 WMV
> - ⚠️ 设备字体无 CJK 字形，标题请用拉丁字符

依赖：Homebrew、`python ffmpeg libusb`、MTPZ 密钥文件 `.mtpz-data`（来自 `libmtp-zune`，所有 Zune 通用）。

> **macOS USB 权限**：访问 Zune bulk 端点需在「系统设置 → 隐私与安全性 → USB」放行终端，或用 `sudo`，否则写入被静默丢弃、读取超时。

```bash
brew install python ffmpeg libusb
cd zune-cli
python3 -m venv .venv && source .venv/bin/activate
pip install -r requirements.txt
```

---

## 环境与阻塞点

- 主机：macOS 10.13.6 / x86_64；Node 20.19.5（MacPorts）
- 代理：V2rayU，SOCKS5 `127.0.0.1:1080`
- `rea` 依赖外部分析后端（Ghidra 12.1.x + Java 21 / Hopper），容器内不可用 → 已改用 `llvm-objdump-mp-17 -d --arch-name=arm` 直接反汇编

---

## 归档 / 部署

打包（排除构建产物与 git 元数据）并上传到 **QydStag**：

```bash
# 打包
cd ~/zune
tar czf ~/zune.tar.gz \
  --exclude='zunextract/target' --exclude='.git' --exclude='.DS_Store' .

# 上传（QydStag 见 ~/.lssh.conf）
ssh -p 10569 root@120.77.79.121 'mkdir -p /root/upload/zune'
scp -P 10569 ~/zune.tar.gz root@120.77.79.121:/root/upload/zune/
```
