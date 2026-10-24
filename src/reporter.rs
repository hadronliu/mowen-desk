//! 晨报生成：open-meteo 天气（免 key）+ hitokoto 一言（免 key）+ 可选 AI 小语。

use chrono::{Datelike, Local, Weekday};

use crate::noteatom::{Block, Inline};
use crate::render::RLine;

pub struct MorningReport {
    pub title: String,
    pub blocks: Vec<Block>,
    pub lines: Vec<RLine>, // 屏幕阅读版
}

pub struct MorningCtx<'a> {
    pub latitude: f64,
    pub longitude: f64,
    pub ai_digest: bool,
    pub ai: Option<&'a crate::transcribe::Ai>,
}

pub async fn build(ctx: &MorningCtx<'_>) -> MorningReport {
    let today = Local::now().date_naive();
    let weekday = match today.weekday() {
        Weekday::Mon => "一", Weekday::Tue => "二", Weekday::Wed => "三",
        Weekday::Thu => "四", Weekday::Fri => "五", Weekday::Sat => "六", Weekday::Sun => "日",
    };
    let title = format!("{}月{}日 · 周{} · 晨报", today.month(), today.day(), weekday);

    let weather = fetch_weather(ctx.latitude, ctx.longitude)
        .await
        .unwrap_or_else(|| "天气：暂不可用".to_string());
    let hito = fetch_hitokoto().await.unwrap_or_else(|| "新的一天，慢慢来。".to_string());

    let mut ai_line = String::new();
    if ctx.ai_digest {
        if let Some(ai) = ctx.ai {
            if let Ok(s) = ai.morning_digest(&weather, &hito).await {
                if !s.is_empty() {
                    ai_line = s;
                }
            }
        }
    }

    // —— 墨问笔记版 ——
    let mut blocks = vec![
        Block::Title(1, title.clone()),
        Block::Para(vec![Inline::Text(weather.clone())]),
        Block::Spacer,
        Block::Quote(
            hito.lines().map(|l| Inline::Text(l.to_string())).collect(),
        ),
    ];
    if !ai_line.is_empty() {
        blocks.push(Block::Spacer);
        blocks.push(Block::Para(vec![Inline::Text(ai_line.clone())]));
    }

    // —— 墨水屏阅读版 ——
    let mut lines = vec![
        RLine::title(title.clone()),
        RLine::normal(weather.clone()),
    ];    if !ai_line.is_empty() {
        lines.push(RLine::normal(ai_line.clone()));
    }
    lines.push(crate::render::RLine { kind: crate::render::LineKind::Quote, spans: hito.lines().map(|l| crate::render::Span::t(l.to_string())).collect() });
    lines.push(RLine::dim("—— 按上下键翻页，长按确认键说话速记 ——"));

    MorningReport { title, blocks, lines }
}

/// open-meteo 免 key 天气
async fn fetch_weather(lat: f64, lon: f64) -> Option<String> {
    let url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={}&longitude={}\
         &daily=temperature_2m_max,temperature_2m_min,weather_code&current=wind_speed_10m&timezone=auto&forecast_days=1",
        lat, lon
    );
    let val: serde_json::Value = reqwest::get(&url).await.ok()?.json().await.ok()?;
    let max = val["daily"]["temperature_2m_max"][0].as_f64()?;
    let min = val["daily"]["temperature_2m_min"][0].as_f64()?;
    let code = val["daily"]["weather_code"][0].as_i64()?;
    let wind = val["current"]["wind_speed_10m"].as_f64().unwrap_or(0.0);
    Some(format!("今日 {} ~ {:.0}°C · {} · 风 {:.0} km/h", min, max, wmo_desc(code), wind))
}

fn wmo_desc(code: i64) -> &'static str {
    match code {
        0 => "晴", 1 => "大致晴", 2 => "多云", 3 => "阴",
        45 | 48 => "雾",
        51..=57 => "毛毛雨",
        61..=67 => "雨",
        71..=77 => "雪",
        80..=82 => "阵雨",
        85 | 86 => "阵雪",
        95..=99 => "雷雨",
        _ => "天气",
    }
}

/// hitokoto 一言
async fn fetch_hitokoto() -> Option<String> {
    let val: serde_json::Value = reqwest::Client::new()
        .get("https://v1.hitokoto.cn/?c=i&c=d&c=k")
        .timeout(std::time::Duration::from_secs(6))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let text = val["hitokoto"].as_str()?;
    let from = val["from"].as_str().unwrap_or("");
    if from.is_empty() {
        Some(text.to_string())
    } else {
        Some(format!("{text}\n—— {from}"))
    }
}
