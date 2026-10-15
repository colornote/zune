# 封面问题：关键诊断信息（Mac 端实测，用户急问）

用户问“封面还是没显示”。我关掉 GUI 直接用 CLI 查设备，得到关键事实：

## 一、设备上其实有 97 个 JPEG 封面对象

0x03000a0c  jpeg   44708 bytes  ht_080917_094308.jpg
0x03000a0f  jpeg  327794 bytes  1150c36ae72.jpg
0x03000a11  jpeg   58603 bytes  6f19aa3e2ff14c13b69d1ef9c7f7c5c7.jpg
0x03000a10  jpeg  184492 bytes  6651debb1389170c39bf2bdd1cb69a89.jpg
...（共 97 个对象，绝大多数是 jpeg，30~330KB，文件名是典型封面哈希）

## 二、handle 高位段编码了对象类型（Zune 特有）

  0x01 = 音频         （如 0x01001316）
  0x03 = 图片         （封面的真实位置！）
  0x06 = abstract album （如 0x060012c1）
  0x07 = playlist     （如 0x07000662）

## 三、但 GUI 日志显示 CoverArt folder index: 0 object(s)

说明查找位置/关联方式错了。封面既不在 CoverArt 文件夹，
也很可能不在 abstract album 的 forward refs 里。

## 四、核心假设（请优先验证）

关联可能是**反向的**：
  a) 封面 JPEG 对象自己的 refs 或 props 指向 album
  b) 通过某个设备属性 + 文件名/哈希匹配
  c) Zune 用 Association(0x3801) 之类的中间对象关联

## 五、请立刻做的诊断（不要依赖全库读取，现在太慢）

  1. dump 前 3 个 0x03 段 JPEG 的：object_info、所有属性、以及它自己的 object_refs
  2. dump 前 3 个 abstract album(0x06) 的：完整 refs 列表 + 每个 ref 的真实 format
     （重点看 refs 里有没有 0x03 段的 handle）
  3. 列出设备上所有文件夹（Association/Folder 格式）及内容数量，
     特别看有没有某个文件夹装着这 97 个 JPEG
  4. 顺便验证 GetThumb(0x100A) 对这些 JPEG 是否可用

诊断入口建议做成 Albums 页的 Diagnose albums 直接调用
（跳过全库读取，只读专辑 + 这几十个图片对象）。

## 六、另外

时长格式 bug（791:30:15 / 4299:29）也还在，一并修。
