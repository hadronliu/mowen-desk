//! 400×300 墨水屏渲染：中文字体探测、折行、分页、PNG 输出。
//!
//! 极趣 Note 4 只接收 PNG（SDK 自动画布合成与抖色），所有排版在这里完成。

use anyhow::{Context, Result};
use ab_glyph::{Font as _, FontArc, FontVec, Glyph, PxScale, Point};
use image::{GrayImage, Luma};

pub const W: u32 = 400;
pub const H: u32 = 300;
const MARGIN: f32 = 14.0;
const STATUS_H: f32 = 30.0;
const FOOTER_H: f32 = 18.0;

/// 行类别（决定字号/字重/行高）
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LineKind {
    Title,   // 标题：1.25x 加粗
    Normal,  // 正文
    Quote,   // 引用：左侧竖线
    Dim,     // 弱化小字（提示、时间戳）
    Item,    // 列表项（光标页用）
}

#[derive(Debug, Clone)]
pub struct Span {
    pub text: String,
    pub bold: bool,
}

impl Span {
    pub fn t(s: impl Into<String>) -> Span {
        Span { text: s.into(), bold: false }
    }
    pub fn b(s: impl Into<String>) -> Span {
        Span { text: s.into(), bold: true }
    }
}

#[derive(Debug, Clone)]
pub struct RLine {
    pub kind: LineKind,
    pub spans: Vec<Span>,
}

impl RLine {
    pub fn normal(s: impl Into<String>) -> RLine {
        RLine { kind: LineKind::Normal, spans: vec![Span::t(s)] }
    }
    pub fn title(s: impl Into<String>) -> RLine {
        RLine { kind: LineKind::Title, spans: vec![Span::t(s)] }
    }
    pub fn dim(s: impl Into<String>) -> RLine {
        RLine { kind: LineKind::Dim, spans: vec![Span::t(s)] }
    }
    pub fn item(s: impl Into<String>, selected: bool) -> RLine {
        RLine { kind: LineKind::Item, spans: vec![if selected { Span::b(s) } else { Span::t(s) }] }
    }
    pub fn plain_text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
}

pub struct Renderer {
    font: FontArc,
    pub base_size: f32,
}

impl Renderer {
    /// 按 [render].font_path → 系统字体顺序探测加载
    pub fn load(font_path: &str, base_size: f32) -> Result<Renderer> {
        let mut candidates: Vec<String> = vec![font_path.to_string()]
            .into_iter()
            .filter(|p| !p.is_empty())
            .collect();
        candidates.extend(
            [
                "/System/Library/Fonts/Hiragino Sans GB.ttc", // macOS
                "/System/Library/Fonts/PingFang.ttc",
                "/System/Library/Fonts/STHeiti Light.ttc",
                "/Library/Fonts/Arial Unicode.ttf",
                "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", // Linux
                "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
                "C:\\Windows\\Fonts\\msyh.ttc", // Windows 微软雅黑
            ]
            .iter()
            .map(|s| s.to_string()),
        );
        let mut last_err = String::from("无候选字体");
        for path in &candidates {
            match std::fs::read(path) {
                Ok(data) => {
                    let loaded = FontVec::try_from_vec_and_index(data.clone(), 0)
                        .or_else(|_| FontVec::try_from_vec(data))
                        .with_context(|| format!("字体 {path} 解析失败"));
                    match loaded {
                        Ok(f) => {
                            let font: FontArc = f.into();
                            eprintln!("[render] 使用字体 {path}");
                            return Ok(Renderer { font, base_size });
                        }
                        Err(e) => last_err = format!("{path}: {e}"),
                    }
                }
                Err(_) => continue,
            }
        }
        anyhow::bail!("找不到可用中文字体（{last_err}）。请在 config.toml [render].font_path 指定一个 ttf/ttc");
    }

    fn size_of(&self, kind: LineKind) -> f32 {
        match kind {
            LineKind::Title => self.base_size * 1.25,
            LineKind::Normal | LineKind::Item => self.base_size,
            LineKind::Quote => self.base_size * 0.95,
            LineKind::Dim => self.base_size * 0.7,
        }
    }
    fn line_height(&self, kind: LineKind) -> f32 {
        match kind {
            LineKind::Title => self.size_of(kind) * 1.7,
            _ => self.size_of(kind) * 1.6,
        }
    }
    fn bold(kind: LineKind) -> bool {
        matches!(kind, LineKind::Title)
    }

    fn advance_px(&self, ch: char, size: f32) -> f32 {
        let id = self.font.glyph_id(ch);
        // 缺字形（.notdef）按半字宽占位，避免未知字符画出占位符/豆腐块
        if id == ab_glyph::GlyphId(0) {
            return size * 0.5;
        }
        let upem = self.font.units_per_em().unwrap_or(1000.0);
        if upem <= 0.0 {
            return size;
        }
        self.font.h_advance_unscaled(id) / upem * size
    }

    /// 折行：字符级，CJK 收尾标点悬挂（避免行首出现，。等）
    fn wrap(&self, line: &RLine, max_w: f32) -> Vec<RLine> {
        let size = self.size_of(line.kind);
        let mut out: Vec<RLine> = Vec::new();
        let mut cur: Vec<Span> = Vec::new();
        let mut cur_w = 0.0f32;
        const NO_LINE_START: &str = "，。！？、；：）】》”’…—%,.;:!?)]}";

        let flush = |out: &mut Vec<RLine>, cur: &mut Vec<Span>| {
            if !cur.is_empty() {
                out.push(RLine { kind: line.kind, spans: std::mem::take(cur) });
            }
        };

        for span in &line.spans {
            let mut piece = String::new();
            let mut piece_w = 0.0;
            for ch in span.text.chars() {
                let adv = self.advance_px(ch, size);
                // 行首悬挂标点：跟随上一行（允许轻微超宽），避免"，。"出现在行首
                if NO_LINE_START.contains(ch) && piece.is_empty() && !cur.is_empty() {
                    cur.push(Span { text: ch.to_string(), bold: span.bold });
                    cur_w += adv;
                    continue;
                }
                if cur_w + piece_w + adv > max_w && !(piece.is_empty() && cur.is_empty()) {
                    // 英文单词不拆：把 piece 尾部连续的 ASCII 字母数字一起搬到下一行
                    let mut moved = String::new();
                    if ch.is_ascii_alphanumeric() {
                        while let Some(last) = piece.chars().last() {
                            if last.is_ascii_alphanumeric() {
                                moved.insert(0, last);
                                piece.pop();
                            } else {
                                break;
                            }
                        }
                    }
                    cur.push(Span { text: std::mem::take(&mut piece), bold: span.bold });
                    piece_w = 0.0;
                    flush(&mut out, &mut cur);
                    cur_w = 0.0;
                    if !moved.is_empty() {
                        let mw: f32 = moved.chars().map(|c| self.advance_px(c, size)).sum();
                        piece.push_str(&moved);
                        piece_w = mw;
                    }
                }
                piece.push(ch);
                piece_w += adv;
            }
            if !piece.is_empty() {
                cur_w += piece_w;
                cur.push(Span { text: piece, bold: span.bold });
            }
        }
        flush(&mut out, &mut cur);
        if out.is_empty() {
            out.push(RLine { kind: line.kind, spans: vec![] });
        }
        out
    }

    /// 折行 + 按内容高度分页
    pub fn paginate(&self, lines: &[RLine]) -> Vec<Vec<RLine>> {
        let max_w = W as f32 - MARGIN * 2.0 - 6.0; // 右侧留一点余量
        let content_h = H as f32 - STATUS_H - FOOTER_H - 6.0;
        let wrapped: Vec<RLine> = lines.iter().flat_map(|l| self.wrap(l, max_w)).collect();

        let mut pages: Vec<Vec<RLine>> = Vec::new();
        let mut page: Vec<RLine> = Vec::new();
        let mut used = 0.0f32;
        for line in wrapped {
            let h = self.line_height(line.kind);
            if used + h > content_h && !page.is_empty() {
                pages.push(std::mem::take(&mut page));
                used = 0.0;
            }
            used += h;
            page.push(line);
        }
        if !page.is_empty() || pages.is_empty() {
            pages.push(page);
        }
        pages
    }

    fn draw_text(&self, img: &mut GrayImage, x: f32, baseline_y: f32, text: &str, size: f32, bold: bool) {
        let scale = PxScale { x: size, y: size };
        let mut pen = Point { x, y: baseline_y };
        for ch in text.chars() {
            let id = self.font.glyph_id(ch);
            let adv = self.advance_px(ch, size);
            if ch != ' ' && id != ab_glyph::GlyphId(0) {
                let glyph = Glyph { id, scale, position: pen };
                if let Some(outlined) = self.font.outline_glyph(glyph) {
                    let bounds = outlined.px_bounds();
                    let passes: &[f32] = if bold { &[0.0, 0.55] } else { &[0.0] };
                    for dx in passes {
                        outlined.draw(|ox, oy, cov| {
                            if cov <= 0.0 {
                                return;
                            }
                            let px = (bounds.min.x + dx + ox as f32).round() as i64;
                            let py = (bounds.min.y + oy as f32).round() as i64;
                            if px < 0 || py < 0 || px >= W as i64 || py >= H as i64 {
                                return;
                            }
                            let v = (255.0 * (1.0 - cov.min(1.0))) as u8;
                            let p = img.get_pixel_mut(px as u32, py as u32);
                            if v < p[0] {
                                *p = Luma([v]);
                            }
                        });
                    }
                }
            }
            pen.x += adv;
        }
    }

    /// 渲染一页。`bar_left` / `bar_right` 为状态栏左右文案。
    pub fn draw_page(&self, lines: &[RLine], bar_left: &str, bar_right: &str, page_no: usize, page_total: usize) -> Vec<u8> {
        let mut img = GrayImage::from_pixel(W, H, Luma([255]));

        // 状态栏
        let bar_size = self.base_size * 0.68;
        let bar_mid = STATUS_H / 2.0 + bar_size * 0.36;
        self.draw_text(&mut img, MARGIN, bar_mid, bar_left, bar_size, true);
        let right_w: f32 = bar_right.chars().map(|c| self.advance_px(c, bar_size)).sum();
        self.draw_text(&mut img, W as f32 - MARGIN - right_w, bar_mid, bar_right, bar_size, false);
        for x in 8..(W - 8) {
            img.put_pixel(x, (STATUS_H - 3.0) as u32, Luma([120]));
        }

        // 正文
        let mut y = STATUS_H + 8.0;
        for line in lines {
            let size = self.size_of(line.kind);
            let lh = self.line_height(line.kind);
            let baseline = y + lh * 0.62;
            match line.kind {
                LineKind::Quote => {
                    for yy in (y as u32 + 2)..((y + lh - 4.0) as u32) {
                        img.put_pixel(MARGIN as u32 - 6, yy, Luma([110]));
                    }
                    let mut x = MARGIN + 2.0;
                    for span in &line.spans {
                        self.draw_text(&mut img, x, baseline, &span.text, size, span.bold);
                        x += span.text.chars().map(|c| self.advance_px(c, size)).sum::<f32>();
                    }
                }
                LineKind::Item => {
                    // 选中项：自绘实心三角光标（字符光标如 › 在中文字体里常缺字形）
                    let is_sel = line.spans.first().map(|s| s.bold).unwrap_or(false);
                    if is_sel {
                        let cy = y + lh * 0.5;
                        let x0 = MARGIN;
                        let x1 = MARGIN + size * 0.55;
                        for (i, px) in ((x0 as u32)..=(x1 as u32)).enumerate() {
                            let half = ((x1 - x0) as u32 - i as u32) as f32 * 0.8;
                            for py in ((cy - half) as u32)..=((cy + half) as u32) {
                                if py < H {
                                    img.put_pixel(px, py, Luma([60]));
                                }
                            }
                        }
                    }
                    let mut x = MARGIN + size * 1.0;
                    for span in &line.spans {
                        self.draw_text(&mut img, x, baseline, &span.text, size, span.bold);
                        x += span.text.chars().map(|c| self.advance_px(c, size)).sum::<f32>();
                    }
                }
                _ => {
                    let mut x = MARGIN;
                    for span in &line.spans {
                        self.draw_text(&mut img, x, baseline, &span.text, size, span.bold || Self::bold(line.kind));
                        x += span.text.chars().map(|c| self.advance_px(c, size)).sum::<f32>();
                    }
                }
            }
            y += lh;
        }

        // 页脚：进度条 + 页码
        let footer_base = H as f32 - 5.0;
        let foot_size = self.base_size * 0.62;
        let pager = format!("{}/{}", page_no + 1, page_total);
        let pager_w: f32 = pager.chars().map(|c| self.advance_px(c, foot_size)).sum();
        self.draw_text(&mut img, W as f32 - MARGIN - pager_w, footer_base, &pager, foot_size, false);
        if page_total > 1 {
            let bar_w = 80.0;
            let filled = bar_w * (page_no as f32 + 1.0) / page_total as f32;
            for x in 0..(bar_w as u32) {
                let v = if (x as f32) < filled { 90 } else { 225 };
                img.put_pixel(x + MARGIN as u32, (H - 8) as u32, Luma([v]));
            }
        }

        let mut out = Vec::new();
        image::DynamicImage::ImageLuma8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .expect("PNG encode");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn renderer() -> Renderer {
        Renderer::load("", 20.0).expect("系统应至少有一个可用字体")
    }

    #[test]
    fn wrap_respects_width() {
        let r = renderer();
        let line = RLine::normal("墨问桌垫是一款把极趣墨水屏和墨问笔记连在一起的桌面小机器，可以语音速记也可以看晨报。");
        let pages = r.paginate(&[line]);
        assert!(!pages.is_empty());
        // 一行 400px 宽 20px 字装不下 40 个中文字，必须被折行
        let total: usize = pages.iter().map(|p| p.len()).sum();
        assert!(total >= 2, "长句应被折为多行");
    }

    #[test]
    fn paginate_produces_png() {
        let r = renderer();
        let lines = vec![RLine::title("晨报"), RLine::normal("今天天气不错。"), RLine::dim("上下键翻页")];
        let pages = r.paginate(&lines);
        let png = r.draw_page(&pages[0], "晨报", "07:00", 0, pages.len());
        assert_eq!(png[..4], [0x89, b'P', b'N', b'G']);
        std::fs::write(std::env::temp_dir().join("mowen-desk-test.png"), &png).ok();
    }

    /// 生成动态列表样张（/tmp/mowen-feed-preview.png），检查光标与缺字形
    #[test]
    fn preview_feed_list() {
        let r = renderer();
        let lines = vec![
            RLine::title("发现 · 动态"),
            RLine::item("新 李俊达「读《斐多》」 10-24", true),
            RLine::item("付 typical Alice「关系现场」 10-24", false),
            RLine::item("赞 池建强 赞了「恐惧时，如何保持理性」 10-23", false),
            RLine::item("评 王江右「厄尔尼诺」 理论容易，涉及… 10-23", false),
            RLine::item("关 海马 关注了你 10-22", false),
            RLine::item("刷新动态", false),
            RLine::dim("120 条 · 更新 14:12 · 确认回主页"),
        ];
        let png = r.draw_page(&lines, "发现", "14:12", 0, 1);
        std::fs::write(std::env::temp_dir().join("mowen-feed-preview.png"), &png).unwrap();
    }

    /// 生成整版晨报样张，供人工检查排版（/tmp/mowen-preview.png）
    /// 生成主页样张（docs/sample-home.png 用）
    #[test]
    fn preview_home_page() {
        let r = renderer();
        let lines = vec![
            RLine::title("墨问桌垫"),
            RLine::item("动态 · 120 条", true),
            RLine::item("10-24 15:02 速记 20261024-145023", false),
            RLine::item("10-24 14:43 速记 20261024-144351", false),
            RLine::normal("长按确认键 1 秒说话，"),
            RLine::normal("松开即自动发到墨问。"),
            RLine::dim("今日已发 4/10 · 上下移动 确认打开"),
        ];
        let png = r.draw_page(&lines, "墨问桌垫", "15:04", 0, 1);
        std::fs::write(std::env::temp_dir().join("mowen-home-preview.png"), &png).unwrap();
    }
}
