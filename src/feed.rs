//! 动态流缓存：discover（mocli 发现动态）为主源，follows（关注作者公开笔记）为降级源。

use anyhow::Context as _;
use std::path::PathBuf;

use crate::config::FeedCfg;
use crate::mowen::{FeedItem, MowenClient};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct FeedStore {
    pub items: Vec<FeedItem>,
    pub updated_at: String,
    /// 本轮使用的源："discover" | "follows"
    pub source: String,
}

pub struct Feed {
    path: PathBuf,
    pub store: FeedStore,
}

impl Feed {
    pub fn load(data_dir: &str) -> Feed {
        let path = PathBuf::from(data_dir).join("feed.json");
        let store = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Feed { path, store }
    }

    fn save(&self) {
        if let Ok(s) = serde_json::to_string_pretty(&self.store) {
            let _ = std::fs::write(&self.path, s);
        }
    }

    pub fn len(&self) -> usize {
        self.store.items.len()
    }

    /// 刷新：按配置选源。
    /// - discover：调 mocli（覆盖关注更新+互动，服务端限频严格，调用方控制频率）
    /// - follows：逐作者拉公开笔记（OpenAPI）
    /// - auto：mocli 可用走 discover，否则 follows；discover 失败时若 follows 非空也可用
    pub async fn refresh(
        &mut self,
        client: &MowenClient,
        cfg: &FeedCfg,
        mocli: Option<&str>,
    ) -> anyhow::Result<(usize, String)> {
        let use_discover = match cfg.source.as_str() {
            "discover" => true,
            "follows" => false,
            _ => mocli.is_some(), // auto
        };

        if use_discover {
            let mocli = mocli.context("未找到 mocli（墨问桌面客户端），无法使用动态源")?;
            // 24h 在服务端有概率解析失败，逐级降范围（限频敏感，档间留 3 秒）
            for (i, recent) in ["24h", "today", "1h"].iter().enumerate() {
                if i > 0 {
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                }
                match crate::discover::fetch_activity(mocli, recent).await {
                    Ok(items) => {
                        let n = items.len();
                        self.store.items = items;
                        self.store.updated_at = chrono::Local::now().to_rfc3339();
                        self.store.source = "discover".into();
                        self.save();
                        return Ok((n, "discover".into()));
                    }
                    Err(e) => eprintln!("[feed] discover --recent {recent} 失败：{e}"),
                }
            }
            // discover 全部失败：若配置了 follows 则降级，否则保留旧数据报错
            if cfg.follows.is_empty() {
                anyhow::bail!("discover 连续失败且未配置 follows");
            }
            eprintln!("[feed] discover 失败，降级 follows 源");
        }

        let merged = self.refresh_follows(client, &cfg.follows, cfg.per_follow as u32).await;
        let n = merged.len();
        self.store.items = merged;
        self.store.updated_at = chrono::Local::now().to_rfc3339();
        self.store.source = "follows".into();
        self.save();
        Ok((n, "follows".into()))
    }

    /// 逐作者拉取公开笔记（限频 1 次/秒，作者间间隔 1.2s），合并去重按时间排序
    async fn refresh_follows(&self, client: &MowenClient, follows: &[String], per_follow: u32) -> Vec<FeedItem> {
        let mut merged: Vec<FeedItem> = Vec::new();
        for (i, uid) in follows.iter().enumerate() {
            if i > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
            }
            match client.author_public_notes(uid, per_follow).await {
                Ok((name, mut items)) => {
                    eprintln!("[feed] 作者「{name}」拉到 {} 条", items.len());
                    merged.append(&mut items);
                }
                Err(e) => eprintln!("[feed] 作者 {uid} 拉取失败：{e}"),
            }
        }
        let mut seen = std::collections::HashSet::new();
        merged.retain(|it| seen.insert(it.note_id.clone()));
        merged.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        merged
    }
}

/// 极简 HTML → 阅读文本（当前列表模式未用，留作后续「点开阅读」扩展）。
/// 处理：块级标签换行；<img>/<note> 转占位符；剥其余标签；解码常见实体。
#[allow(dead_code)]
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut chars = html.chars().peekable();
    let block_tags = ["p", "div", "br", "h1", "h2", "h3", "h4", "blockquote", "li", "tr", "pre"];
    while let Some(ch) = chars.next() {
        if ch != '<' {
            out.push(ch);
            continue;
        }
        // 收标签名
        let mut tag = String::new();
        let mut closing = false;
        if chars.peek() == Some(&'/') {
            closing = true;
            chars.next();
        }
        while let Some(&c) = chars.peek() {
            if c == '>' || c == ' ' || c == '/' {
                break;
            }
            tag.push(c);
            chars.next();
        }
        // 跳到标签结束
        while let Some(&c) = chars.peek() {
            chars.next();
            if c == '>' {
                break;
            }
        }
        let tag = tag.to_lowercase();
        if tag == "img" {
            out.push_str("\n[图片]\n");
        } else if tag == "note" {
            out.push_str(" [内链笔记] ");
        } else if block_tags.contains(&tag.as_str()) && !closing {
            // 连续换行折叠交给下游 split("\n\n")
            out.push_str("\n\n");
        }
    }
    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&ldquo;", "“")
        .replace("&rdquo;", "”")
        .replace("&hellip;", "…");
    // 折叠 3+ 连续换行为两个，去掉每行首尾空白
    let mut text = String::with_capacity(decoded.len());
    let mut blank = 0;
    for line in decoded.lines() {
        let t = line.trim();
        if t.is_empty() {
            blank += 1;
            if blank == 1 {
                text.push('\n');
            }
        } else {
            blank = 0;
            text.push_str(t);
            text.push('\n');
        }
    }
    text.trim().to_string()
}
