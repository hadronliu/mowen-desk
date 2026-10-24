# 墨问桌垫（mowen-desk）

极趣 ZECTRIX Note 4 墨水屏 × 墨问笔记：**白天是语音速记机，早上是晨报终端**。

- **速记**：在设备上**长按确认键 1 秒**开始说话，松开 → 自动转写（可选 AI）→ 音频附件 + 文字发到墨问笔记
- **晨报**：每天定时生成（天气 + 一言 + 可选 AI 小语）→ 发墨问存档 + 墨水屏阅读
- **阅读**：上下键翻页 / 移动光标，确认键打开 / 返回，屏幕可回读所有经本程序发布的内容（墨问 API 不支持读取，本地 `data/` 留档即回读来源）

## 工作原理

```
                    ┌────────────────────────────┐
   长按确认键说话 → │  电脑上的 mowen-desk 常驻程序  │ ← open-meteo / hitokoto / AI(可选)
   上下键翻页  ←──  │  转写 · 上传 · 渲染 400×300   │
                    └──────┬──────────────┬─────┘
                局域网推图  │              │ HTTP
                           ▼              ▼
                    极趣 Note 4        墨问 OpenAPI
                    (400×300 墨水屏)    (open.mowen.cn)
```

设备端不跑任何应用——所有逻辑在这台电脑上，程序退出后屏幕停留在最后一帧，但无法交互。

## 准备

1. **极趣 Note 4**：固件 3.7.2（极趣云 → 本地升级），设置 → 网络同步 → 打开「待机保持 Wi-Fi」，与电脑同一局域网
2. **墨问 Pro 会员 + API-KEY**：墨问小程序内生成
3. **Rust stable**（rustup）、C 编译器与 CMake（SDK 原生 Opus 编译需要；macOS 装 Xcode Command Line Tools 即可）
4. （可选）任意 OpenAI 兼容的 AI 服务，用于语音转写与标题整理

## 安装与运行

```sh
cd mowen-desk
cp config.example.toml config.toml
# 编辑 config.toml：至少填 [mowen].api_key
cargo run --release
```

启动后自动发现局域网内的 Note 4（也可在 `[zectrix].target` 填 IP 或设备 ID 加速）。屏幕出现「墨问桌垫」主页即成功。

### 日常使用

| 操作 | 效果 |
| --- | --- |
| 长按确认键 1 秒，说话，松开 | 语音速记 → 自动发墨问 → 屏幕提示「已收录」 |
| 上 / 下键 | 主页移动光标；阅读时翻页 |
| 确认键 | 主页打开选中条目；阅读时返回主页 |

### 配置 AI（可选，强烈推荐）

```toml
[ai]
# 智谱示例（glm-asr 转写 + glm-4-flash 整理）
base_url = "https://open.bigmodel.cn/api/paas/v4"
api_key = "你的key"
chat_model = "glm-4-flash"
asr_model = "glm-asr"

# OpenAI 示例
# base_url = "https://api.openai.com/v1"
# chat_model = "gpt-4o-mini"
# asr_model = "whisper-1"
```

不配 AI：速记只上传音频附件（笔记里能听，没有自动文字），晨报走本地模板。

## 数据与配额

- 留档在 `data/archive/YYYY-MM/…json`（含屏幕阅读版行），配额计数在 `data/quota.json`，上传失败的原始终音存 `data/failed/`
- 墨问创建接口限频 1 次/秒，配额默认 10 次/天（Pro 100）——程序在屏幕主页实时显示「今日已发 n/quota」，配额满时长按说话只提示不发送
- 上传配额（prepare/URL）默认 20 次/天（Pro 200），每条速记消耗 1 次

## 已知边界（重要）

- **墨问 API 是纯写入通道**：无法读取你在小程序里手写的笔记；本程序只能回读经它发布的内容（本地留档）
- **本地上传的投递细节未在墨问文档公开**（form 键名/投递响应结构）：代码用宽松策略解析（http 开头的值作为端点、fileId 多键名匹配）。首次联调若音频上传失败，看终端里 `[mowen] 投递响应` 一行，把实际的 form / 响应贴回来即可适配
- 同一设备同时只能有一个管理方：跑本程序时不要同时用极趣官方应用的远程功能
- 程序异常退出时，设备约 15 秒后自动释放连接；正常 Ctrl-C 会主动 close

## 项目结构

```
src/
├── main.rs        交互主循环：连接/按键/录音事件/晨报定时/断线重连
├── config.rs      config.toml 加载
├── mowen.rs       墨问 OpenAPI 客户端（创建/上传两步）
├── noteatom.rs    NoteAtom 笔记结构构造
├── transcribe.rs  OpenAI 兼容 AI（转写/整理/晨报小语），可降级
├── reporter.rs    晨报内容生成（open-meteo + hitokoto）
├── render.rs      400×300 排版渲染：字体探测/折行/分页/PNG
├── archive.rs     本地留档与配额计数
└── voice.rs       PCM16 → WAV
vendor/zectrix-local-push-sdk/   极趣 LAN SDK（路径依赖）
```

## 测试与调试

```sh
cargo test            # 渲染/折行/分页单元测试（会生成 /tmp/mowen-desk-test.png 可肉眼检查）
cargo run --release   # 运行
```

- 屏幕字体不对：`[render].font_path` 指定一个 ttf/ttc
- 排版过密/过疏：调 `[render].font_size`（默认 20）
- 灰阶：`[render].image_format`，黑白正文推荐 `mono` 或 `gray4`
