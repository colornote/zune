# macOS (Apple Silicon) 实机测试反馈

来源：Mac 端的 pi agent，通过 mac 隧道提交
时间：2026-10-15 20:2x CST
被测产物：`dist/zune-aarch64-apple-darwin.tar.gz` (19:20 构建, 924K)
测试机：Mac16,8 / Apple M4 Pro / macOS (arm64)
设备：Zune 30, serial `XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX`（已隐去）

---

## 🔴 P0（阻塞）：macOS 上无法建立 MTP 会话

```
» Zune 30 connected (serial XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX)
Error: opening MTP session
Caused by:
    0: bulk OUT failed
    1: unknown (error 0xe00002ed)
```

错误码已从本机 SDK 头文件确证
(`/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/.../IOKit.framework/Headers/IOReturn.h:149`):

```c
#define kIOReturnNotResponding   iokit_common_err(0x2ed) // device not responding
```

### 可疑代码：`src/usb.rs:94-96`

```rust
// Clear a stall inherited from an interrupted session.
let _ = ep_out.clear_halt().wait();
let _ = ep_in.clear_halt().wait();
```

`clear_halt()` 的错误被 `let _ =` 完全吞掉 —— macOS 上失败时上层拿不到任何原因。

### 分析

1. 设备枚举本身完全正常：`ioreg` 显示 `IOUSBHostDevice ... active`，`UsbLinkSpeed = 480000000`（High Speed），
   VID/PID `045e:0710`(=1118/1808) 正确，无其他进程占用。
2. **macOS 的 IOUSBHost 下，当端点已处于 stall 时，`clear_halt()` 自身就会失败**
   （CLEAR_FEATURE(ENDPOINT_HALT) 控制传输未被设备响应 → `kIOReturnNotResponding`）。
   Linux/libusb 路径下同一流程通常能成功，所以这个坑只在 macOS 上暴露。
3. 失败状态会累积：19:12 版二进制报 `bulk OUT timed out after 10s`，
   19:19 版报 `0xe00002ed` —— 说明每次失败尝试都会让端点状态更僵。

### 建议修复方向（按优先级）

1. **不要再吞错误**。至少：
   ```rust
   if let Err(e) = ep_out.clear_halt().wait() {
       eprintln!("warning: bulk OUT clear_halt failed: {e:?}");
   }
   ```
   失败时给出可操作提示（"请重新插拔设备后重试"），而不是让后续 write 报一个无从下手的错误。
2. macOS 上考虑在 claim 之后先做 **device reset**（IOUSBHostDevice ResetDevice / 重新枚举语义），
   再 clear_halt；或 `clear_halt` 失败时降级为 reset。
3. 或调整次序：先发一个短探测写，失败后再 clear_halt 重试一次。
4. 核对 nusb macOS 后端对 stall 的处理；确认 clear_halt 是在 endpoint 创建后、首次 I/O 前执行的最优位置。

### 硬件拓扑（可能相关，建议一并排除）

Zune 当前挂在**扩展坞的嵌套 hub** 上：

```
AppleT8132USBXHCI
 └─ USB2.1 Hub           (扩展坞)
     ├─ USB 2.0 Hub       (二级 hub)
     │   ├─ BillBoard Device
     │   └─ Zune@00120000  ← 设备在此
```

建议复测时把设备直连 Mac 机身 USB 口，排除 hub 引入的因素。

---

## 🟡 P1：dist 产物落后于源码

| 文件 | 时间 |
|---|---|
| `dist/zune-aarch64-apple-darwin` | 19:19 |
| `target/aarch64-apple-darwin/release/zune` | 19:19 |
| `src/library.rs` | **20:19** |
| `src/media.rs` | **20:19** |

`library.rs` / `media.rs` 比 dist 晚 1 小时，**当前 dist 不含这两处最新改动**。
若其中有修复内容，需要重新执行 `SDKROOT=$HOME/opt/MacOSX11.3.sdk ./build-macos.sh` 再分发。

---

## ℹ️ 运行依赖：MTPZ 密钥文件缺失

`$MTPZ_DATA` 未设置，`~/.mtpz-data` 不存在；Mac 与 4090d 上都未找到。
即使 USB 会话打通，MTP 存储操作仍需要这个 5 行 hex 文件（public exponent / encryption key /
modulus / private key / certificates）。请提供路径或说明获取方式。

---

## 复现命令

```bash
scp -O mac-host:~/zune/zune-rs/dist/zune-aarch64-apple-darwin.tar.gz .
tar xzf zune-aarch64-apple-darwin.tar.gz
./zune-aarch64-apple-darwin info
```

Mac 端当前安装：`~/.local/bin/zune`（19:19 版，旧版备份在 `~/.local/bin/zune.bak-old`）

---

## ✅ 修复回应（Linux 端，2026-10-15 20:32）

P0 和 P1 都已处理，新产物已重新交叉编译到 `dist/`（20:32，晚于源码 20:31）。

### P0 — macOS 上 MTP 会话打不开

改动集中在 `src/usb.rs`，`src/main.rs` 增加恢复与诊断入口：

1. **不再吞错误**。`clear_halt` 现在重试 3 次（间隔 80 ms），每次失败都把
   `OUT/IN` 两端的真实错误打到 stderr：
   ```
   · clear_halt attempt 1/3: OUT 0x01 failed to clear halt on endpoint · IN 0x81 ok
   ```
2. **失败自动降级为 device reset**。三次都失败 → `IOUSBHostDevice ResetDevice`
   → 等 900 ms → 按 serial 轮询重新枚举（10 × 250 ms）→ 重新 claim → 重新 clear_halt。
   这条路只在 macOS 上必要，Linux 上通常第一次就成功。
3. **会话级恢复**。首次 `OpenSession` 若返回 stuck 类错误
   （`Stall` / `Fault` / `Cancelled` / `Unknown(0xe00002ed|0xe000404f|0xe00002c0)`），
   自动 reset + 重试一次，成功则打印 `» recovered after reset`。
4. **超时不再积毒**。bulk 超时后调用 `cancel_all()` 并排空完成队列 ——
   之前挂起的 transfer 留在端点上，正是「每失败一次状态更僵」的原因。
5. **错误带上下文与建议**，例如：
   ```
   bulk OUT failed on endpoint 0x01: unknown (error 0xe00002ed) —
   kIOReturnNotResponding: the Zune did not answer. This is the classic stale-stall
   symptom after an interrupted session, and on macOS the endpoint cannot be un-stalled
   in place — run `zune usb --reset`, or unplug and replug the device
   ```
   并把 hub 拓扑一起打出来（`bus 0 port-chain 1.2 addr 3 location 0x120000`），
   方便判断是不是扩展坞那条链。

### 新的诊断命令（请在 Mac 上先跑这个）

```bash
./zune-aarch64-apple-darwin usb          # 描述符 + 拓扑 + claim + un-stall 结果 + 控制传输探针
./zune-aarch64-apple-darwin usb --reset  # 复位并等待重新枚举
```

`usb` 的探针是标准控制传输（GET_DESCRIPTOR / GET_STATUS），**不碰 MTP**，
用来把「链路本身不通」和「bulk 管道/MTP 状态不通」分开：

- 探针失败 → 问题在 MTP 之下（线/坞/供电/别的程序占着设备）
- 探针成功但 bulk 失败 → 管道或设备端 MTP 状态问题

请按这个顺序复测，并把 `usb` 的完整输出回传：
① 换到 **Mac 机身直连口**（排除嵌套 hub）② `usb` ③ `usb --reset` ④ `info`。

### P1 — dist 落后于源码

已重新执行 `SDKROOT=$HOME/opt/MacOSX11.3.sdk ./build-macos.sh`：

```
dist/zune-aarch64-apple-darwin        4,180,656 B   Mach-O arm64, minos 11.0, ad-hoc signed
dist/zune-aarch64-apple-darwin.tar.gz 1,509,143 B
sha256  37c38536b355b0449b0360ade506c4d68809c8d75b677ce154000c9ca56ee337
```

### ℹ️ MTPZ 密钥文件

公开文件，libmtp-zune 直接发布（非机密，所有 Zune 通用），Mac 上一条命令：

```bash
curl -fsSL https://raw.githubusercontent.com/kbhomes/libmtp-zune/master/src/.mtpz-data -o ~/.mtpz-data
wc -l ~/.mtpz-data   # 应为 5
```

已验证可达，内容与本客户端期望完全一致：5 行 = 指数(5 hex) / 加密密钥(32 hex) /
模数(256 hex) / 私钥(256 hex) / 证书(1258 hex)。

---

## 🔧 第二轮修复（20:35）— `InvalidArgument` 根因已定位

新数据点（物理拔插后仍是参数类错误、失败在第一次 bulk IN）把范围缩到了 nusb 的
**客户端侧校验**。在 `nusb 0.2.8/src/device.rs:792` 找到了确切条件：

```rust
/// For an IN transfer, the buffer's `requested_len` is the number of bytes
/// requested. It must be a nonzero multiple of the endpoint's maximum packet
/// size or the transfer will fail with `TransferError::InvalidArgument`.
pub fn submit(&mut self, buf: Buffer) {
    if Dir::DIR == Direction::In {
        let req_len = buf.requested_len();
        if req_len == 0 || req_len % self.max_packet_size() != 0 {
            warn!("Submitting transfer with length {req_len} which is not a multiple of max packet size {} …");
            return self.backend.submit_err(buf, TransferError::InvalidArgument);
        }
    }
    self.backend.submit(buf)
}
```

即：**IN 传输的请求长度必须是端点 max packet size 的非零整数倍**，否则 nusb 在提交前
直接返回 `InvalidArgument`（Display 文本就是 “invalid or unsupported argument”，
对应 `macos_iokit/mod.rs:39` 的 `kIOReturnBadArgument`）。

原代码第一次读写死了 512 字节。512 只在 max packet ∈ {8,16,32,64,128,256,512} 时成立；
macOS 上描述的 wMaxPacketSize 与之不整除时，**第一个 IN 必然报参数错误**，
且与 stall 无关 —— 和你“拔插后仍然失败”的观察完全一致。

### 改动

1. `Transport::read()` 现在把请求长度向上取整到 max packet 的整数倍
   （`round_in_request()`，带单测：`(512,1024)→1024`、`(5,512)→512`、`(0,512)→512`、
   `(100,0)→100` 防除零）。
2. 端点 max packet size 在打开时记录并打印，出现在启动行与 `zune usb`
   的 `bulk IN 0x81 · max packet N`。
3. 新增 `zune usb` 里的 **bulk IN 探针**：claim 之后单独发一次「刚好一个 packet」
   的 IN（2 s 超时），报告原始结果 —— 用来区分「主机拒绝这个 IN 请求」（参数问题）
   和「设备没话可说」（MTP/session 问题）。
4. 接入 `env_logger`：`RUST_LOG=nusb=debug ./zune-aarch64-apple-darwin info`
   会打印 nusb 后端自己的判断，**包括上面那条 warn**（客户端拒绝，不进内核）。
   之前没有 logger，这条关键警告被丢掉了。

### 新产物（20:35，晚于源码 20:34）

```
dist/zune-aarch64-apple-darwin        5,437,232 B   Mach-O arm64, minos 11.0, ad-hoc signed
dist/zune-aarch64-apple-darwin.tar.gz 1,966,761 B
sha256   a83cdca7ee6553e60757c5d401315200e2ae138441fe05d1c275fd13b975afab
```

### 请在 Mac 上按这个顺序跑，把输出回传

```bash
./zune-aarch64-apple-darwin usb                     # 看 max packet 与实际值 + 两个探针
RUST_LOG=nusb=debug ./zune-aarch64-apple-darwin info # 看 nusb 内部判断
```

如果 `usb` 里 `bulk IN ok` 且 `info` 通过了 OpenSession，就说明就是这个问题；
`max packet` 打印出来的数值请一并回传（我怀疑不是 512）。

Mac 上有 cargo 1.94.0，也可以原生构建（依赖已经是纯 Rust，无 libusb 需求）：

```bash
cd zune-rs && cargo build --release    # target/release/zune
```

---

## 🩹 第三轮修复（20:44）— panic 是我的 bug，同时给出新的根因假设

### 1. panic 确实是我写的：drain 循环调用了空队列

你的堆栈（`take_completed_from_queue` → `queue.front().expect("no transfer pending")`）
命中的是我上一轮加的这段：

```rust
let Some(c) = ep.wait_next_complete(timeout) else {
    ep.cancel_all();
    while ep.wait_next_complete(Duration::from_millis(200)).is_some() {}   // ← BUG
    bail!("bulk OUT timed out …");
};
```

`cancel_all()` 之后**第一次**迭代会弹出那个被取消的 completion（`is_some` → 继续循环），
**第二次**调用时队列已空 → nusb 的 `expect` 直接 panic。

堆栈里 `<MacEndpoint>::wait_next_complete` 来自 `Transport::write` 的两个调用点是同一个符号，
所以真实时间线是：**write 等满 10 s 超时 → 进 drain → panic**。
也就是说：**bulk OUT 在 macOS 上根本没被设备确认**，和 19:12 版看到的
`bulk OUT timed out after 10s` 是同一件事，只是这次被我变成了崩溃。

修法：
- 两个方向都改用 nusb 官方文档推荐的 `Endpoint::transfer_blocking()` —— 提交 + 等待 +
  超时取消 + **必定消费掉传输**，不会再留 pending；超时返回 `status = Cancelled`
  （我把它翻译成明确的「设备未确认这次写入」错误，并附下一步动作）。
- 所有 drain 循环都先用 `pending() > 0` 守卫，绝不再对空队列等待。

### 2. 新根因假设：**设备有未读响应时不接受新命令**

你那次 `zune usb` 的 IN 探针在「我们什么都没写」的情况下读回了 **309 字节**，内容是

```
92 23 | 92 2b | 92 2c | 92 2d | 92 42 | 92 43 | 92 40 | 92 08 | 61 04 | ...
```

按 PTP 数据集的大端解析就是 `0x9223, 0x922B, 0x922C, 0x922D, 0x9242, 0x9243, 0x9240,
0x9208, 0x6104…` —— 这是 **Zune 厂商 opcode 列表**（`0x9204..0x9267` 正是 RE_NOTES 里
记录的厂商扩展块），也就是某次 `GetDeviceInfo`/`OperationsSupported` 响应的残留。

而 MTP 设备在半双工语义下，**上一笔响应没被读走时会对新的 bulk OUT 一直 NAK** ——
表现就是「写永远不完成」，且与 stall、参数、hub 都无关。这和你「拔插后仍失败」
并不矛盾：拔插清掉的是主机侧状态，而每次我们 probe 之前可能又产生了新的残留
（`zune usb` 的 IN 探针本身就会读走一部分）。

### 3. 这一轮的改动

1. `connect()` 在 OpenSession 之前**先排空**：安静读一次（350 ms），
   有残留就打印 `» cleared N bytes left over from an earlier session`，然后才开 session。
   这本身就是对上述假设的**自动修复**。
2. `zune usb` 现在是完整三步探针（都带超时、都不会 panic）：
   ```
   1. drain pending IN (quiet 350 ms) … → 报告读回多少字节（关键！）
   2. bulk OUT 16 bytes (OpenSession) … → ok / FAILED
   3. bulk IN one packet (512 bytes) … → 报告字节
   ```
3. 写入超时的报错现在直接告诉你下一步：`run zune usb, which drains pending data first
   and then retries the write`。

### 新产物（20:44，晚于源码 20:44:03）

```
dist/zune-aarch64-apple-darwin        5,454,560 B
dist/zune-aarch64-apple-darwin.tar.gz 1,970,892 B
sha256   bd402661b1de97dd6123c837ada64531d1c5afb76f72d52b937f88e9de1d47da
```

### 请跑这个（这次一定能看到东西，不会再 panic）

```bash
./zune-aarch64-apple-darwin usb 2>&1 | tee /tmp/zune-usb.txt
./zune-aarch64-apple-darwin info 2>&1 | tee /tmp/zune-info.txt
```

重点看 `usb` 第一步 drain 读回多少字节、第二步 OUT 是否 ok。三种结果对应三种结论：

| 现象 | 结论 |
|---|---|
| drain 读到 ~309B，之后 OUT ok | 就是残留响应阻塞 —— 已修（`connect()` 自动排空） |
| drain 为空，OUT 仍超时 | OUT 管道本身的问题，下一步换 libusb 后端（你机器上有 1.0.29，Python 版走 libusb 是通的，可原生 `cargo build --features libusb`） |
| OUT 超时且 drain 读到数据但读不完 | 设备固件卡在「响应未取走」，需要 `--reset` 或拔插后**先**跑我们的客户端（不要先跑别的程序） |

---

## 🎯 第四轮修复（20:51）— 0x201c 定位到**两个**独立 bug

### 4.1 `0x201C` 是什么

PTP/MTP 标准响应码，不是 Zune 私有码：

| code | name |
|---|---|
| 0x201A | InvalidParameter |
| 0x201B | SessionAlreadyOpen |
| **0x201C** | **TransactionCancelled** |
| 0x201D | SpecificationOfDestinationUnsupported |

即**设备主动取消了这笔事务** —— 不是参数不合法（那会是 0x201A/0x2006），
也不是不支持（0x2005）。「事务被取消」最典型的成因是：**它所在的 trusted session
已经不存在了**。

### 4.2 Bug ①：我多发了一次 `EndTrustedAppSession`

对比 Python 参考实现（`zune-cli.py` 的 `MTPClient.authenticate`），
`reset_mtpz()` 只出现**一次**，在交换**之前**：

```python
mtp.set_device_prop(0xD406, "libmtp/Sajid Anwar - MTPZClassDriver")
mtp.reset_mtpz()                 # ← 只有这一次
msg, rand = _build_app_cert_msg(certs, n, d)
mtp.send_mtpz(msg)
resp = mtp.get_mtpz()
mac_hash, _key = _parse_dev_resp(resp, rand, n, d)
mtp.send_mtpz(_build_confirmation(mac_hash))
mtp.enable_trusted(mac_hash)
```

我的 Rust 版在 `get_mtpz()` 和 confirmation 之间**又加了一次** `reset_mtpz()`
（0x9216 = EndTrustedAppSession）。它把设备刚刚建立的 trusted session 拆掉了，
随后的 `SendWMDRMPDReq` 自然被取消 → **0x201C**。

已删除，并给每一步加上标签，以后报错会直接说清是哪一步：

```
MTPZ 1/6 set DeviceProp 0xD406 (initiator name)
MTPZ 2/6 EndTrustedAppSession (0x9216) — clears any previous trusted session
MTPZ 3/6 SendWMDRMPDReq (0x9212) — app certificate
MTPZ 4/6 GetWMDRMPDResp (0x9213) — device challenge
MTPZ 5/6 decrypting the device response
MTPZ 6/6 SendWMDRMPDReq (0x9212) — confirmation MAC
MTPZ EnableTrustedOps (0x9214)
```

同时 `recv_resp()` 的错误现在带操作码与响应码名字，例如
`MTP error 0x201c (TransactionCancelled) from 0x9212 (SendWMDRMPDReq (MTPZ))`。

### 4.3 Bug ②（更要命，而且你原本也会撞上）：我的 AES-CMAC 是错的

为了在实机之前把密码学钉死，我写了**离线对拍**：用 Python 参考实现生成向量，
在 Rust 侧断言。结果：

| 测试 | 结果 |
|---|---|
| RSA + MGF1-SHA1 + 证书消息整体（785 B，sha1 `9a5d1530…`） | ✅ 和 Python **逐字节一致** |
| RFC 4493 AES-CMAC 三组向量 | ❌ **FAILED** |
| MTPZ confirmation / EnableTrustedOps 四字 | ❌ FAILED |
| NIST SP 800-38A AES-128-CBC | ✅ |

罪魁祸首是我手写 CMAC 里的 `dbl`（子密钥倍增）——**进位方向写反了**：

```rust
for i in 1..16 { o[i] |= x[i - 1] >> 7; }   // ✗ 从低位往高位进位
// 正确：大端左移时，byte i 的 bit7 进到 byte i-1
```

RFC 4493 的 `K1 = dbl(E_K(0))` 应该是 `fbeed618357133667c85e08f7236a8de`，
我的实现算出来是另一个值 —— 于是 confirmation MAC 和 EnableTrustedOps 四字**全错**。
**即使修好了 4.2 的顺序，这一步照样过不了握手。**

修法：直接删掉 45 行手写实现，改用 `cmac` crate（`<Cmac<Aes128> as Mac>`），
它本身就通过 RFC 4493。现在 `cargo test` 共 12 项全绿，包含：

```
test mtpz::tests::aes_cmac_matches_rfc4493                         ok
test mtpz::tests::mtpz_macs_match_the_reference_implementation     ok   # 0203001040104e8db8d421e74222e6381b4f50fa
test mtpz::tests::app_cert_message_matches_the_reference_implementation ok
test mtpz::primitive_tests::aes128_block_matches_the_reference...  ok
test mtpz::primitive_tests::aes128_cbc_decrypt_matches_nist        ok
```

（证书消息那一项需要 `MTPZ_DATA` 或 `~/.mtpz-data`，没有就自动跳过而不是失败。）

顺手修了个显示 bug：`MTP version 0.100` → `1.00`（该字段是定点表示，100 = 1.00）。

### 4.4 新产物（20:51:01，晚于全部源码 20:50:38）

```
dist/zune-aarch64-apple-darwin        5,46x,xxx B   Mach-O arm64, minos 11.0, ad-hoc signed
dist/zune-aarch64-apple-darwin.tar.gz
sha256  binary 6b0638927246950a557a9efdb4ea8014b83a5e2d559a9934ab0695c31bea6e74
        tarball 3af2bb0e43dbfacce8eaf589296629cc491df581a17c036535bdfd805e0b73c1
```

### 4.5 请测

```bash
./zune-aarch64-apple-darwin info      # 期望：MTPZ authenticated + storages + library 统计
./zune-aarch64-apple-darwin tracks    # 期望：列出设备上的曲目（这台应该已经有歌）
```

如果还有 MTP 错误，报错现在会自带步骤名与响应码名，直接回传即可。

---

## 🎯 第五轮修复（21:03）— 0x201C 真正的原因：**DATA 容器头长度写错**

你从 `mtpz.c:1763` 挖出的参考实现非常有价值 —— 它确认了**顺序、操作码、属性码、
值字符串、Nparam=1 全都没问题**，也把怀疑方向指向数据格式。对照之后我在自己的容器层
找到了那个 bug，而且它比「字符串编码」更底层：

### 根因

`src/mtp.rs` 里我让 `Container::data()` 复用了命令容器的 `build()`：

```rust
pub fn data(code: u16, txid: u32, payload: &[u8]) -> (Vec<u8>, Vec<u8>) {
    (build(T_DATA, code, txid, &[]), payload.to_vec())   // ✗ build() 按 params 数量算长度
}
```

而 `build()` 的长度是 `12 + params.len()*4`，params 传空 → **声明长度 = 12**。
于是设备收到的是：

```
DATA 头声明 total=12  ← 它认为这笔 DATA 到此结束
紧接着 75 字节负载     ← 它眼里这是下一条容器的开头，是垃圾
```

→ 设备认为事务不一致，**主动取消** → `0x201C TransactionCancelled`。**这也解释了为什么报错在
"1/6 set DeviceProp 0xD406"：那是我们第一次向设备写 DATA 阶段**（OpenSession/GetDeviceInfo
都不带 host→device 数据，所以之前的步骤全过）。

参考实现里这一行本来就是显式算的，是我翻译时写错的：

```python
# zune-cli.py (MTP.send_data)
self.write(struct.pack("<IHHI", 12 + len(data), self.DATA, opcode, self.tx))
```
```python
# 与 C 等价：ptp_setdevicepropvalue -> ptp_transaction(..., PTP_DP_SENDDATA, size, &dpv, NULL)
```

### 关于值编码（你怀疑的点，一并核实）

- 值 `"libmtp/Sajid Anwar - MTPZClassDriver"` 实为 **36 字符**（不是 35）
- MTP 字符串 = `u8 (字符数+NUL)` + UTF-16LE + NUL，即 `1 + 37*2 = 75` 字节
  （你说的 `1 + 70 + 2 = 73` 少算了 1 个字符；两种描述其实是同一种编码，结果都是 75）
- 负载 75 字节 → DATA 头必须声明 `12 + 75 = 87`，`75 % 512 != 0` 无需 ZLP
- `Nparam = 1` ✓（我们发 16 字节命令容器：len=16, type=1, code=0x1016, tx, param=0xD406）
- 所以字符串本身没问题；错的只是容器头。已加单测把这三点钉死。

### 新增的回归测试（共 15 项，全绿）

```
mtp::tests::data_header_declares_the_payload_length     ok   ← 头声明 12+len(payload)，且空负载=12
mtp::tests::command_header_declares_the_parameter_count ok   ← 命令容器仍按参数个数
mtp::tests::strings_round_trip                          ok   ← u8 计数含 NUL、UTF-16LE、可逆
```

同时 `Session::push_object_data`（SendObject）也改用同一个 `Container::data_header()`，
两条 DATA 路径现在由同一份实现 + 同一个测试保证，不会再各错各的。

### 新增字节级 trace（回应你「要不要 dump 收发字节」）

```bash
ZUNE_TRACE=1 ./zune-aarch64-apple-darwin info
```
每一步 bulk 传输都会打印方向、长度和头 48 字节，例如：
```
    [OUT]    16 B  1000000001000210000000000604d400
    [OUT]    12 B  570000000200061000000000
    [OUT]    75 B  25006c00690062006d00740070002f0053006100…
    [IN ]    12 B  0c0000000300010000000000
```
要和我这边对照时把这个日志贴回来即可。

### 新产物（21:03:30，晚于全部源码 21:03:09）

```
dist/zune-aarch64-apple-darwin        Mach-O arm64, minos 11.0, ad-hoc signed
bin sha256  b31b420c1cb373f688ec32b46cefdbb7d7ef21ce62f550f523dd43f2d440afdf
tar sha256  0c67e05ddb9cff80f23f27935d5fbfc764b29956c877ea14d9107e09384348ac
```

### 请测

```bash
./zune-aarch64-apple-darwin info      # 期望：MTPZ authenticated + storages + library 统计
./zune-aarch64-apple-darwin tracks    # 期望：列出设备上的曲目
./zune-aarch64-apple-darwin albums
```

如果还想抓字节，加 `ZUNE_TRACE=1` 前缀。这一步通了之后，`push` / `album` / `photo`
（同样走 DATA 阶段）应该也一起可用。

---

## 🎉 第六轮（21:13）— 核心链路已通，处理两个打磨项

先记一下里程碑：**USB → MTPZ → 音乐库枚举 全线打通**，2422 tracks / 87 albums / 7 playlists，
中文文件名（含点号）正常。

### 6.1 `ARTIST` / `ALBUM` 全显示 "-"：**我读属性的 opcode 用错了**

MTP 1.0 扩展块的正确码表是：

| opcode | name |
|---|---|
| 0x9801 | GetObjectPropsSupported |
| 0x9802 | GetObjectPropDesc |
| **0x9803** | **GetObjectPropValue** ← 读属性该用这个 |
| 0x9804 | SetObjectPropValue |
| **0x9805** | **GetObjectPropList** ← 我原来错用了这个 |
| 0x9810 | GetObjectReferences |
| 0x9811 | SetObjectReferences |

我写的 `OP_GET_OBJECT_PROP = 0x9805` 是 **GetObjectPropList**（参数/数据阶段完全不同），
所以每次读都失败，而我又用 `.ok()` 把错误吞了 → 渲染成空列。已改为 **0x9803**。

**同时改了「吞错误」这件事**：`Library` 现在统计属性读取失败次数并在 `tracks` 末尾提示：

```
(!) N property reads failed — labels may be incomplete. Last error: …
```

这样「设备确实没有标签」和「我们问错了」一眼可分 —— 这次的 bug 就是因为静默失败才拖了一轮。

我也顺手确认过固件侧：`mtpsvc_dis.txt` 里这些 opcode 不是立即数（走指针表 0x3601608），
所以无法从反汇编直接读表；因此新增了 **`zune ops`**，让**设备自己报**它的 85 个 opcode，
并直接列出我们依赖的几个是否被支持：

```
zune ops
  0x9803  GetObjectPropValue
  ...
the ones this client depends on:
  GetObjectPropValue       0x9803  supported / NOT advertised
  GetObjectReferences      0x9810  supported / NOT advertised
```

### 6.2 `albums` 显示 0 tracks：ObjectReferences 不能用属性读

播放列表/专辑的成员不是属性，而是 **GetObjectReferences (0x9810)**（与 SetObjectReferences
0x9811 配对，后者在 Python 版上已实机验证）。原来我用属性读 0xDC41，同样失败 → 计数为 0。

已改用 `0x9810`，并且**专辑的封面引用会自动扣除**：只额外读最后一条引用的 ObjectInfo，
若它是 CoverArt(0x3802)/JPEG 就减 1 —— 每个专辑只多花一次调用，不用遍历整个引用列表。

### 6.3 新增两个诊断命令

```bash
zune ops                 # 设备advertise的 opcode 列表 + 依赖项是否支持
zune props 0x0001002a    # dump 单个对象：format/size/parent/filename + Name/Artist/Album/
zune props "吻别"         #   AlbumArtist/Track + ObjectReferences（可用句柄或文件名）
```

`props` 是专门为 6.1 准备的：如果 `zune tracks` 仍显示 "-"，跑一次 `zune props <handle>`
就能看到**每个属性的原始值或原始错误**，立刻区分「设备没标签」与「读取失败」。

### 6.4 新产物（21:13:40，晚于全部源码 21:13:26）

```
dist/zune-aarch64-apple-darwin   Mach-O arm64 / minos 11.0 / ad-hoc signed
bin sha256  c02d33c415bf0c0f2a3489a7387e861503b9ec7d37b6c76dd770887bfdaf07c3
tar sha256  f478b7cf63fbf1b2ede4c55a03d2eff699f0a7529241a5e9a2445aa14c91f99a
```

单测 15 项全绿（新增 MTP 字符串/容器头不变量测试见第五轮）。

### 6.5 请测（纯读操作，不改设备内容）

```bash
./zune-aarch64-apple-darwin ops                     # 先看设备到底支持哪些 opcode（关键）
./zune-aarch64-apple-darwin tracks | head -20       # 期望 ARTIST/ALBUM 有内容
./zune-aarch64-apple-darwin albums | head -20       # 期望 tracks 计数非 0
./zune-aarch64-apple-darwin props 0x0001002a        # 单对象逐属性 dump
```

如果 `tracks` 的 ARTIST/ALBUM 出来了，说明 0x9803 正确；若 `zune ops` 里 0x9803
是 "NOT advertised"，则这台固件只提供 GetObjectPropList(0x9805)，我再走那条路。

**关于写路径**：`push`/`album`/`photo` 会改动设备内容。我不会擅自让你跑；
等用户明确同意后再测，建议先拿「一个新专辑文件夹」做增量测试，不要动现有 2422 首。

---

## 第七轮（21:26）— 回答 0x200f，并修掉性能退化

### 7.1 关于 0x9804 / 0x200f：**不是权限缺失，握手已经完整**

我读了 `mtpz.c` 的完整握手（1727-1814 行），第 6 步原文是这样的：

```c
static uint16_t
ptp_mtpz_opensesecuresyncsession (PTPParams* params, unsigned char *hash)
{
	unsigned char	mch[16];
	uint32_t	*hashparams = (unsigned int *)mch;
	unsigned int	macCount = *(unsigned int *)(hash + 16);
	mtpz_encryption_encrypt_mac(hash, 16, (unsigned char *)(&macCount), 4, mch);
	ret = ptp_mtpz_wmdrmpd_enabletrustedfilesoperations(params,
		__builtin_bswap32(hashparams[0]), __builtin_bswap32(hashparams[1]),
		__builtin_bswap32(hashparams[2]), __builtin_bswap32(hashparams[3]));
}
```

也就是说 **第 6 步就是 `EnableTrustedOps`(0x9214)**，参数正是 `CMAC(hash[0..16], hash[16..20])`
拆成 4 个大端 u32 —— 和我们 `enable_trusted()` 逐字节相同。设备回了 OK，所以
**secure session 是开着的**，`0x200f` 不是「还没进 secure 阶段」。

另外关于操作码命名：0x9804 **就是标准 MTP 的 SetObjectPropValue**（不是 Zune 私有）：
MTP 1.0 扩展块是 0x9801 GetObjectPropsSupported / 0x9802 GetObjectPropDesc /
**0x9803 GetObjectPropValue** / **0x9804 SetObjectPropValue** / 0x9805 GetObjectPropList /
0x9810 GetObjectReferences / 0x9811 SetObjectReferences。

**那 0x200F 是什么在挡？** 你的实测数据已经给出了答案：

> 推送后设备上显示 `0x010013ec  01 Mac 推送测试.mp3`

我们发过去的 ObjectInfo 文件名是 `push-test.mp3`（本地文件名），而设备显示的是
**`01 Mac 推送测试.mp3`** —— 那个 `01` 和标题只能来自**文件里的 ID3 标签**
（我们推的 mp3 标签是 track=1 / title=Mac 推送测试）。结论：

- **固件在收对象时自己解析 ID3 并把元数据写进它的媒体库**，然后把这个对象标记为
  「数据库拥有」，于是对客户端返回 `AccessDenied` 拒绝二次改写。
- 也就是说 **元数据本来就是对的**，走的不是 SetObjectPropValue，而是文件标签；
  官方客户端也是这么工作的（只发文件）。
- 我们的推送链路已经带上了标签：非 mp3 走 ffmpeg `-map_metadata 0`，
  ID3v2.4 会被重写为 v2.3（Zune 不认 2.4），mp3 原样直传时不重编码。

所以这一项的处理是「**改变行为预期**」而不是「修 bug」：

- 属性写入仍然尝试（无害），但失败时只打印**一行**汇总而不是 5 行噪音，并说明原因：
  ```
  · 5/5 properties refused by the device
    The Zune indexes metadata from the file's own tags and answers
    SetObjectPropValue with 0x200F (AccessDenied). The tags we sent (ID3v2.3)
    already carry title/artist/album/track, so this is expected —
    run `zune props <handle>` after a re-index to confirm.
  ```

**验证方法**（关键）：设备重新索引后再读一次属性，看 Artist/Album 是否已有值：

```bash
./zune-aarch64-apple-darwin props 0x010013ec     # 第六轮修的 0x9803 读路径
./zune-aarch64-apple-darwin tracks | head -20
```

如果 `props` 显示 Artist/Album 有值 → 标签生效，元数据功能完成；
如果全是空 → 说明固件只在「自己的媒体库」里存，属性读不出来，那我们读 Artist/Album
这条路本身不成立（那就该由我们自己维护 sidcar 索引，另议）。

### 7.2 性能：修好了，根因是**每次调用都逐对象往返**

`2427 曲 × (1 GetObjectInfo + 5 次属性读) ≈ 14500 次 USB 往返`，10ms/次 ≈ 145s，
正好对上你测到的 2 分 11 秒。`info` 超时也是同一原因（它原本为了算总大小把整库读了一遍）。

修法：改用 **GetObjectPropList (0x9805)** —— 一次调用取回**所有对象的某个属性**：

| 命令 | 之前 | 现在 |
|---|---|---|
| `tracks` | ~14500 次往返 | **2 次**（一次 all-properties，或 6 次 per-property 兜底） |
| `info` | 整库遍历（>300s 超时） | **3 次**（GetObjectHandles 计数，不再逐对象） |

- `tracks` 走 `GetObjectPropList(handle=0xFFFFFFFF, format=0, propcode=0xFFFFFFFF)`；
  若设备不支持「一次取全部属性」，自动退化为「每个属性一次调用」（6 次）；
  再不支持才回退逐对象，并每 250 条打印进度，不再静默卡住。
- 体积走属性 `0xDC04 ObjectSize`，所以快路径连 `GetObjectInfo` 都不用。
- `info` 默认只报计数（tracks/albums/playlists，3 次调用）；要总大小加 `--size`。
- 新增 `parse_prop_list` 的解析测试（含中文字符串 + u16 两种数据类型），共 **16** 项单测。

### 7.3 新产物（21:26:14，晚于全部源码 21:26:04）

```
dist/zune-aarch64-apple-darwin   Mach-O arm64 / minos 11.0 / ad-hoc signed
bin sha256  eb3f46b2a7477f04188c3c0c94d6039f92995c02170ac4f0a48942c7b253ae5d
tar sha256  d2cdbd2a21547f2299cb73b67c00841a1836162597930f780c169985abd67ec1
```

### 7.4 请测

```bash
time ./zune-aarch64-apple-darwin info            # 期望：秒级返回
time ./zune-aarch64-apple-darwin tracks | head   # 期望：秒级，且 ARTIST/ALBUM 有内容
./zune-aarch64-apple-darwin props 0x010013ec     # 看刚推的那首的标签是否已进设备库
./zune-aarch64-apple-darwin ops | tail -12       # 确认 0x9805 / 0x9803 是否 supported
```

`ops` 的输出对 7.2 的快路径能否生效很关键，麻烦一并回传。
如果 `tracks` 变快但 ARTIST/ALBUM 仍为空，请把 `props 0x010013ec` 的原始输出贴回来。
