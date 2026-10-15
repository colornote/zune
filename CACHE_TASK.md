# 任务：为 zune-gui 实现设备库的本地缓存层

## 背景（为什么做）

现在每次启动或 Refresh，都要从 Zune 读 2425 首曲目 + 89 专辑 + 播放列表，
耗时 2~3 分钟（逐对象 USB 往返）。这严重阻塞 UI 开发与验收测试
（改一行样式要等 3 分钟才能看到专辑页）。

## 目标

1. **首次读取后落盘缓存**；后续启动**秒级**加载
2. **手动强制刷新**保留（Refresh = 真的重读设备）
3. UI 明确标示数据来源：缓存（+ 时间）/ 实时
4. 封面图片也缓存（避免每次重新 GetObject + 解码）

## 硬性约束

- 不要动 ~/zune/zune-rs/ 目录（另一个 agent 在改）
- 只在 zune-gui/ 里实现
- 另一个 agent（w3:p3）正在同一个 repo 里改**封面解析 + 专辑页 + 时长格式**，所以：
  - 优先改**新文件**（如 src/cache.rs），避免和他们抢 app.rs / worker.rs 的热点
  - 需要改 worker.rs 时改动要小而聚焦，并在提交说明里写清改了哪几行
  - 缓存结构必须能容纳 cover_handle 等字段（专辑页要用）

## 设计要求

### 缓存内容
- 设备信息：serial（缓存键）、model、firmware、storage（id/容量/可用）
- tracks：handle、filename、size、duration(ms)、title/artist/album/track_no
- albums：name、handle、track_handles、cover_handle
- playlists：handle、name、track handle 列表
- 封面图片：原始 JPEG 字节（按 handle 命名）或已解码缩略图

### 缓存位置与键
- 跨平台数据目录（macOS 用 ~/Library/Application Support/zune-gui/）
- 以**设备序列号**分目录，多设备互不干扰：
    data_dir/devices/SERIAL/library.json
    data_dir/devices/SERIAL/covers/HANDLE.jpg
- JSON 优先（易调试）；实测太大再考虑压缩

### 失效与刷新策略
- 缓存**不做**每次启动的在线校验（那还是要跑 2 分钟，等于没缓存）
- 提供：
  a) 启动即用缓存（秒开），UI 标注 cached · N min ago
  b) Refresh 按钮 = 强制重读设备并覆盖缓存
  c) 可选：缓存超过 TTL（比如 24h）时温和提示数据可能过期
- 设备序列号变化 = 换设备，自动切到对应缓存

### UI 要求（遵循 UI_DESIGN.md v1.1）
- 底部状态栏或顶栏加小标识：数据来源 + 相对时间（如 cached · 3 min ago），
  颜色用 text-2，不要抢眼
- Refresh 按钮语义明确为重新读取设备

## 为后续「Mac 与 Zune 同步机制」留接口

这一版只做**读缓存**，但结构要为将来的双向同步打基础：
- 缓存里为每个对象记录 handle + 关键属性 + 读取时间戳
- 设计成可 diff 的结构（本地库 vs 设备库），将来做增量同步/变更检测
- 预留字段记录上次完整同步时间

## 验收

1. 首次跑（无缓存）：正常读设备并写缓存，UI 显示 live
2. 第二次跑：**秒级**进入专辑页，UI 显示 cached · N min ago
3. 点 Refresh：重新读设备并更新缓存
4. 拔掉设备再启动：能用缓存展示上次的库（并提示设备未连接）
5. 封面也走缓存：第二次进入专辑页，封面应立即可见

## 交付

- 源码 + 交叉编译双后端到 dist/
- README 片段说明缓存位置、失效策略、如何清空
