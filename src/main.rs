//! 墨问桌垫：极趣 Note 4 墨水屏 × 墨问。
//!
//! 白天是速记机：长按设备确认键说话 → 转写（可选 AI）→ 音频+文字发墨问。
//! 早上是晨报终端：定时生成晨报 → 发墨问存档 + 推墨水屏阅读。
//! 上下键翻页，确认键选择/返回。

mod archive;
mod config;
mod discover;
mod feed;
mod mowen;
mod noteatom;
mod render;
mod reporter;
mod transcribe;
mod voice;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use tokio::sync::{mpsc, Mutex};
use zectrix_sdk::{Device, Event, Key, KeyAction, OpenOptions};

use archive::{Archive, Entry, SerLine};
use config::Config;
use mowen::MowenClient;
use render::{RLine, Renderer};
use transcribe::Ai;

/// 发给设备会话的屏幕指令
enum ScreenCmd {
    Repaint,
    Toast(String),
    ShowLatest,
    FeedRefreshed,
}

#[allow(non_snake_case)]
static SCREEN_TX: std::sync::OnceLock<mpsc::Sender<ScreenCmd>> = std::sync::OnceLock::new();

fn notify(cmd: ScreenCmd) {
    if let Some(tx) = SCREEN_TX.get() {
        let _ = tx.try_send(cmd);
    }
}

/// Reader 打开前的来源（确认键按来源返回）
#[derive(Clone, Copy, PartialEq)]
enum Nav {
    Home,
    Feed,
}

/// 屏幕状态机
enum Mode {
    Home { items: Vec<Entry>, cursor: usize },
    /// 关注流列表：cursor 光标 / window 滚动窗口起点
    FeedList { cursor: usize, window: usize },
    Reader { title: String, lines: Vec<RLine>, page: usize, from: Nav },
    Listening,
}

struct App {
    cfg: Config,
    renderer: Renderer,
    mowen: MowenClient,
    ai: Option<Ai>,
    archive: Archive,
    feed: Mutex<feed::Feed>,
    /// 已解析的 mocli 路径（动态源）
    mocli: Option<String>,
    /// 上次手动刷新时间（discover 限频保护）
    last_feed_refresh: Mutex<Option<std::time::Instant>>,
    ui: Mutex<Mode>,
}

impl App {
    fn home_items(&self) -> Vec<Entry> {
        self.archive.list_recent(8)
    }

    async fn render_current(&self) -> Vec<u8> {
        let ui = self.ui.lock().await;
        let now = chrono::Local::now().format("%H:%M").to_string();
        match &*ui {
            Mode::Home { items, cursor } => {
                let feed_n = self.feed.lock().await.len();
                let mut lines = vec![RLine::title("墨问桌垫")];
                let feed_enabled = self.cfg.feed.enabled;
                let mut base = 0usize;
                if feed_enabled {
                    lines.push(RLine::item(format!("动态 · {} 条", feed_n), *cursor == 0));
                    base = 1;
                }
                let show: Vec<&Entry> = if items.is_empty() && feed_enabled {
                    vec![]
                } else {
                    items.iter().collect()
                };
                if show.is_empty() && feed_enabled {
                    // 只有资讯入口时不再显示引导长文
                    if feed_n == 0 {
                        lines.push(RLine::normal("资讯还没拉取，稍等片刻"));
                    }
                } else if show.is_empty() {
                    lines.push(RLine::normal("还没有内容。"));
                    lines.push(RLine::normal("长按设备确认键 1 秒说话，"));
                    lines.push(RLine::normal("松开即自动发到墨问。"));
                } else {
                    for (i, e) in show.iter().enumerate() {
                        let dt = chrono::DateTime::parse_from_rfc3339(&e.created_at)
                            .map(|d| d.format("%m-%d %H:%M").to_string())
                            .unwrap_or_default();
                        lines.push(RLine::item(format!("{dt} {}", e.title), *cursor == base + i));
                    }
                }
                let quota = self.archive.today_create_count();
                lines.push(RLine::dim(format!(
                    "今日已发 {}/{} · 上下移动 确认打开",
                    quota, self.cfg.mowen.daily_quota
                )));
                self.renderer.draw_page(&lines, "墨问桌垫", &now, 0, 1)
            }
            Mode::FeedList { cursor, window } => {
                let f = self.feed.lock().await;
                let n = f.store.items.len();
                let src = if f.store.source == "discover" { "动态" } else { "关注" };
                let mut lines = vec![RLine::title(format!("发现 · {src}"))];
                if n == 0 {
                    lines.push(RLine::normal("还没有内容。"));
                    lines.push(RLine::normal("稍后自动刷新，或选择刷新。"));
                }
                let page_size = 7usize;
                let end = (*window + page_size).min(n);
                for (i, it) in f.store.items[*window..end].iter().enumerate() {
                    let dt = chrono::DateTime::from_timestamp_millis(it.created_at)
                        .map(|d| d.format("%m-%d").to_string())
                        .unwrap_or_default();
                    lines.push(RLine::item(
                        format!("{} {}", it.display, dt),
                        *window + i == *cursor,
                    ));
                }
                lines.push(RLine::item("刷新动态", *cursor == n));
                let updated = chrono::DateTime::parse_from_rfc3339(&f.store.updated_at)
                    .map(|d| d.format("%H:%M").to_string())
                    .unwrap_or_else(|_| "--:--".into());
                lines.push(RLine::dim(format!("{n} 条 · 更新 {updated} · 确认回主页")));
                self.renderer.draw_page(&lines, "发现", &now, 0, 1)
            }
            Mode::Reader { title, lines, page, .. } => {
                let pages = self.renderer.paginate(lines);
                let idx = (*page).min(pages.len().saturating_sub(1));
                self.renderer.draw_page(&pages[idx], title, &now, idx, pages.len())
            }
            Mode::Listening => {
                let lines = vec![
                    RLine::title("正在聆听…"),
                    RLine::normal("松开确认键结束录音"),
                    RLine::dim("内容将自动发到墨问"),
                ];
                self.renderer.draw_page(&lines, "速记", &now, 0, 1)
            }
        }
    }
}

async fn repaint(app: &Arc<App>, device: &Arc<Device>) {
    let png = app.render_current().await;
    // 录音收尾/连续推图时设备会短暂 Busy，小退避重试
    for attempt in 0..3 {
        match device.show_with_format(&png, app.cfg.render.format()).await {
            Ok(()) => return,
            Err(zectrix_sdk::Error::Busy) => {
                eprintln!("[screen] 设备忙碌，第 {} 次重试", attempt + 1);
                tokio::time::sleep(Duration::from_millis(900)).await;
            }
            Err(e) => {
                eprintln!("[screen] 推图失败：{e}");
                return;
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = Config::load()?;
    std::fs::create_dir_all(&cfg.app.data_dir).ok();

    let archive = Archive::new(&cfg.app.data_dir)?;
    let home_items = archive.list_recent(8);
    let mocli = discover::resolve_mocli(&cfg.feed.mocli_path);
    if cfg.feed.enabled {
        match &mocli {
            Some(p) => eprintln!("[feed] 动态源 mocli：{p}"),
            None => eprintln!("[feed] 未找到 mocli，动态将降级为 follows 源（若配置）"),
        }
    }
    let app = Arc::new(App {
        renderer: Renderer::load(&cfg.render.font_path, cfg.render.font_size)
            .context("初始化字体失败")?,
        mowen: MowenClient::new(&cfg.mowen.base_url, &cfg.mowen.api_key),
        ai: if cfg.ai.usable() { Some(Ai::new(cfg.ai.clone())) } else { None },
        archive,
        feed: Mutex::new(feed::Feed::load(&cfg.app.data_dir)),
        mocli,
        last_feed_refresh: Mutex::new(None),
        ui: Mutex::new(Mode::Home { items: home_items, cursor: 0 }),
        cfg,
    });

    if app.ai.is_none() {
        eprintln!("[app] 未配置 [ai]：速记不转写（只传音频附件），晨报用本地模板");
    }

    let (screen_tx, mut screen_rx) = mpsc::channel::<ScreenCmd>(16);
    let (ev_tx, mut ev_rx) = mpsc::channel::<Event>(16);
    let _ = SCREEN_TX.set(screen_tx.clone());

    // 晨报任务
    if app.cfg.morning.enabled {
        let app2 = app.clone();
        tokio::spawn(async move { morning_loop(app2).await });
    }

    // 资讯流任务：启动即拉一次，之后按间隔刷新
    if app.cfg.feed.enabled {
        let app2 = app.clone();
        tokio::spawn(async move { feed_loop(app2).await });
    }

    // 设备会话（含断线重连）
    let mut backoff = Duration::from_secs(5);
    loop {
        let target = app.cfg.zectrix.target.trim().to_string();
        let opts = OpenOptions {
            target: if target.is_empty() { None } else { Some(target) },
            client_name: Some(app.cfg.zectrix.client_name.clone()),
        };
        match Device::open(opts).await {
            Ok(device) => {
                let device = Arc::new(device);
                eprintln!("[device] 已连接：{}", device.summary.name);
                backoff = Duration::from_secs(5);
                // 事件泵：唯一的 next_event 消费者
                let dev2 = device.clone();
                let ev_tx2 = ev_tx.clone();
                let pump = tokio::spawn(async move {
                    loop {
                        match dev2.next_event().await {
                            Ok(Some(ev)) => {
                                if ev_tx2.send(ev).await.is_err() {
                                    break;
                                }
                            }
                            Ok(None) | Err(_) => break,
                        }
                    }
                });
                repaint(&app, &device).await;
                let session = session_loop(&app, &device, &mut ev_rx, &mut screen_rx).await;
                let _ = device.close().await;
                pump.abort();
                match session {
                    Ok(()) => eprintln!("[device] 会话结束，准备重连"),
                    Err(e) => eprintln!("[device] 会话异常：{e}"),
                }
            }
            Err(e) => {
                eprintln!("[device] 连接失败：{e}，{} 秒后重试", backoff.as_secs());
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = tokio::signal::ctrl_c() => {
                eprintln!("[app] Ctrl-C，退出");
                return Ok(());
            }
        }
        backoff = (backoff * 2).min(Duration::from_secs(60));
    }
}

/// 单次连接内的交互循环
async fn session_loop(
    app: &Arc<App>,
    device: &Arc<Device>,
    ev_rx: &mut mpsc::Receiver<Event>,
    screen_rx: &mut mpsc::Receiver<ScreenCmd>,
) -> Result<()> {
    loop {
        tokio::select! {
            ev = ev_rx.recv() => {
                let Some(ev) = ev else { anyhow::bail!("事件泵退出") };
                match ev {
                    Event::Key { key, action } => {
                        // 确认长按由固件录音流程消费；上下长按当普通翻页
                        if action == KeyAction::Long && key == Key::Confirm {
                            continue;
                        }
                        {
                            let mut ui = app.ui.lock().await;
                            match &mut *ui {
                                Mode::Home { items, cursor } => {
                                    // 列表 = [资讯入口] + 留档条目；last 为最大合法下标
                                    let last = if app.cfg.feed.enabled { items.len() } else { items.len().saturating_sub(1) };
                                    match key {
                                        Key::Up => {
                                            if *cursor > 0 { *cursor -= 1; }
                                        }
                                        Key::Down => {
                                            if *cursor + 1 <= last { *cursor += 1; }
                                        }
                                        _ => {
                                            if app.cfg.feed.enabled && *cursor == 0 {
                                                *ui = Mode::FeedList { cursor: 0, window: 0 };
                                            } else {
                                                let idx = if app.cfg.feed.enabled { *cursor - 1 } else { *cursor };
                                                if let Some(e) = items.get(idx) {
                                                    let lines: Vec<RLine> =
                                                        e.lines.iter().map(|l| l.to_rline()).collect();
                                                    *ui = Mode::Reader { title: e.title.clone(), lines, page: 0, from: Nav::Home };
                                                }
                                            }
                                        }
                                    }
                                }
                                Mode::FeedList { cursor, window } => {
                                    let feed_len = app.feed.lock().await.len();
                                    let last = feed_len; // 刷新项下标
                                    let page_size = 7usize;
                                    match key {
                                        Key::Up => {
                                            if *cursor > 0 {
                                                *cursor -= 1;
                                                if *cursor < *window {
                                                    *window = *cursor;
                                                }
                                            }
                                        }
                                        Key::Down => {
                                            if *cursor + 1 <= last {
                                                *cursor += 1;
                                                if *cursor >= *window + page_size {
                                                    *window = *cursor + 1 - page_size;
                                                }
                                            }
                                        }
                                        _ => {
                                            if *cursor == last {
                                                // 刷新项
                                                let app2 = app.clone();
                                                tokio::spawn(async move {
                                                    refresh_feed(app2, true).await;
                                                });
                                                *ui = Mode::Reader {
                                                    title: "刷新中…".into(),
                                                    lines: vec![RLine::normal("正在拉取关注作者的更新")],
                                                    page: 0,
                                                    from: Nav::Feed,
                                                };
                                            } else {
                                                // 确认键返回主页
                                                *ui = Mode::Home { items: app.home_items(), cursor: 0 };
                                            }
                                        }
                                    }
                                }
                                Mode::Reader { page, from, .. } => match key {
                                    Key::Up => {
                                        if *page > 0 { *page -= 1; }
                                    }
                                    Key::Down => *page += 1,
                                    _ => {
                                        *ui = match from {
                                            Nav::Home => Mode::Home { items: app.home_items(), cursor: 0 },
                                            Nav::Feed => Mode::FeedList { cursor: 0, window: 0 },
                                        };
                                    }
                                },
                                Mode::Listening => {}
                            }
                        }
                        repaint(app, device).await;
                    }
                    Event::RecordingStarted => {
                        *app.ui.lock().await = Mode::Listening;
                        repaint(app, device).await;
                    }
                    Event::RecordingFinished { audio, .. } => {
                        let app2 = app.clone();
                        tokio::spawn(async move {
                            handle_recording(app2, audio.map(|c| c.samples)).await;
                        });
                    }
                    Event::Disconnected { reason } => {
                        anyhow::bail!("设备断开：{reason:?}");
                    }
                }
            }
            cmd = screen_rx.recv() => {
                let Some(cmd) = cmd else { return Ok(()) };
                match cmd {
                    ScreenCmd::Repaint => repaint(app, device).await,
                    ScreenCmd::Toast(msg) => {
                        let prev_mode = {
                            let mut ui = app.ui.lock().await;
                            std::mem::replace(
                                &mut *ui,
                                Mode::Reader {
                                    title: "消息".into(),
                                    lines: vec![RLine::title(msg)],
                                    page: 0,
                                    from: Nav::Home,
                                },
                            )
                        };
                        repaint(app, device).await;
                        let app2 = app.clone();
                        let dev2 = device.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_millis(3500)).await;
                            *app2.ui.lock().await = prev_mode;
                            repaint(&app2, &dev2).await;
                        });
                    }
                    ScreenCmd::ShowLatest => {
                        let items = app.home_items();
                        if let Some(e) = items.first() {
                            let lines: Vec<RLine> = e.lines.iter().map(|l| l.to_rline()).collect();
                            *app.ui.lock().await = Mode::Reader { title: e.title.clone(), lines, page: 0, from: Nav::Home };
                        }
                        repaint(app, device).await;
                    }
                    ScreenCmd::FeedRefreshed => {
                        *app.ui.lock().await = Mode::FeedList { cursor: 0, window: 0 };
                        repaint(app, device).await;
                    }
                }
            }
        }
    }
}

fn write_wav<'a>(path: &'a std::path::Path, wav: &[u8]) -> std::io::Result<&'a std::path::Path> {
    std::fs::write(path, wav)?;
    Ok(path)
}

/// 语音速记链路：录音 →（转写+整理）→ 音频上传 → 创建墨问笔记 → 留档 → 屏幕反馈
async fn handle_recording(app: Arc<App>, samples: Option<Vec<i16>>) {
    let Some(samples) = samples else {
        notify(ScreenCmd::Toast("没收到录音内容".into()));
        return;
    };
    if samples.len() < 16000 / 4 {
        notify(ScreenCmd::Toast("录音太短，已忽略".into()));
        return;
    }

    // 配额预检
    let used = app.archive.today_create_count();
    if used >= app.cfg.mowen.daily_quota {
        notify(ScreenCmd::Toast(format!("今日墨问配额已用满 {}/{}", used, app.cfg.mowen.daily_quota)));
        return;
    }

    let dur = voice::fmt_duration(samples.len(), 16000);
    let wav = voice::pcm16_to_wav(&samples, 16000);
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let wav_name = format!("速记-{stamp}.wav");

    // 1. 转写 + 整理（AI 可选，失败降级）
    let (title, body_text) = match app.ai.as_ref().filter(|a| a.asr_usable()) {
        Some(ai) => match ai.transcribe(wav.clone(), &wav_name).await {
            Ok(t) if !t.trim().is_empty() => {
                let digest = if ai.chat_usable() { ai.digest_quick_note(&t).await.ok() } else { None };
                match digest {
                    Some(d) => (d.title, d.body_text),
                    None => (format!("速记 {stamp}"), t),
                }
            }
            _ => (format!("速记 {stamp}"), String::new()),
        },
        None => (format!("速记 {stamp}"), String::new()),
    };

    // 2. 上传音频附件（WAV 先转 M4A：墨问 policy 不收 wav；失败降级纯文字并留底）
    let audio_atom = {
        let wav_path = std::path::Path::new(&app.cfg.app.data_dir)
            .join("tmp")
            .join(format!("速记-{stamp}.wav"));
        let _ = std::fs::create_dir_all(wav_path.parent().unwrap());
        let upload_result = async {
            let (bytes, name, mime) = match voice::transcode_to_m4a(write_wav(&wav_path, &wav)?) {
                Some(m4a) => (m4a, format!("速记-{stamp}.m4a"), "audio/mp4"),
                None => {
                    eprintln!("[voice] 转码失败，尝试直接上传 wav（可能被 policy 拒绝）");
                    (wav.clone(), wav_name.clone(), "audio/wav")
                }
            };
            app.mowen.upload_local(2, &name, bytes, mime).await
        };
        match upload_result.await {
            Ok(f) => {
                eprintln!("[voice] 音频上传成功 file_id={}", f.file_id);
                let _ = std::fs::remove_file(&wav_path);
                noteatom::audio_block(&f.file_id, None)
            }
            Err(e) => {
                eprintln!("[voice] 音频上传失败（降级纯文字）：{e}");
                let dir = std::path::Path::new(&app.cfg.app.data_dir).join("failed");
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::fs::write(dir.join(&wav_name), &wav);
                let _ = std::fs::remove_file(&wav_path);
                serde_json::Value::Null
            }
        }
    };

    // 3. 组装 doc：音频节点在最前，正文段落随后
    //    注意：墨问后端要求笔记至少含一个文本节点（纯音频节点会被判 note empty）
    let mut content = Vec::new();
    if !audio_atom.is_null() {
        content.push(audio_atom.clone());
    }
    if body_text.trim().is_empty() {
        let hint = if audio_atom.is_null() {
            format!("[语音速记] 时长 {dur}，音频上传失败，原文件已留在本机 data/failed/")
        } else {
            format!("[语音速记] {stamp} · 时长 {dur}（未配置 AI 转写，点击上方音频收听）")
        };
        content.push(noteatom::block_value(&noteatom::Block::Para(vec![noteatom::Inline::Text(
            hint,
        )])));
    } else {
        for para in body_text.split("\n\n") {
            let inlines: Vec<_> = para.lines().map(|l| noteatom::Inline::Text(l.to_string())).collect();
            if !inlines.is_empty() {
                content.push(noteatom::block_value(&noteatom::Block::Para(inlines)));
            }
        }
    }
    let body = serde_json::json!({ "type": "doc", "content": content });
    finish_create(&app, &title, body, &body_text, "quick").await;
}

async fn finish_create(app: &Arc<App>, title: &str, body: serde_json::Value, body_text: &str, kind: &str) {
    let tags = if kind == "morning" { &app.cfg.mowen.tags_morning } else { &app.cfg.mowen.tags_quick };
    let note_id = match app.mowen.create_note(&body, app.cfg.mowen.auto_publish, tags).await {
        Ok(id) => id,
        Err(e) => {
            eprintln!("[mowen] 创建失败：{e:#}");
            notify(ScreenCmd::Toast(format!("✗ 发布失败：{e}")));
            return;
        }
    };
    app.archive.incr_quota();

    // 屏幕阅读版：标题 + 摘要
    let mut lines = vec![RLine::title(title)];
    if !body_text.trim().is_empty() {
        for para in body_text.split("\n\n").take(3) {
            lines.push(RLine::normal(para));
        }
    }
    lines.push(RLine::dim("已同步到墨问"));

    let entry = Entry {
        note_id,
        kind: kind.to_string(),
        title: title.to_string(),
        created_at: chrono::Local::now().to_rfc3339(),
        lines: lines.iter().map(SerLine::from).collect(),
    };
    if let Err(e) = app.archive.save(&entry) {
        eprintln!("[archive] 留档失败：{e}");
    }
    eprintln!("[mowen] 已发布：{title} noteId={}", entry.note_id);
    // Toast 之后 3.5s 会自动 repaint 回主页，此时留档已落盘，列表自然包含新条目
    notify(ScreenCmd::Toast(format!("✓ 已收录：{title}")));
}

/// 晨报循环
async fn morning_loop(app: Arc<App>) {
    loop {
        let (h, m) = parse_hm(&app.cfg.morning.time);
        let now = chrono::Local::now();
        let mut next = now
            .date_naive()
            .and_hms_opt(h, m, 0)
            .expect("时间合法")
            .and_local_timezone(chrono::Local)
            .single()
            .unwrap_or(now);
        if next <= now {
            next += chrono::Duration::days(1);
        }
        let wait = (next - chrono::Local::now()).to_std().unwrap_or(Duration::from_secs(60));
        eprintln!("[morning] 下一次晨报：{}（{} 秒后）", next, wait.as_secs());
        tokio::time::sleep(wait).await;

        if app.archive.today_create_count() >= app.cfg.mowen.daily_quota {
            eprintln!("[morning] 今日配额已满，跳过");
            continue;
        }

        let ctx = reporter::MorningCtx {
            latitude: app.cfg.morning.latitude,
            longitude: app.cfg.morning.longitude,
            ai_digest: app.cfg.morning.ai_digest,
            ai: app.ai.as_ref(),
        };
        let report = reporter::build(&ctx).await;
        let body = noteatom::doc(&report.blocks);
        let note_id = match app
            .mowen
            .create_note(&body, app.cfg.mowen.auto_publish, &app.cfg.mowen.tags_morning)
            .await
        {
            Ok(id) => id,
            Err(e) => {
                eprintln!("[morning] 发布失败：{e:#}");
                notify(ScreenCmd::Toast("✗ 晨报发布失败".into()));
                continue;
            }
        };
        app.archive.incr_quota();
        let entry = Entry {
            note_id,
            kind: "morning".into(),
            title: report.title.clone(),
            created_at: chrono::Local::now().to_rfc3339(),
            lines: report.lines.iter().map(SerLine::from).collect(),
        };
        let _ = app.archive.save(&entry);
        eprintln!("[morning] 晨报已发布：{}", report.title);
        notify(ScreenCmd::ShowLatest);
    }
}

/// 拉取动态流。manual=true 表示用户在设备上主动触发（完成后回列表）；
/// discover 源限频严格，手动刷新间隔不足 5 分钟时直接提示。
async fn refresh_feed(app: Arc<App>, manual: bool) {
    if manual {
        let mut last = app.last_feed_refresh.lock().await;
        if let Some(t) = *last {
            let elapsed = t.elapsed().as_secs() / 60;
            if elapsed < 5 {
                notify(ScreenCmd::Toast(format!("刚刷过（{elapsed} 分钟前），稍后再试")));
                return;
            }
        }
        *last = Some(std::time::Instant::now());
    }
    match app
        .feed
        .lock()
        .await
        .refresh(&app.mowen, &app.cfg.feed, app.mocli.as_deref())
        .await
    {
        Ok((n, src)) => {
            eprintln!("[feed] 刷新完成（{src}），共 {n} 条");
            notify(if manual { ScreenCmd::FeedRefreshed } else { ScreenCmd::Repaint });
        }
        Err(e) => {
            eprintln!("[feed] 刷新失败：{e}");
            if manual {
                notify(ScreenCmd::Toast(format!("✗ 刷新失败：{e}")));
            }
        }
    }
}

/// 资讯流定时任务：启动即拉一次，之后按间隔刷新
async fn feed_loop(app: Arc<App>) {
    loop {
        refresh_feed(app.clone(), false).await;
        tokio::time::sleep(Duration::from_secs(app.cfg.feed.refresh_minutes * 60)).await;
    }
}

fn parse_hm(s: &str) -> (u32, u32) {
    let mut it = s.split(':');
    let h: u32 = it.next().and_then(|x| x.trim().parse().ok()).unwrap_or(7);
    let m: u32 = it.next().and_then(|x| x.trim().parse().ok()).unwrap_or(0);
    (h.min(23), m.min(59))
}
