# Zune (Keel) 逆向笔记

## 目标设备
- 连接于 USB `0x14200000`,`Microsoft Zune`,`VID 0x045e / PID 0x0710 / Rev 1.00`
- 序列号 `XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX`（已隐去）
- 设备类 `bDeviceClass=0xFF`(厂商私有),不走 UMS → 不挂载为磁盘;macOS 走 Image Capture/PTP

## 固件来源
- 仓库:`simulacra10/zunefirmware-upgrade`(`FirmwareUpdate.Xml` 中 **Keel** = `USB\Vid_045e&Pid_0710&Rev_0100`,固件 **3.3**)
- 文件:`KeelBaseline.Cab`(35,832,824 B,MS CAB,4 files)

## 解包流水线
```
KeelBaseline.Cab  --cabextract-->  EBoot.bin / nk.bin / recovery.bin / Games.cab
nk.bin (B000FF)   --zunextract-->  128 modules + 26 files
```
- 工具:`balcsida/zunextract`(Rust,处理 B000FF→NB0→CE XIP + CECompress/LZX)
- CE6 ROMHDR: virt_base `0x88200000`,romhdr `0xd81a60`,CPU `ARM Thumb (0x1c2)`,128 modules / 26 files
- PDB 路径泄露构建信息:`E:\pyxis\v3.3\platform\Keel\target\ARMV4I\retail\`

## WiFi 控制面(核心发现)
Wi-Fi 由 **ZNet** API 统一管理,`znet.dll` 导出 98 个 `ZNet*` 符号。
关键:**`MtpSvc.dll`(USB MTP 服务)直接调用 ZNet 的 WiFi 接口**
→ 即「通过 USB 配置 Zune WiFi」在协议上成立:MTP 命令 → MtpSvc → ZNet。

MtpSvc.dll 引用的 WiFi 相关 ZNet 接口:
- `ZNetSetWifiState` / `ZNetGetWifiState`
- `ZNetAddWifiProfile` / `ZNetSetWifiProfile` / `ZNetGetWifiProfile`
- `ZNetGetNetworkInfo` / `ZNetScanAndWaitForNetworks`
- `ZNetGetWifiCapability` / `ZNetTestConnectivity` / `ZNetPassthroughConnect`

znet.dll 全部 WiFi 接口(节选):
```
ZNetAddWifiProfile   ZNetGetWifiProfile   ZNetSetWifiProfile
ZNetGetWifiState     ZNetSetWifiState     ZNetGetWifiStrength
ZNetGetWifiCapability ZNetScanNetworks    ZNetPerformOneTimeScan
ZNetEnableVigorousScan ZNetIsVigorouslyScanning
ZNetConnectPeer      ZNetDisconnectPeers  ZNetGetNearbyPeers
ZNetEraseUserCredentials ZNetGetUserCredentials ZNetStoreUserCredentials
ZNetRequireWifiPower ZNetReleaseWifiPower
```
(其余为 Zune Cloud/Sync / ZNetCloud* 子系统)

## WiFi 硬件
- 驱动:`ks7010sd.dll`(KS7010 SDIO WiFi 芯片,`NdisReadNetworkAddress`)
- 芯片固件:`ks7010sd.rom`(87,124 B,ARM 代码,头 `1b0000ea ...`)
- 协议栈:`ndis.dll` / `ndisuio.dll` / `tcpstk.dll`

## 阻塞点(rea 分析)
`rea` 依赖外部分析后端;容器里都没有:
- Ghidra:Ghidra 12.1.x(需 `GHIDRA_INSTALL_DIR` + Java 21)
- Hopper:仅 macOS,容器不可用
- `rea doctor`:alpine = `unsupported_host`(rea 支持 macOS 12+/Ubuntu 24.04+ …)
- Docker Hub 被墙 → 拉不到 debian/ubuntu 底座(需走 rootfs / daemon 代理)

## 环境
- 主机:macOS 10.13.6 / x86_64;Node 20.19.5(MacPorts),rea 跑在容器
- 代理:V2rayU,SOCKS5 `127.0.0.1:1080`(出口 150.109.68.126)—— 已开启

## ZNet API 逆向(绕过 rea,用 objdump 直接做)
工具:`/opt/local/bin/llvm-objdump-mp-17 -d --arch-name=arm`,先用 PE patcher 清零
越界的 `DataDirectory[5]`(BaseReloc)才能过校验。

### ZNet 函数 = 极薄 RPC 桩
每个 `ZNet*`(znet.dll,98 个导出)在栈上拼一个请求结构,跳到统一分发器 `0x0332112c`:
```
请求结构:
  +0x00  u16   命令码
  +0x02  u16   pad
  +0x04  u32   ctx = *(0x01f0506c + 0xc)
  +0x08  u32   arg0
  +0x0c  u32   arg1
  +0x10  u32   arg2
  size  12 / 16 / 20  (按参数个数)
```
命令码:
```
 30 (0x1e) ZNetSetWifiState        31 (0x1f) ZNetSetPassthroughState
 43 (0x2b) ZNetSetWifiProfile      44 (0x2c) ZNetGetWifiProfile
 45 (0x2d) ZNetAddWifiProfile      48 (0x30) ZNetRequireWifiPower
 51 (0x33) (ZNetCloud...)
```
### 分发器 @0x0332112c
1. `handle = *(g+0)`;若 == -1 → `CreateFile(*(g+4), 0xC0000000, 3, ...)` 打开设备
2. `DeviceIoControl(handle, dwIoControlCode=*(g+8), lpInBuffer=请求结构, nInBufferSize=size, ...)`
3. `CloseHandle`
全局连接对象 `g = 0x01f0506c`:`{ +0 handle, +4 设备名, +8 IOCTL 码, +0xc ctx }`
(API 调用经 CE API-set 间接表 `0x01f05xxx`,故直接 IAT 搜不到)

### MTP ↔ WiFi 桥
- `MtpSvc.dll` = CE **流驱动**(导出 `MTP_Open/Read/Write/IOControl/Init/Deinit`)
- 导入 6 个 ZNet WiFi 函数 → **USB MTP 命令 → MtpSvc → ZNet 客户端桩 → DeviceIoControl → ZNet 设备**
- 即「通过 USB 配 WiFi」的通路:发 MTP 命令到 MtpSvc,触发 `ZNetSetWifiProfile`/`ZNetAddWifiProfile`/`ZNetSetWifiState`
- WiFi profile blob 的确切字段格式在 ZNet **服务端**(另一模块/设备),尚未解出

### 平台信息(ceconfig.h)
- 平台代号 **Pyxis**;模块:`MTP / MTPCLIENTDRVIP / MTPCLIENTDRVUSB / MTPSVC / ZDKNET / ZDKSYSTEM / ZNET`
- PDB 路径:`E:\pyxis\v3.3\platform\Keel\target\ARMV4I\retail\`

## MTP 命令面(MtpSvc.dll 反汇编)
`MtpSvc.dll` = CE 流驱动,导出 `MTP_Open/Close/IOControl/Init/Deinit`。

### `MTP_IOControl` 接受的 IOCTL
```
0x00000200, 0x10400004, 0x10400008, 0x1040000C, 0x10400020
```
只有 `0x200` 走真正的 MTP 处理(`0x3610828`)。

### MTP 操作码总派发(`0x36108f8` + `0x3611428`,比较 `[ctx+0x28]`)
标准 MTP:`0x1001 0x1002 0x1006 0x1007 0x1008 0x1009 0x100C 0x100D 0x1013 0x1015 0x1016 0x101b 0x107a`
厂商扩展:`0x6108`,`0x9102..0x9185`,`0x9204..0x9267`,`0x9801..0x9811`

### ★ WiFi 控制块:操作码 `0x9220–0x9229`
| Opcode | 处理函数 | 动作(ZNet) |
|---|---|---|
| `0x9222` | 0x3625234 | (读取类) |
| `0x9223` | 0x36252f0 | |
| `0x9224` | 0x362341c | |
| **`0x9225`** | 0x3623550 | **扫描网络** `ZNetScanAndWaitForNetworks` |
| `0x9226` | 0x36236a8 | |
| **`0x9227`** | 0x3623834 | **设置/新增 WiFi profile** `ZNetSetWifiProfile` + `ZNetAddWifiProfile` |
| **`0x9228`** | 0x36238e0 | **连接测试** `ZNetTestConnectivity`(+`ZNetPassthroughConnect`) |
| `0x9229` | 0x3614eac | |
| `0x9220/0x9221` | 0x3611c7c / 0x3611c68 | |

### WiFi profile 提交细节(0x3623834)
```
入参: ctx(r0), len(r1), out(r2)
  if len==0 or len>0x1000 → 错误 0x2002
  buf = [ctx+0x50]
  ZNetSetWifiProfile(0x3E8, buf, len)     ; 0x3E8=1000
  ZNetAddWifiProfile(0x3EF, buf, len)     ; 0x3EF=1007 (1004|3)
```
即:profile blob 放在 `ctx+0x50`,长度 ≤ 4096,由 MTP opcode **0x9227** 写入并触发。

### 完整通路
```
USB MTP (opcode 0x9227 + profile blob)
  → MtpSvc.dll  MTP_IOControl(0x200) → 0x3610828 → 派发器 0x3611428
  → 0x3623834 → ZNetSetWifiProfile/ZNetAddWifiProfile (tramp 0x3646b00/0x3646af0)
  → DeviceIoControl → ZNet 设备 → ks7010 SDIO WiFi
```

### trampoline 映射技巧
MtpSvc 走 CE API-set 间接表(`0x01f39xxx`),直接 IAT 搜不到。解法:
按大小把 trampoline 连续槽位组对齐到导入表 DLL(130→COREDLL,30→wmdrm,8→zam,
38→zmedia,**33→znet.dll**),再按导入顺序把 idx 映射到函数名 → 得到每个 ZNet 函数的 trampoline 地址。

## ★ ZNet 服务端定位(后续追进)
1. znet.dll(客户端)里含 **UTF-16 字符串 `ZSV0:`** → 它 `CreateFile("ZSV0:")`,
   即打开 **zserv.dll** 的 ZSV 流驱动设备。
2. `zserv.dll`(ZSV_IOControl)有**命令分派器**(`0x3257740`,`cmp r1,#43/44/45/48`)
   → `0x324e5d0`(按命令建对象并调用)→ `0x3258b54`/`0x3257c68`(命令处理)。
3. `zserv.dll` **唯一导入 `wzcsapi.dll`**:
   `WZCSetInterface / WZCQueryContext / WZCPassword2Key / WZCDeleteIntfObj / WZCSetContext`
   → WiFi 最终落到 **WZC(Wireless Zero Configuration)/ NDIS 802.11**。
4. `wlclient.dll` = WLAN 客户端库(`CE_Password2Key`、全套 `GetDot11*/SetDot11*`、802.1x supplicant);
   `wzcsvc.dll` = WZC 服务(`OID_802_11_SSID`,`ZCF_IOControl`);`ks7010sd.dll` = KS7010 NDIS 小端口。

### 最终完整链路(闭合)
```
USB MTP: opcode 0x9227 + profile blob
  → MtpSvc.dll  MTP_IOControl(0x200) → 0x3610828 → 0x3611428(opcode 0x9227) → 0x3623834
  → ZNet client znet.dll: CreateFile("ZSV0:") + DeviceIoControl(cmd 43/44/45)
  → zserv.dll  ZSV_IOControl → 命令分派 0x3257740 → (WZC 层)
  → wzcsapi.dll WZCSetInterface / WZCPassword2Key
  → wlclient.dll (NDIS 802.11 OID) → ndis/ndisuio → ks7010sd.dll → KS7010 SDIO WiFi
```
- WiFi profile = **WZC WLAN 配置 / NDIS_802_11 结构**(SSID、认证模式、WEP/WPA 密钥、BSS 类型…),
  由 MTP opcode 0x9227 原样透传:profile 缓冲放 `MTP ctx+0x50`,长度 ≤ 4096。
- 具体字段偏移 = WZC `WZC_WLAN_CONFIG`(内嵌 `NDIS_802_11_SSID` / `NDIS_802_11_CONFIGURATION` / 密钥)布局,
  待从 zserv 的 cmd-43 路径逐字段确认。




---

## Zune 桌面客户端(PC 端)逆向 — 2026-10-15

**来源**：archive.org `zunepackage_4.8.2345.0` → `ZunePackage.exe` (SFXCAB；真 CAB 在文件偏移 `0x8E00`，
`cbCabinet=0x10E7528D`) → `x86/packages/zune-x86.msi` (50 MB) → 内嵌 `Zune.cab` (49 MB) → **101 个 PE**。

工作目录 `~/zune/winclient/`：`bin/` 为按 PE 原名重命名后的客户端二进制，`rea/` 为 rea+Ghidra 证据 JSON。

| 模块 | 大小 | 作用 |
|---|---|---|
| `ZuneMTPZ.dll` | 199 KB | PC 侧 MTP 发起端（`CMtpDevice::*`：NewCommand / GetObjectInfo / BeginSendObject…），COM，4 导出 |
| `ZuneNativeLib.dll` | 7.3 MB | 协议/命令层，25,457 函数；**MTP opcode 表就在这里** |
| `ZuneWlanCfgSvc` | 434 KB | "Zune Wlan Configuration" COM 服务，CLSID `{BABB2B95-9545-47DA-973E-298F292607CC}`，ProgID `ZuneWlanCfg.ZuneWlanCfg`；仅导入 `CryptUnprotectData`(DPAPI) + AES("expand 16-byte k") |
| `ZuneUsbTransport.dll` | 127 KB | USB 传输（COM） |
| `ZuneIpTransport.dll` | 108 KB | IP 传输 → 无线同步 |
| `ZuneZMDB{Classic,Mobile,ZuneHD}.dll` | ~130 KB | 按机型分的媒体库 |

### ★ WiFi profile 提交（opcode 0x9227）— 记录尺寸确认

`ZuneNativeLib.dll` 中 opcode 立即数（VA = ImageBase `0x6a740000` + RVA；PE 里 `.text` RAW=`0x400`/VA=`0x1000`，
故 **VA = ImageBase + (file_off − 0x400 + 0x1000)**）：

| opcode | VA | 位置 |
|---|---|---|
| `0x9227` | `0x6A7AB447` | 函数 `FUN_6a7ab402` |
| `0x9225` | `0x6A9BFEB3` | —（sky: `ZuneNSSCIDLL`/`ZuneDriver.dll` 也各有一处 `0x9226`）|
| `0x9228` | `0x6A97EDF1` | — |
| `0x9102` | `0x6A9B81C7` | — |

发送函数（Ghidra 伪码）：
```c
int FUN_6a7ab402(undefined4 *param_1)          // param_1 = profile 结构
{
  FUN_6a7ab3ba(4, *param_1, 0x140, &local_8);  // 按 (*param_1) * 0x140 分配缓冲
  (**(code **)(*in_ECX + 0x24))(*in_ECX, 0x9227, param_1, local_8);  // 提交 MTP 0x9227
}
```
调用链：`FUN_6a7ab477` → `FUN_6a7ab4e9(&struct)` → `FUN_6a7ab3ba`(分配) → `FUN_6a7ab402`(发 0x9227)。

**结论**：PC 客户端提交的 WiFi profile blob = **N × 0x140（320）字节**的记录数组，N 取自调用方结构首字段。
与固件 `MtpSvc.dll` 0x3623834 的 `len ≤ 0x1000` 校验一致，固件把该缓冲原样交给
`ZNetSetWifiProfile(1000, buf, len)` / `ZNetAddWifiProfile(1007, buf, len)`。

**下一步**：解 320 字节记录内部布局（SSID / 认证模式 / WEP-WPA 密钥 / BSS），入口在 `FUN_6a7ab4e9`
（结构来源）与 `ZuneWlanCfgSvc`（已用 Ghidra 分析：634 函数 / 290 字符串，尚未细读）。

---

## ★ WiFi 无线连接实测成功（2026-10-17 凌晨，macOS）—— 无线同步未竟

### 结论
**Zune 30 已能通过 USB 配置 WiFi 并自动连网**（SSID `<路由器SSID>`，WPA-PSK/TKIP，IP `192.168.x.x`）。
无线同步（PTP/IP）差最后一步：设备监听 TCP 15740 但对初始化回 `INIT_FAIL`。

### 工具链（zune-rs 新增，实测通过）
- `zune wifi connect --ssid X --password Y [--flags a,b,c]`（0x9224→0x9227→0x9228×5）
- `zune wireless setup --ssid X --password Y --name <PC名>`（Phase 1+2 配对 + profile + 0x9230）
- `zune wireless status` / `zune wifi op <hex> [args]`（带数据载荷的事务已支持）
- 代码：`zune-rs/src/mtpz.rs`（`find_device_modulus` / `encrypt_wifi_password`）、
  `device.rs`（`wireless_pair` / `wireless_enable` / `build_wifi_profile`）、`main.rs`

### profile 记录布局（写入 324B = 4B profileId + 320B 记录；0x9226 读回是去掉前缀的 320B）
```
+0x04 u32 ssidLen  +0x08 SSID(32, NUL 填充)
+0x28 u32 flags[0]=1  +0x2c auth  +0x30 cipher  +0x34 =1(常量)  +0x38/+0x3c =0
+0x40 u32 密文长度=128  +0x44 RSA-1024(PKCS#1v1.5, UTF-16LE 明文)  +0xc4 零填充到 324
```
- flags 用 NDIS 枚举：auth 4=WPA-PSK / 7=WPA2-PSK；cipher 2=TKIP / 4=AES-CCMP / 0=无
- **密码用“设备自己的 RSA 公钥”加密**：公钥在 MTPZ 握手解密 payload 里（marker `01 00 80` + 128B 小端模数，低字节奇数）
- **扫描记录（0x9225，56B/条）尾部三元组 ≠ 安全类型**（校准实验：同一 AP 从 WPA2/AES 改为 WPA/TKIP 后扫描值不变）→ flags 直接按目标网络安全类型填

### 关键实测数据
- **设备 WiFi MAC：`00:17:FA:XX:XX:XX`**（微软 OUI；来自属性 `0xD21F` = `uuid:00000000-0000-0000-ffff-0017faXXXXXX`）
- 设备 GUID（`0xD221`）= `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`（=序列号，已隐去）；网络状态属性 `0xD217` = 7459
- 路由器实测：`active=1`、DHCP hostname = 写入 `0xD402` 的值；profile 密文**“使用后清零”**（写入后读回在，设备尝试连接后变全零）
- `0x9224` 返回 **DATA 容器**（非 RESPONSE）；`0x9227` 探测（4B 零）返回 `0x2002`，与 AFTL 抓包一致

### PTP/IP（无线同步）现状 —— 未完成
- 设备联网后**监听 TCP 15740**；PC 主动连 + Init Command Request → **`INIT_FAIL` reason=1**（libgphoto2 注：permission denied 类）
- 已试变体：Initiator GUID = 设备 GUID / PC GUID / 全零 / 随机 × 字符串序 / LE 字节序 × name 带/不带长度字节 —— 均被拒或超时
- XuneSyncLibrary 的 `ConnectWireless()` 是**空实现**，其 ptpip_client 从未验证 → 无可靠参考
- 抓包：设备上线只发 **Gratuitous ARP Announcement**（约 4 次）后完全沉默（不查名字/不广播）；PC 每秒高频重连时设备不回 SYN-ACK（**需温和重试**：每 10s、连上保持）
- 设备周期（实测）：连上 ~30–90 秒 → 找不到同步 PC → 断开 → 重试

### 下一步（若继续）
1. 固件里定位监听 15740 的模块（端口为数值常量需反汇编；`fw/modules/modules/` 已解包）
2. 或逆向 Zune Desktop 的 `ZuneIpTransport.dll`（官方 PC 侧 PTP/IP）
3. 核对 `0xD230` blob（`7a 24 ec 12 …`，疑似时间戳/配对 ID）与“配对有效期”检查
