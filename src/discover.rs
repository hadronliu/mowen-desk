//! 墨问「发现与动态」：调用墨问桌面客户端自带的 mocli（disco activity），
//! 覆盖 关注的人发新笔记/付费笔记、被点赞、被评论、被收藏、被关注 等事件。
//! 解析规则遵循墨问官方 mo-discover SKILL：以 reply.events 为主序列，
//! 用 eid 关联详情 Map，notes/users 补齐标题与昵称，缺信息时保留 ID 不丢事件。
//!
//! 注意：mocli discover 服务端限频严格（连续请求很快 429），调用方须控制频率。

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::mowen::FeedItem;

const DEFAULT_MOCLI_PATH: &str = "/Applications/墨问.app/Contents/Resources/bin/mocli";

fn default_mocli() -> String {
    DEFAULT_MOCLI_PATH.to_string()
}

/// 探测 mocli：优先配置路径 → PATH → 墨问.app 内置
pub fn resolve_mocli(configured: &str) -> Option<String> {
    let candidates: Vec<String> = if configured.is_empty() {
        vec!["mocli".into(), default_mocli()]
    } else {
        vec![configured.to_string()]
    };
    for c in candidates {
        if std::process::Command::new(&c)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return Some(c);
        }
    }
    None
}

/// 拉取动态并转为展示条目。`recent` 取 "1h".."24h" / today / yesterday。
pub async fn fetch_activity(mocli: &str, recent: &str) -> Result<Vec<FeedItem>> {
    let out = tokio::process::Command::new(mocli)
        .args(["disco", "activity", "--recent", recent])
        .output()
        .await
        .with_context(|| format!("执行 {mocli} 失败"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let val: Value = serde_json::from_str(stdout.trim())
        .with_context(|| format!("mocli 输出非 JSON：{}", &stdout[..stdout.len().min(200)]))?;
    if val["code"].as_i64() != Some(0) {
        bail!(
            "mocli 错误 {}: {} {}",
            val["code"].as_i64().unwrap_or(-1),
            val["reason"].as_str().unwrap_or("?"),
            val["msg"].as_str().unwrap_or("")
        );
    }
    Ok(parse_reply(&val["reply"]))
}

/// 按 mo-discover SKILL 的解析规则把 reply 转为条目
fn parse_reply(reply: &Value) -> Vec<FeedItem> {
    let empty = serde_json::Map::new();
    let users = reply["users"].as_object().unwrap_or(&empty);
    let notes = reply["notes"].as_object().unwrap_or(&empty);
    let comms = reply["comms"].as_object().unwrap_or(&empty);

    let name_of = |uid: &str| -> String {
        users
            .get(uid)
            .map(|u| u["name"].as_str().unwrap_or("").to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| uid_short(uid))
    };
    let title_of = |nid: &str| -> String {
        notes
            .get(nid)
            .map(|n| n["title"].as_str().unwrap_or("").to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| nid.to_string())
    };

    let mut items = Vec::new();
    if let Some(events) = reply["events"].as_array() {
        for ev in events {
            let eid = match ev["eid"].as_i64() {
                Some(v) => v.to_string(),
                None => match ev["eid"].as_str() {
                    Some(s) => s.to_string(),
                    None => continue,
                },
            };
            let etype = ev["type"].as_i64().unwrap_or(0);
            let ts_ms = normalize_ms(ev["happened_at"].as_i64().unwrap_or(0));

            let (display, note_id, actor) = match etype {
                1 => {
                    let d = &reply["favors"][eid.as_str()];
                    let actor = name_of(d["favor_uid"].as_str().unwrap_or(""));
                    let nid = d["target_id"].as_str().unwrap_or("").to_string();
                    (format!("赞 {actor} 赞了「{}」", title_of(&nid)), nid, actor)
                }
                2 => {
                    let d = &reply["comments"][eid.as_str()];
                    let actor = name_of(d["cmt_uid"].as_str().unwrap_or(""));
                    let nid = d["target_id"].as_str().unwrap_or("").to_string();
                    let cmt = comms
                        .get(d["cmt_id"].as_str().unwrap_or(""))
                        .and_then(|c| c["content_html"].as_str())
                        .map(strip_html)
                        .unwrap_or_default();
                    let head: String = cmt.chars().take(14).collect();
                    let prefix = if d["cmt_p_id"].as_str().unwrap_or("").is_empty() {
                        format!("评 {actor}「{}」", title_of(&nid))
                    } else {
                        format!("回 {actor}「{}」", title_of(&nid))
                    };
                    let display = if head.is_empty() { prefix } else { format!("{prefix} {head}…") };
                    (display, nid, actor)
                }
                3 => {
                    let d = &reply["collects"][eid.as_str()];
                    let actor = name_of(d["collect_uid"].as_str().unwrap_or(d["favor_uid"].as_str().unwrap_or("")));
                    let nid = d["target_id"].as_str().unwrap_or("").to_string();
                    (format!("藏 {actor} 收藏「{}」", title_of(&nid)), nid, actor)
                }
                4 => {
                    let actor = name_of(ev["uid"].as_str().unwrap_or(""));
                    (format!("关 {actor} 关注了你"), String::new(), actor)
                }
                5 => {
                    let actor = name_of(ev["uid"].as_str().unwrap_or(""));
                    (format!("售 {actor} 购买了你的内容"), String::new(), actor)
                }
                6 | 7 => {
                    let d = if etype == 6 {
                        &reply["follow_notes"][eid.as_str()]
                    } else {
                        &reply["follow_fee_notes"][eid.as_str()]
                    };
                    let actor = name_of(d["note_uid"].as_str().unwrap_or(""));
                    let nid = d["note_id"].as_str().unwrap_or("").to_string();
                    let tag = if etype == 7 { "付" } else { "新" };
                    (format!("{tag} {actor}「{}」", title_of(&nid)), nid, actor)
                }
                _ => (format!("他 动态({etype})"), String::new(), String::new()),
            };
            items.push(FeedItem {
                note_id,
                display,
                author_name: actor,
                created_at: ts_ms,
            });
        }
    }
    // 事件本身按时间序返回，稳妥起见再按时间排一次（新→旧）
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    items
}

/// uid 太长时截短展示（无昵称时的兜底）
fn uid_short(uid: &str) -> String {
    if uid.len() <= 8 {
        uid.to_string()
    } else {
        format!("{}…", &uid[..8])
    }
}

/// SKILL 提示时间戳可能秒/毫秒/微秒混杂，按数值长度归一为毫秒
fn normalize_ms(ts: i64) -> i64 {
    match ts {
        t if t <= 0 => 0,
        t if t < 10_000_000_000 => t * 1000,          // 秒
        t if t < 10_000_000_000_000 => t,             // 毫秒
        t => t / 1000,                                // 微秒
    }
}

/// 评论区 content_html 只留纯文本
fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&nbsp;", " ").replace("&amp;", "&").trim().to_string()
}
