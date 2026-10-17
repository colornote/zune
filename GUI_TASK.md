# 任务：为 zune 实现桌面 GUI 客户端

## 目标

给已有的 CLI 客户端加一个桌面图形界面，管理 Zune 30 / Zune HD：
设备状态、曲目列表、专辑网格、推送音乐（带进度）、删除、eject。

## ⚠️ 工作区约束（重要）

- **不要改动 `~/zune/zune-rs/` 下的任何文件**。
  另一个 agent 正在那里修 macOS 的 USB 问题（`src/usb.rs`），改同目录会冲突。
- GUI 代码放在**新目录** `~/zune/zune-gui/`。
- 需要复用核心逻辑时，优先「复制最小必要模块」或「等 USB 修复稳定后再抽 lib」，
  不要直接编辑 zune-rs。

## 技术选型（默认方案，有强理由可改）

**egui + eframe（纯 Rust）**，理由：

1. 现有构建链是「Linux 上 Zig + macOS SDK 交叉编译」（见 `zune-rs/build-macos.sh`），
   纯 Rust 依赖最可控；引入 Node/WebView（Tauri）会让交叉编译复杂化。
2. 与 zune-rs 同为 Rust，核心结构体可直接复用，无需 IPC/序列化层。
3. 单二进制、无运行时依赖，方便分发到 Mac。

服务器环境已具备：`cargo 1.99`、`cargo-zigbuild`、`zig`、`~/opt/MacOSX11.3.sdk`、
`node v22`（Tauri 路线备用）。

## 分两阶段执行

### 阶段 1：PoC —— 先证明「能产出能在 Mac 上跑的 GUI 二进制」

这是最大风险点，**不要跳过**：

1. `cargo new zune-gui`，加 `eframe` 依赖，写一个最小窗口
   （标题 "Zune"，一行文字 + 一个按钮，点按钮改文字）。
2. 交叉编译到 `aarch64-apple-darwin`：
   - 复用 `zune-rs/build-macos.sh` 的 linker 方案（Zig 做 linker + `SDKROOT`）
   - 或试 `cargo zigbuild --target aarch64-apple-darwin`
3. 产出 `dist/zune-gui-aarch64-apple-darwin.tar.gz`。

**必须回答**：`eframe`/`winit`/`wgpu`（或 `glow`）能否从 Linux 交叉编译到 macOS arm64？
框架链接（Cocoa / Metal / QuartzCore / CoreGraphics / IOKit）是否顺利？

若卡住：立刻报告卡点 + 替代方案（例如换 `glow` 后端、或改由 Mac 本地 `cargo build`），
**不要死磕**。

### 阶段 2：完整 UI（PoC 通过后）

参考原版 Zune 软件的视觉语言（Metro 风格、橙色调、大字号扁平化、专辑封面为主）。

MVP 功能：

1. **设备栏**：连接状态、设备名/型号/序列号、剩余空间、eject 按钮
   （复用 zune-rs 的 `usb::find_device` + `device::Session`）
2. **曲目列表**：标题 / 艺术家 / 专辑 / 时长 / 大小，可排序、可搜索
   （复用 `library.rs` 的 tracks 逻辑）
3. **专辑网格**：封面 + 专辑名，点击进入曲目
4. **推送**：选文件/文件夹 → 进度条 → 完成提示
   （复用 `push` / `album` 流程，注意进度回调需要改造为事件/通道）
5. **删除**：选中曲目 → 确认 → 删除
6. **WiFi 配置页**（可选，复用 `wifi scan` / `set-profile`）

工程注意：

- 设备 I/O 是阻塞的（`nusb` 同步 API），**必须放到后台线程**，
  用 `std::sync::mpsc` 或 `crossbeam-channel` 把状态/进度送回 UI 线程。
- UI 刷新用 `ctx.request_repaint()`，不要 busy-loop。
- MTPZ 密钥文件路径：`$MTPZ_DATA` → `~/.mtpz-data` → `./.mtpz-data`，
  UI 里要能显示「密钥缺失」的明确提示。

## 交付物

1. `zune-gui/` 源码
2. `zune-gui/dist/zune-gui-aarch64-apple-darwin.tar.gz`
3. 简短的构建/运行说明（README 片段）

## Mac 端测试者信息

Mac 端有另一个 pi agent 负责实测，会把结果反馈回来：
- 机器：Mac16,8 / Apple M4 Pro / macOS arm64
- 设备：Zune 30, serial `XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX`（已隐去）
- Mac 上有 cargo 1.94 + ffmpeg 8.1.1 + sips，必要时可本地构建

---

## 环境与网络（已核实，省你排查时间）

| 项 | 位置/状态 |
|---|---|
| cargo | `~/.cargo/bin/cargo`（1.99.0），`cargo-zigbuild` 已装 |
| zig | `~/opt/zig-x86_64-linux-0.15.2/zig`（**不在默认 PATH**，需显式加） |
| macOS SDK | `~/opt/MacOSX11.3.sdk` |
| node / npm | v22.22.0 / 10.9.4（Tauri 备用路线） |
| crates.io | **直连返回 403** |
| USTC 镜像 | 可达（200） |

**拉依赖若失败/慢**，在项目内放 `.cargo/config.toml`（只影响本项目，别改全局 `~/.cargo/config.toml`）：

```toml
[source.crates-io]
replace-with = "ustc"

[source.ustc]
registry = "sparse+https://mirrors.ustc.edu.cn/crates.io-index/"
```

## 交叉编译命令草案

```bash
export PATH="$HOME/opt/zig-x86_64-linux-0.15.2:$PATH"
export SDKROOT="$HOME/opt/MacOSX11.3.sdk"
export MACOSX_DEPLOYMENT_TARGET=11.0
# 方式 A：复用 zune-rs 的 linker wrapper（zig cc 做 linker）
# 方式 B：cargo zigbuild --target aarch64-apple-darwin --release
```

注意：`eframe` 默认走 `wgpu`（Metal）后端；若链接框架出问题，
可先试 `default-features = false, features = ["glow"]`（OpenGL 后端，macOS 上虽弃用但可用）。
