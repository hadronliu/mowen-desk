//! NoteAtom（墨问笔记原子结构）构造辅助。
//! 参考 https://mowen.apifox.cn/6682171m0.md

use serde_json::{json, Value};

/// 行内富文本片段
#[derive(Debug, Clone)]
#[allow(dead_code)] // Link/Code 等变体供后续笔记类型使用
pub enum Inline {
    Text(String),
    Bold(String),
    Highlight(String),
    Link { text: String, href: String },
    Code(String),
}

/// 块级内容
#[derive(Debug, Clone)]
pub enum Block {
    Title(u8, String),      // level 1..=3
    Para(Vec<Inline>),
    Quote(Vec<Inline>),
    Spacer,                 // 空段落，段间分隔
}

fn inline_to_atom(i: &Inline) -> Value {
    match i {
        Inline::Text(t) => json!({ "type": "text", "text": t }),
        Inline::Bold(t) => json!({
            "type": "text",
            "text": t,
            "marks": [ { "type": "bold" } ]
        }),
        Inline::Highlight(t) => json!({
            "type": "text",
            "text": t,
            "marks": [ { "type": "highlight" } ]
        }),
        Inline::Link { text, href } => json!({
            "type": "text",
            "text": text,
            "marks": [ { "type": "link", "attrs": { "href": href } } ]
        }),
        Inline::Code(t) => json!({
            "type": "text",
            "text": t,
            "marks": [ { "type": "code" } ]
        }),
    }
}

fn block_to_atom(b: &Block) -> Value {
    match b {
        Block::Spacer => json!({ "type": "paragraph" }),
        Block::Title(level, text) => json!({
            "type": "heading",
            "attrs": { "level": level },
            "content": [ { "type": "text", "text": text } ]
        }),
        Block::Para(inlines) => json!({
            "type": "paragraph",
            "content": inlines.iter().map(inline_to_atom).collect::<Vec<_>>()
        }),
        Block::Quote(inlines) => json!({
            "type": "quote",
            "content": inlines.iter().map(inline_to_atom).collect::<Vec<_>>()
        }),
    }
}

/// 单个块 → NoteAtom Value（main 里手工拼 content 数组时用）
pub fn block_value(b: &Block) -> Value {
    block_to_atom(b)
}

/// 把块列表组装成一篇笔记的 `body`
pub fn doc(blocks: &[Block]) -> Value {
    json!({
        "type": "doc",
        "content": blocks.iter().map(block_to_atom).collect::<Vec<_>>()
    })
}

/// 音频节点（file_id 来自本地上传投递结果；show_note 为时间轴字幕，可省略）
pub fn audio_block(file_id: &str, show_note: Option<&str>) -> Value {
    let mut attrs = json!({ "audio-uuid": file_id });
    if let Some(sn) = show_note {
        attrs["show-note"] = json!(sn);
    }
    json!({ "type": "audio", "attrs": attrs })
}
