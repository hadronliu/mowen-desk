//! 本地留档：所有成功发到墨问的内容在本地留一份（墨问 API 无法读取，这里是"回读"的唯一来源）。
//!
//! data/
//! ├── archive/2026-10/20261024-073000-晨报.json
//! └── quota.json   { "date": "2026-10-24", "count": 3 }

use anyhow::Result;
use chrono::Local;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::render::RLine;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub note_id: String,
    pub kind: String,       // quick / morning
    pub title: String,
    pub created_at: String, // RFC3339
    /// 屏幕阅读版行
    pub lines: Vec<SerLine>,
}

/// RLine 的可序列化形态（render::RLine 不上 serde，这里转一层）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerLine {
    pub kind: String,
    pub text: String,
}

impl From<&RLine> for SerLine {
    fn from(l: &RLine) -> Self {
        SerLine {
            kind: match l.kind {
                crate::render::LineKind::Title => "title",
                crate::render::LineKind::Normal => "normal",
                crate::render::LineKind::Quote => "quote",
                crate::render::LineKind::Dim => "dim",
                crate::render::LineKind::Item => "normal",
            }
            .to_string(),
            text: l.plain_text(),
        }
    }
}

impl SerLine {
    pub fn to_rline(&self) -> RLine {
        match self.kind.as_str() {
            "title" => RLine::title(&self.text),
            "quote" => RLine { kind: crate::render::LineKind::Quote, spans: vec![crate::render::Span::t(&self.text)] },
            "dim" => RLine::dim(&self.text),
            _ => RLine::normal(&self.text),
        }
    }
}

pub struct Archive {
    root: PathBuf,
}

impl Archive {
    pub fn new(data_dir: &str) -> Result<Archive> {
        let root = Path::new(data_dir);
        std::fs::create_dir_all(root.join("archive"))?;
        Ok(Archive { root: root.to_path_buf() })
    }

    fn month_dir(&self) -> PathBuf {
        let m = Local::now().format("%Y-%m");
        let d = self.root.join("archive").join(m.to_string());
        let _ = std::fs::create_dir_all(&d);
        d
    }

    pub fn save(&self, e: &Entry) -> Result<()> {
        let stamp = Local::now().format("%Y%m%d-%H%M%S");
        let safe: String = e.title.chars().take(12).collect::<String>()
            .chars().map(|c| if c.is_whitespace() || matches!(c, '/' | '\\') { '_' } else { c }).collect();
        let path = self.month_dir().join(format!("{stamp}-{safe}.json"));
        std::fs::write(&path, serde_json::to_string_pretty(e)?)?;
        Ok(())
    }

    /// 最近 n 条（新→旧）
    pub fn list_recent(&self, n: usize) -> Vec<Entry> {
        let mut files: Vec<PathBuf> = Vec::new();
        if let Ok(months) = std::fs::read_dir(self.root.join("archive")) {
            let mut mdirs: Vec<_> = months.flatten().map(|e| e.path()).collect();
            mdirs.sort();
            for md in mdirs.iter().rev() {
                if let Ok(fs) = std::fs::read_dir(md) {
                    let mut fs: Vec<_> = fs.flatten().map(|e| e.path()).collect();
                    fs.sort();
                    files.extend(fs.into_iter().rev());
                }
                if files.len() >= n * 2 {
                    break;
                }
            }
        }
        files
            .into_iter()
            .filter_map(|p| {
                std::fs::read_to_string(&p)
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
            })
            .take(n)
            .collect()
    }

    pub fn today_create_count(&self) -> u32 {
        let path = self.root.join("quota.json");
        #[derive(Deserialize)]
        struct Q { date: String, count: u32 }
        if let Ok(s) = std::fs::read_to_string(&path) {
            if let Ok(q) = serde_json::from_str::<Q>(&s) {
                if q.date == Local::now().format("%Y-%m-%d").to_string() {
                    return q.count;
                }
            }
        }
        0
    }

    pub fn incr_quota(&self) {
        let today = Local::now().format("%Y-%m-%d").to_string();
        let count = self.today_create_count() + 1;
        let _ = std::fs::write(
            self.root.join("quota.json"),
            serde_json::to_string(&serde_json::json!({ "date": today, "count": count })).unwrap(),
        );
    }

    /// 按 noteId 删除本地留档（只影响屏幕列表；墨问里的笔记需在小程序手动删）
    pub fn delete_by_note_id(&self, note_id: &str) -> bool {
        let mut removed = false;
        if let Ok(months) = std::fs::read_dir(self.root.join("archive")) {
            for md in months.flatten() {
                if let Ok(files) = std::fs::read_dir(md.path()) {
                    for f in files.flatten() {
                        let path = f.path();
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            if let Ok(e) = serde_json::from_str::<Entry>(&content) {
                                if e.note_id == note_id {
                                    if std::fs::remove_file(&path).is_ok() {
                                        removed = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        removed
    }
}
