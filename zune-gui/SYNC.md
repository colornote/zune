# 音乐同步（Sync）设计与实施计划

状态：**调研完成，待实现**。本文是实现的契约 —— 代码与本文冲突时，先改本文。
调研数据来自真实设备缓存（2425 首，serial `XXXXXXXX…`）、`zune-gui`/`zune-rs` 源码、
以及本机 ffmpeg/lofty 实测；每节都标了依据。§13 是待设备在线时补的验证项。

> **范围调整（2026-10-16，设备实测后）**：先不做 Zune → Mac 的导出，只做
> **本地 → 设备**的单向管理与同步；并且先修三个已定位的问题（缺封面、中文方框、
> 播放卡死），其中封面提取已修复。本文的**下行部分**（§4.2 / §5 / §8.3）保留作为
> 后续设计，不在当前实施范围内。当前实施清单与本次实测数据见 **§15**。

一句话：在 Mac 与 Zune 之间做**双向、增量、只增不删**的音乐同步 —— 设备上多的拉到
本地，本地多的推到设备，两侧都有的一动不动。先给计划，再执行；带容量检查、逐项进度、
失败清单和同步历史。

---

## 1. 目标与非目标

### 目标（v1）

| # | 目标 | 说明 |
|---|---|---|
| G1 | **双向增量** | `Mac → Zune` 与 `Zune → Mac` 各算一次差额，只传差的 |
| G2 | **计划预览** | 执行前给出 dry-run 计划：上行 N / 下行 M / 已同步 K / 待确认 J |
| G3 | **幂等** | 重复运行安全；中断后重跑只补缺口 |
| G4 | **只增不删** | 任何一侧都不因"另一侧没有"而自动删除 |
| G5 | **可见** | 容量、逐项状态、失败原因、同步历史 |
| G6 | **无设备可用** | 扫描与计划只用缓存里的设备库；设备不在也能准备 |
| G7 | **可导出可播放** | 设备 99.9% 是 WMA，导出提供转码选项（实测 ffmpeg 63× 实时） |
| G8 | **纯函数核心** | 匹配器/计划器/路径净化全部可单测，用真实数据做 fixture |

### 非目标（v1）

- **镜像/清理**：自动删除任一侧的多余曲目（v1 只报告）。
- **播放列表同步**（设备上的 7 个 `.pla`；它们是官方软件为每个专辑生成的 2 首列表）。
- **内容级去重**（hash/指纹）：设备不提供 mtime/hash；v1 用元数据 + manifest。
- **照片/视频**：Push 页现有路径保留，Sync 只管音频。
- 反向元数据合并：设备端 `SetObjectPropValue` 返回 `0x200F (AccessDenied)`。

---

## 2. 现有基础与实测数据

### 2.1 代码基础（已核实）

| 能力 | 位置 | 状态 |
|---|---|---|
| 单文件推送（标签 → 必要时 ffmpeg → 上传 → 字节进度） | `worker.rs::push_one` / `core/media.rs::to_mp3` | ✅ |
| `push_track_reported(path, data, tags, progress)` | `core/library.rs:485` | ✅ |
| 对象下载 `get_object_data(handle) -> Vec<u8>` | `core/device.rs:296` | ✅（封面已用） |
| 分段读 `get_partial_object(handle, offset, count)` | `core/device.rs:343` | ✅ **生产在用**（`embedded_cover` 读 ID3/APIC） |
| 设备库磁盘缓存（filename/title/artist/album/album_artist/track_no/duration_ms/size/format） | `cache.rs::LibraryCache` | ✅ 有真实数据 |
| 标签读取 `tags::read`、设备名规范化 `tags::device_key` | `core/tags.rs` | ✅ |
| 容量 `GetStorageInfo (0x1005)` | `worker.rs` refresh 内 | ⚠️ 部分固件不实现 |
| 事件/命令通道 `Event` / `Command` / `Progress` | `model.rs` | ✅ |
| 设计系统 v1.1 + 组件（`ProgressRing`/`GlassCard`/`SectionRule`…） | `ui/` | ✅ 新页面直接用 |

### 2.2 真实设备数据（2425 首，来自 `library.json`）

```
格式        实测：2423 个 MTP 对象标记为 0x3009 + 2 个 0xB901。
            下载验证后：**标记 0x3009 的文件内容是 MP3**（ID3 + MPEG 帧，128–320 kbps）；
            这个固件的 0x3009 表示“音频”而非标准的 WMA。README 里的
            "132 mp3 · 15 wma" 和“全库是 WMA”的假设都要以此为准。
大小        min 0.1 MB · 中位 5.7 MB · p99 18.4 MB · max 69.1 MB · 合计 15.35 GB
容量        总量 29.8 GB · 缓存时 free 10.16 GB
元数据      album 空缺 981 首 (40%) · artist 空缺 130 · track_no 空缺 1057 (44%)
            album_artist 空缺 981 · album_artist ≠ artist 594 首
            duration 只有 1 首缺失
文件名      99.9% 等于 `f"{track_no:02} {title}.{ext}"`（track 缺失时 `f"{title}.{ext}"`）
            两个例外是 .wma 扩展名的 AAC；2423/2425 用 (track,title) 可精确重建
身份碰撞    (title)                       → 196 组重复
            (title, artist)              → 178 组
            (title, artist, album)       →  78 组（3.4%）
            (title, artist, album, track_no) → 70 组（track_no 大量缺失）
            (title, artist, album, duration_ms) → 0 组 ← 2425/2425 唯一
播放列表    7 个，每个 2 首（官方软件的专辑 playlist）
```

### 2.3 三个必须写进设计的既成事实

**(a) 设备会重命名对象。** 真实 filename 形态是 `"{track:02} {title}.{ext}"`，
title/track 来自固件读到的文件内嵌标签：

```
'10  吻别.mp3'                     title=' 吻别'                    track=10
'01 01. Chorus Of The Saints.mp3'  title='01. Chorus Of The Saints' track=1
'2010-09-26.wma'                   title='2010-09-26'              track=None（AAC，未重命名）
```

所以**不能只按文件名匹配**：官方软件推上去的文件，本地 stem 与设备 filename 对不上；
反过来，(track, title) → 设备名的重建率是 99.9%，可以当匹配的一条腿。

**(b) `push_track` 是 replace 语义。** 同名对象会被**删除后重建**
（`core/library.rs::push_track` → `find_track` → `delete_object`）。同步执行器不能直接用；
需要 no-replace 模式（§8.2）。Push 页保持现状（手工推送的意图就是覆盖）。

**(c) `CacheDiff` 不是同步 diff。** 它的身份是对象 handle，只回答"两次设备读取之间变没变"。
本地文件没有 handle；同步需要自己的匹配器（§3）。

### 2.4 USB 与性能（来自 `MACOS_TEST_REPORT.md` 与本机实测）

```
链路          480 Mbps High Speed（ioreg UsbLinkSpeed = 480000000）
MTP 往返      ≈ 10 ms（实测：14500 次往返 ≈ 145 s；库读取已优化为 GetObjectPropList × 2）
推送块        16 KB（PUSH_BATCH）
读取超时      30 s（READ_TIMEOUT）；单次 IN transfer 一次请求全部剩余字节
转码          ffmpeg WMA→AAC：180 s 音频 2.87 s ⇒ ≈ 63× 实时，单线程
              推及全库 15.35 GB ≈ 2450 min 音频 ≈ 40 min CPU（单线程）
lofty         mp3/m4a 读标签与时长为真（duration 与 ffprobe 差 36 ms）
              但 lofty 0.25.4 **打不开 ffmpeg 生成的 WMA**（"failed to parse file"）——见 §5.3
```

### 2.5 由此得出的两个硬性结论

1. **大对象下载必须分块读**。现在 `recv_container` 对大文件会发起一次 69 MB 的 IN
   transfer，30 s 超时在 <2.3 MB/s 时必然触发（USB 速度待实测，§13）；分块后每块独立
   超时，顺带得到字节级进度（§8.3）。
2. **导出侧不能依赖 lofty 解析设备文件**。元数据（title/artist/album/track）直接取自
   设备对象属性（缓存里就有）；转码交给 ffmpeg（它能读 WMA 并保留标签）。

---

## 3. 身份与匹配（核心）

### 3.1 两侧条目

**DeviceEntry**（来自内存库或磁盘缓存）：
`handle`、`filename`、`size`、`format`、`title`、`artist`、`album`、`album_artist`、
`track_no`、`duration_ms`。

**LocalEntry**（扫描源目录）：
`path`（绝对）、`ext`、`size`、`mtime`、
`title`、`artist`、`album`、`album_artist`、`track`、`duration_ms`、
`stem_key = device_key("{stem}.mp3")`、`sig_key = norm(title, artist, album)`。

`duration_ms` 需要给 `tags::read` 加一项（`lofty` 的 `properties().duration()`，
`ZUNE-GUI` 扩展，几行）。解析失败的文件（§5.3 的 WMA）降级：用文件名解析
（`parse_device_filename`，"01 标题.wma" → track=1/title="标题"，单测）补上 track/title，
其余留空；这类文件绝不因为"没有标签"而被判成新曲目重复上传 —— 见 §3.3 第 3 条。

### 3.2 配对算法（确定性，按优先级，已配对的两侧出局）

1. **manifest 命中**：上次同步记录的 `(handle ↔ local path)`，两侧都还在 → 直接配对。
   manifest 是权威：标签被用户改过、文件名被改过都不影响。这是"同步过后永不重传"的保证。
2. **sig_key 唯一命中**：`norm(title, artist, album)` 两侧各只有一条候选 → 配对；
   若两侧都有 `duration_ms` 且差 > 3 s → 降级为 `Divergent`（同名不同版本，只报告）。
3. **设备名预测命中**：本地条目有 (track, title) 时，预测设备名
   `f"{track:02} {title}"`（无 track 时 `title`），与设备条目 `stem` 的 `device_key` 比。
   这一条兜住"标签对但 sig_key 有碰撞"的情况，且对 WMA 文件名解析出的条目同样有效。
4. **stem 兜底**：`device_key(local stem)` 与设备 `stem` 相等（老工具/未重命名的文件）。

剩余未配对的：仅本地 → **ToZune**；仅设备 → **ToMac**。
同一 sig_key 多条候选且 duration 也无法消歧 → **Ambiguous**（默认不动，UI 单列）。

依据：§2.2 的碰撞表（加 duration 后 0 碰撞）；文件名重建率 99.9%。
设计上遵守一条不变量：**匹配失败的方向必须是保守的**（宁可不传，不误删）。

### 3.3 边界情况与处理

| 情况 | 处理 |
|---|---|
| 两侧配对但 duration 差 > 3 s | `Divergent`，不动，UI 报告 |
| 两侧配对但 size 差很大（WMA 190k vs MP3 320k） | **正常**，size 不参与判定 |
| 本地多版本（同名不同源） | 各自进计划；sig_key 相同的第二首标 `Ambiguous` 不自动传 |
| 设备曲目被删（handle 消失） | manifest 失效 → 本地那首变 `ToZune`（推回去；只增不删的安全方向） |
| 导出目录又被加入 sources | manifest 保证 in sync，不会回环重传 |
| 本地文件解析完全失败（无名无签） | 用 路径+size 作弱身份，默认 `Ambiguous`，不自动上传 |

---

## 4. 双向语义

### 4.1 上行 Mac → Zune

- 计划：`ToZune`，排序 `(album_artist|artist, album, track_no)`，保持专辑内顺序。
- 执行：复用 `push_one`（读标签 → 必要时转码 → 上传），走 **no-replace** 通道。
- 容量：设备 `free` 可用时校验 `Σsize ≤ free`，不足拒绝开始并指出缺口；不可用则提示估计。
- `size` 用原始文件大小估算（`to_mp3` 通常输出更小）。
- **完成后**：把每个新 handle 写进 manifest（本地路径 ↔ handle），这样即使设备库缓存
  还是旧的，下一轮计划也不会重复推送（manifest 优先，§3.2）。UI 提示"Refresh 可看到新曲目"。

### 4.2 下行 Zune → Mac

- 计划：`ToMac`，排序 `(album, track_no, title)`。
- 执行：分块下载（§8.3）→ 校验字节数 → 写目标（§5.1）→ 需要时转码（§5.2）
  → 写 manifest（handle ↔ 目标路径）。
- 目录结构（默认）：`<export_dir>/{artist}/{album}/{NN} {title}.{ext}`
  - 缺失字段 `Unknown Artist` / `Unknown Album`（130 首两者都缺）；
  - 非法字符与 `..` 净化、长度截断、同名冲突加 ` (2)`；
  - 扩展名由输出格式决定：copy → 设备原格式（`.wma`）；transcode → `.m4a`。
- 容量：校验导出卷可用空间（全量 15.35 GB 或转码后 ≈ 8 GB，视格式）。

### 4.3 执行顺序与取消

- 一次 `Sync now`：**先下行、后上行**（备份优先）。计划可分别勾选；UI 也提供独立按钮。
- **取消**：项与项之间检查 `cancel` 标志（与 `push_tracks` 一致），当前项完成后停止；
  已完成的项留在 manifest 与历史里。
- **单项失败**：记录原因继续；`usb::is_stuck` 时中止整个 run。
- **失败项即差额**：下一轮计划天然把它们算回 ToZune/ToMac，重跑即重试。

---

## 5. 导出格式：copy 还是转码

### 5.1 两种模式

| 模式 | 过程 | 体积 | 可播性 | 时间（全库） |
|---|---|---|---|---|
| **copy**（忠实备份） | 字节原样落盘，扩展名按设备格式 | 15.35 GB | macOS 不能直接播 `.wma`（QuickTime 无解码器） | ≈ USB 时间 |
| **transcode → m4a**（默认推荐） | ffmpeg `-c:a aac -b:a 256k -map_metadata 0` | ≈ 8 GB | Music.app/QuickTime 直接播 | + ≈ 40 min CPU |

转码标签保真已实测：title/artist/album/track 全部保留；`album_artist` 需要显式
`-metadata album_artist=` 补写（ffmpeg 不会自动从 ASF 映射到 m4a 的 `aART`，实测为 None）。

### 5.2 转码的工程细节

- 输入是**下载后的临时文件**，输出 `.m4a` 写 `.part` 再 rename，成功后删除临时 WMA。
- `Zune 30` 的 WMA 是官方软件当年的有损转码，再转一层是二次有损 —— UI 必须说明这一点
  （文案：*"the device stores these as WMA; an .m4a copy plays everywhere but is not the
  original file"*）。
- v1 顺序执行（每首下载 → 转码 → 落盘 → manifest）。并发转码是 v1.1 优化
  （63× 实时、4 并发即可把全库压进 15 分钟），接口预留 `transcode_parallelism` 配置。

### 5.3 lofty 的 WMA 局限（为什么元数据不来自文件）

本机实测：`lofty 0.25.4` 对 ffmpeg 生成的 WMA 报 `failed to parse file`（mp3/m4a 正常）。
设备固件写入的 WMA 能否解析未知（§13）。因此：

- **导出不需要它**：元数据来自设备对象属性（缓存里齐全），转码用 ffmpeg 自己的 ASF 解析。
- **上行扫描不能依赖它**：解析失败的文件走 §3.3 降级，不让"读不到标签"变成"重复上传"。
- 如果 §13 验证设备 WMA 可解析，也只是锦上添花，不改上述设计。

---

## 6. 数据结构与磁盘布局

```rust
// locallib.rs —— 本地音乐库（跨设备，可重建）
pub struct SyncConfig { pub sources: Vec<SyncSource>, pub export_dir: PathBuf,
                        pub export_format: ExportFormat /* Copy | M4a */ }
pub struct SyncSource { pub path: PathBuf, pub enabled: bool }
pub struct LocalIndex { pub version: u32, pub scanned_at: i64, pub entries: Vec<LocalEntry> }

// sync.rs —— 匹配、计划、记录（per device）
pub struct SyncManifest { pub version: u32, pub pairs: Vec<Pair> }   // { handle, local_path, sig_key, synced_at, size }
pub struct SyncPlan { pub to_zune: Vec<LocalEntry>, pub to_mac: Vec<DeviceEntry>,
                      pub synced: usize, pub ambiguous: Vec<Ambiguous>, pub divergent: Vec<Pair>,
                      pub bytes_up: u64, pub bytes_down: u64, pub origin: DataOrigin }
pub struct SyncRun { pub started_at: i64, pub finished_at: i64,
                     pub pushed: usize, pub pulled: usize, pub failed: usize,
                     pub bytes_up: u64, pub bytes_down: u64, pub failures: Vec<String> }
pub struct SyncHistory { pub version: u32, pub runs: Vec<SyncRun> }  // 保留 50 次
```

磁盘（沿用 `cache::data_dir()` 的平台规则）：

```
<data-dir>/sync-config.json                      # sources + export_dir + export_format
<data-dir>/sync-index.json                       # LocalIndex
<data-dir>/devices/<SERIAL>/sync-manifest.json   # SyncManifest
<data-dir>/devices/<SERIAL>/sync-history.json    # SyncHistory
```

`LibraryCache::last_full_sync`（已预留）在每次完整成功的 run 结束时写入。

---

## 7. 架构与线程

新增模块（`core/` 快照只做 §8.2 标注的两处 `ZUNE-GUI` 小扩展）：

- `src/locallib.rs` — 源配置读写、递归扫描（栈式遍历，跟随 `pick_cover` 的风格；跳过
  隐藏目录与符号链接，深度 ≤ 8）、增量索引（mtime+size 未变不重读标签）、索引落盘。
- `src/sync.rs` — 匹配器、计划器、manifest/history、文件名净化与导出路径构造。
  **全部纯函数**，`plan(local, device, manifest) -> SyncPlan` 不碰 USB、不碰 UI。
- `src/worker.rs` — `Command` 的扫描与执行器。
- `src/app.rs` + `src/ui/sync_page.rs` — 页面（照 `ui/board.rs` 的模式：页面在 `ui/` 下，
  自己的状态与它在一起；app.rs 只留路由与事件臂）。

线程模型不变：**UI 只发命令，worker 是唯一碰 USB 的线程**。扫描/计划也放 worker ——
另起线程只会带来状态同步问题；扫描期间其他命令排队是可接受的。

```
Sync 页面
  ├─ Command::SyncPlan { }            → 扫描（增量）⊕ 设备库 ⊕ manifest → Event::SyncPlan
  ├─ Command::SyncRun { tasks }       → 逐项执行（先 ToMac 后 ToZune）→ Event::SyncItem / SyncFinished
  ├─ Command::SyncSetConfig { config }→ 落盘；UI 随后自己再发 SyncPlan
  └─ Command::Cancel                  → 项间停止
```

计划在 `SyncRun` 时由 UI 回传（`Vec<SyncTask>`），worker 不保存计划状态；每个
`Event::SyncItem` 自带 `index`，UI 直接映射列表行。

设备库来源：`Backend` 增加 `tracks: Vec<Track>`（与现有 `albums` 对称；`refresh` /
`apply_cache` 时填充）。**无设备**时来自磁盘缓存，`Event::SyncPlan.origin` 带
`cached · N min ago`，页面必须显示这一点。

---

## 8. 执行器

### 8.1 新事件

```rust
pub enum SyncDirection { Up, Down }
pub enum SyncState { Pending, Running, Done, Failed, Skipped }
pub struct SyncItemEvent { pub index: usize, pub total: usize, pub direction: SyncDirection,
                           pub label: String, pub state: SyncState, pub note: String,
                           pub bytes_done: u64, pub bytes_total: u64 }
Event::SyncPlan(Box<SyncPlan>)
Event::SyncItem(Box<SyncItemEvent>)
Event::SyncFinished(Box<SyncRun>)
```

### 8.2 `core/` 的两处最小扩展（`ZUNE-GUI` 标记）

1. `tags`：`TrackTags` 增加 `duration_ms`（lofty `properties().duration()`）。
2. `Library`：`push_track_reported` 增加 `replace: bool`（默认真，签名兼容）；
   同步传 `false` —— 遇到同名返回错误、**绝不 `delete_object`**。

### 8.3 下行 `export_one`（关键路径）

```text
① 分块读（协议层小改，core/device.rs，ZUNE-GUI 标记）
   recv_container 的循环把请求切成 ≤ 4 MB 的块：
       let want = (total - buf.len()).min(4 << 20);
       let more = self.transport.read(want, timeout)?;   // 每块独立 30 s 超时
   依据：现状对大文件一次请求 69 MB，30 s 超时在 <2.3 MB/s 时必然触发（§2.5）。
   4 MB 的下限速率是 133 KB/s，安全；顺带拿到字节级进度回调点。
② 写临时文件 <target>.part → 校验字节数 == device.size
③ 需要转码（ExportFormat::M4a）：ffmpeg -i .part -c:a aac -b:a 256k
   -map_metadata 0 -metadata album_artist=… -movflags +faststart <target>.part
④ fs::rename(.part → 目标名)（同目录，避免跨卷）
⑤ 写 manifest（handle ↔ 路径），记历史
```

备选方案（若 §13 实测分块读有问题）：`get_partial_object(handle, offset, count)` 分段，
它已在生产验证；代价是每块多一次 MTP 往返（4 MB/块 ≈ 加 77 s 全库）。

### 8.4 上行

复用 `push_one` 的流程，但：
- 计划已排除"设备已有"的项，no-replace 只作为最后一道保险（同名即失败并报告，不删除）；
- 每个成功的 handle 立刻写 manifest；
- 转码进度与上传进度沿用现有 `Progress`（任务名 "Syncing"）。

---

## 9. Sync 页面（UI）

导航新增 `Page::Sync`（`Icon::Sync` 已存在），排在 `Push` 之前；**Push 页保留**。
页面在 `ui/sync_page.rs`，用 v1.1 令牌与现有组件。

```
┌─ Sync ──────────────────────────────────────────────────────────────┐
│  ┌────────────┐   计划        ↑ 12 to push · 210 MB                  │
│  │  ◕ 24.1 GB │              ↓  3 to pull ·  18 MB                  │
│  │  8.9 free  │              = 342 in sync      ! 2 need review      │
│  └────────────┘   device library · cached 3 min ago                  │
│  Sources            [ + Add folder ]      [ Pull ]  [ Push ]        │
│    ~/Music/Zune  342 首                                              │
│  Export to          ~/Music/Zune Export   m4a · 12.3 GB free         │
│  History            today 14:02  ↑12 ↓3  1 failed                    │
│                     yesterday    ↑0  ↓40                             │
│  ────────────────────────────────────────────────────────────────    │
│  QUEUE                                            [Scan] [Sync now]  │
│   ↑ ○ 01 曲名 · 艺术家                     4.1 MB                    │
│   ↑ ◐ 02 曲名 · 艺术家   转码中…           5.2 MB                    │
│   ↑ ✓ 03 曲名 · 艺术家                     3.8 MB                    │
│   ↓ ○ 10 那首歌 · 歌手                     6.0 MB                    │
│   ✓ 已同步 342 … （折叠）      ! 待确认 2 … （折叠）                 │
└─────────────────────────────────────────────────────────────────────┘
```

- **空状态**：无源（"Add a folder to sync"）；无设备（计划区标注来自缓存，允许预览）；
  已全部同步（"Everything is in sync — last run …"）；密钥/ffmpeg 缺失（warn 卡片，现成文案）。
- **dev 环境变量**：`ZUNE_PAGE=sync`；`ZUNE_DEMO=1 ZUNE_DEMO_SYNC=1` 合成 3 上 2 下计划，
  截图不依赖设备（沿用现有 `ZUNE_SCREENSHOT` 机制）。
- 列表用 `ScrollArea::show_rows` 虚拟化（2425 条时保持空闲 0 % CPU 的纪律）。

---

## 10. 分阶段实施

| 阶段 | 内容 | 验收 |
|---|---|---|
| **S1** | `locallib.rs`：配置、扫描、增量索引、落盘 | 单测：fixture 目录、mtime 不变不重读、round trip、WMA 名解析降级 |
| **S2** | `sync.rs`：匹配器 + 计划器 + manifest/history + 净化 | 单测：真实样本 fixtures（`10  吻别.mp3` 等）、四类结果、碰撞消歧、`..` 净化 |
| **S3** | `core/` 两扩展 + `recv_container` 分块 + worker 执行器 | 单测 no-replace；`cargo test` 全绿；真机 dry-run 只读不写 |
| **S4** | `ui/sync_page.rs`：双栏、状态、空态 | `ZUNE_DEMO_SYNC=1` 截图可评审；无设备不崩 |
| **S5** | 打磨：取消、失败清单、`last_full_sync`、README/CACHE 文档 | 手工清单；空闲 CPU 仍 0 |
| **S6** | Mac 实测：dry-run → 小批量（上行 3 + 下行 2）→ 幂等重跑 → 全量 | §11.2 |

---

## 11. 测试与验收

### 11.1 单元测试（`cargo test --features linux`）

- 匹配器三层优先级、Ambiguous、Divergent、重名双向、全半角/大小写/繁简（`t2s` 已有）。
- 计划器：计数/字节/排序/free 判定。
- 索引器：增量正确性、空目录、符号链接、深度上限。
- 路径净化：`/ : * ? " < > |`、尾部空格与点、`..`、超长、中文保留。
- manifest/history round trip、版本不匹配忽略（与 `cache.rs` 同策略）。

### 11.2 Mac 实测流程

1. `ZUNE_DEMO_SYNC=1` 看页面（无设备）。
2. 连设备 → Plan → 核对数字：首次 `to_pull` 应 ≈ 2425（本地为空时）；
   `to_push` 视本地库。
3. 下行 2 首 → `ffprobe` 校验（时长/标签/可播）→ 再次 Plan → 这 2 首转 `in sync`。
4. 上行 3 首 → 设备上可见 → 再次 Plan → 转 `in sync`。
5. 幂等：再跑一次 → 0 传输。
6. 全量下行 + 中断续传（强制退出后重跑只补缺口）。

判据：每一步**计划数字与执行结果一致**（transfer N == plan N）。

---

## 12. 风险与缓解

| # | 风险 | 缓解 |
|---|---|---|
| R1 | 匹配失败 → 重复传输 | 三层匹配 + manifest 权威；首次全量数字人工核对一次 |
| R2 | 执行器误删设备文件 | no-replace 硬要求（§8.2） |
| R3 | 30 s 超时打断大文件下载 | 分块读（§8.3）；备选 GetPartialObject |
| R4 | 首次下行 15 GB / 转码 40 min | 幂等续传；copy 模式可先落地；并发转码留 v1.1 |
| R5 | `GetStorageInfo` 被拒 | 显示 unknown，不阻塞 |
| R6 | 首次扫描大库慢 | 增量索引（mtime+size 命中不读标签）+ Progress 事件 |
| R7 | `lofty` 读不了 WMA | 导出不依赖它；扫描降级；§13 验证真实 WMA |
| R8 | 导出卷磁盘满 | 开始前校验；写 `.part` 失败即停，不留半成品 |
| R9 | 同名不同内容（Divergent） | 默认不动 + UI 报告，绝不自动覆盖 |
| R10 | 目标目录已有同名文件（非本工具所写） | manifest 判定；不是我们的就跳过并标冲突，**不覆盖** |

---

## 13. 待设备在线的验证项（按优先级）

> 2026-10-16 设备已实测，完成情况标在每项后面。

1. **下载吞吐**：`get_object` 一首 8 MB 曲目计时 → 推算全量时间；
   同时验证 §8.3 的 4 MB 分块（与现有一把梭的对照）。
   —— **已完成**：5.0 MB / 2.6 s（含握手）≈ 2 MB/s，全库 15.35 GB ≈ 1.5–2 h。
   未测：4 MB 分块（导出不在当前范围）。
2. **真实 WMA 的标签**：下载一首设备 WMA，`ffprobe` 看 ASF 标签；`lofty` 再试一次
   （决定 §5.3 的降级是否成为常态）。
   —— **已完成且结论变了**：设备文件是 MP3（不是 WMA）。ID3 标签普遍带官方 Zune
   私有帧（`ZuneCollectionID`），部分中文标签是 GBK 编码。已下载的 41 个文件样本中
   10 个带 `APIC` 封面（约四分之一）。§5.3 的“lofty 读不了 WMA”仍然成立，但不再重要。
3. **导出文件可播性**：转码后的 `.m4a` 在 Music.app 打开、标签正确。
   —— **不在当前范围**（不做下行）。
4. **`GetStorageInfo`** 在当前固件是否可用（决定容量显示）。
   —— **已完成**：缓存里有 capacity 29.8 GB / free 10.16 GB，可用。
5. **`size` 一致性**：下载文件字节数 == 设备对象 size（校验逻辑的前提）。
   —— **已完成**：5249476 == 5249476 ✓（多个样本一致）。

---

## 14. 待拍板的决策（默认值已写进规划）

> 2026-10-16 已定：**不做下行导出**（本条以下的 1、2、5 暂缓）；中文不转写；
> 先做单向管理与问题修复。

1. **导出默认**：`m4a 转码`（推荐，可直接播放）还是 `copy 原格式`（忠实、更快）？
   —— 计划默认 m4a，UI 一个开关切换。
2. **导出目录结构**：`Artist/Album/NN Title.m4a`（默认）还是平铺？
3. **上/下行都只增不删**（默认）。镜像删除何时做？
4. **Push 页与 Sync 页并存**（默认）还是并入 Sync？
5. **首次全量下行的默认范围**：全部 2425 首（默认）还是按专辑勾选？

---

## 15. 当前实施清单与本次实测结论（2026-10-16）

### 已完成

**单向管理：Sync 页面** — `src/locallib.rs` + `src/sync.rs` + `src/worker.rs` + `src/app.rs`：

- 源文件夹（`sync-config.json`）→ 递归扫描 + 增量索引（size/mtime 不变则不重读标签，`sync-index.json`）
  → 设备库匹配 → 计划 → 一键推送。
- 匹配用三把 key（`sync.rs` 头注释）：设备文件名的规范化、固件会写出的 `{track:02} {title}.mp3`、
  以及去掉前导编号的标签签名 `title|artist|album`；空白折叠（设备上有 `10  吻别.mp3`）。
- 无设备也能出计划（用缓存里的设备库）；推送后 worker 把新推的条目记入内存库，
  所以“再计划一次”会直接显示全部已同步（不重复传输）。
- 推前的 `is_present` 检查保证只要设备上存在同名对象就不会推 → 不会触发 `push_track` 的
  “同名删除重建”，即误删风险在设计上被消除。
- 单测：扫描/增量/往返（`locallib`）与匹配/计划/重名（`sync`），共 12 项；
  `cargo test --features linux` 全量 **102 项全绿**。

**手动封面** — 专辑详情页的 “Set cover…” + 拖一张图到窗口：

- `Command::SetCover` → 读文件 → `decode_cover` 验证 → `cache::write_cover` → `Event::Cover`
  （origin 显示为 `your file · x.jpg`）；同时 `coverfetch::clear_miss` 撤掉该专辑的 miss 记录。

**封面提取修复** — `src/core/id3.rs` + `src/worker.rs::embedded_cover`：

- 根因（设备实测）：真实文件的 APIC 帧带 **v2.4 Data Length Indicator**（帧 flags `0x01`），
  且整个 tag 带 **unsynchronisation**（tag flags `0x80`，图片字节里的 `0xFF` 存为 `0xFF 0x00`）。
  旧代码把 DLI 的 4 字节当成 mime 字段 → 不以 `image/` 开头 → 判定“没有封面” → 静默跳过。
- 修法：跳过 DLI；解转义（tag 级与帧级，且只解一次）；帧 size 两种读法（plain / synchsafe）；
  worker 从“64 KB 定位 + 精确二段读”改为“探测 64 KB，有 APIC 则一次读 1 MB 并解析整个 tag 区”。
- 验证：41 个设备文件样本中 10 个提取出有效 JPEG（200×200 ～ 800×800，`sips` 全部解出）；
  其中 **6 个属于 GUI 标记“缺封面”的专辑**（Amanda Morra、Believe、DJ-Kicks、LeToya、
  Mädchen im Regen、The Boat That Rocked OST），另 1 个“有封面”专辑（B-Sides）也修好了。
  新增单测：DLI 帧、unsynchronised tag；`cargo test --features linux`（工具链 1.99）
  **90 项全绿**，`cargo check` 无错误。

### 待办

1. **剩余缺封面的 23 张专辑**：设备文件里确实没有 `APIC`（普查 29 张里 23 张无图）→
   需要“手动指定封面”（新功能）或更好的在线匹配。
2. **播放卡死**：需要用户指出具体曲目。已知候选：`01 Mac 推送测试.mp3`（设备读不出 duration，
   126 KB，之前 agent 的测试文件）与 2 个 AAC（格式/扩展名不符）。定位方法：
   `get-object` 下载 → `ffmpeg -v error -i x -f null -` 全解码 → 看错误位置。
3. **中文方框**：固件缺 CJK 字体（微软官方确认；旧的“拷 4 个字体到设备”的 hack 已被
   新固件封堵）→ **设备端不可修**；用户已决定不做拼音转写。
4. **单向管理功能**（本阶段主体）：浏览 / 搜索 / 删除 / 推送 + 封面修复 + 计划。
   上行的匹配与计划部分（§3 / §4.1 / §6 / §7）就是它的设计基础，去掉下行的部分。

### 本次实测数据（补充 §2）

- 设备：Zune 30，serial `XXXXXXXX…`，固件 `03.30.00039.00-00435`，Keel 3.30。
- 库：2425 个音频对象；`zune list --kind audio` 全量遍历 ≈ 2m21s（逐对象 `GetObjectInfo`）
  → 同步的设计必须按缓存算，不要逐对象扫设备。
- MTP 错误码注记：`get-object` 的 handle 参数**只按十六进制解析**（传十进制会被当成
  0x16779320 而报 `0x2009 InvalidObjectHandle`）；缓存里的 handle 在设备端仍然有效。
- 中文标签的实际构成（按字符统计）：常用简体 13541、繁体/生僻 2655、全角标点 741、
  日文假名 44、韩文 3；**乱码字符 0 个** —— 数据侧是干净的，方框纯粹是固件字体缺字形。
- 设备库内部没有精确重复（title+artist+album+时长+大小 全同 = 0 组）。
