//! 配置加载与校验。

use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub zectrix: ZectrixCfg,
    pub mowen: MowenCfg,
    pub ai: AiCfg,
    pub morning: MorningCfg,
    pub feed: FeedCfg,
    pub render: RenderCfg,
    pub app: AppCfg,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct FeedCfg {
    pub enabled: bool,
    /// 动态源：auto（默认，有 mocli 用 discover，否则 follows）/ discover / follows
    pub source: String,
    /// mocli 路径（留空自动探测：PATH → /Applications/墨问.app 内置）
    pub mocli_path: String,
    /// discover 源的时间范围
    pub recent: String,
    /// follows 源：关注作者 uid 列表
    pub follows: Vec<String>,
    /// follows 源：每位作者每次拉取条数
    pub per_follow: usize,
    /// 刷新间隔（分钟）。discover 源服务端限频严格，建议 ≥30
    pub refresh_minutes: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ZectrixCfg {
    pub target: String,
    pub client_name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct MowenCfg {
    pub api_key: String,
    pub base_url: String,
    pub auto_publish: bool,
    pub tags_quick: Vec<String>,
    pub tags_morning: Vec<String>,
    pub daily_quota: u32,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct AiCfg {
    pub base_url: String,
    pub api_key: String,
    pub chat_model: String,
    pub asr_model: String,
}

impl AiCfg {
    pub fn usable(&self) -> bool {
        !self.base_url.is_empty() && !self.api_key.is_empty()
    }
    pub fn chat_usable(&self) -> bool {
        self.usable() && !self.chat_model.is_empty()
    }
    pub fn asr_usable(&self) -> bool {
        self.usable() && !self.asr_model.is_empty()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct MorningCfg {
    pub enabled: bool,
    pub time: String,
    pub latitude: f64,
    pub longitude: f64,
    pub ai_digest: bool,
}

impl Default for FeedCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            source: "auto".into(),
            mocli_path: String::new(),
            recent: "24h".into(),
            follows: vec![],
            per_follow: 6,
            refresh_minutes: 60,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RenderCfg {
    pub font_path: String,
    pub font_size: f32,
    pub image_format: String,
}

impl RenderCfg {
    pub fn format(&self) -> zectrix_sdk::ImageFormat {
        match self.image_format.as_str() {
            "mono" => zectrix_sdk::ImageFormat::Mono,
            "gray4" => zectrix_sdk::ImageFormat::Gray4,
            "gray16" => zectrix_sdk::ImageFormat::Gray16,
            _ => zectrix_sdk::ImageFormat::Gray4,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AppCfg {
    pub data_dir: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            zectrix: ZectrixCfg::default(),
            mowen: MowenCfg::default(),
            ai: AiCfg::default(),
            morning: MorningCfg::default(),
            feed: FeedCfg::default(),
            render: RenderCfg::default(),
            app: AppCfg::default(),
        }
    }
}
impl Default for ZectrixCfg {
    fn default() -> Self {
        Self { target: String::new(), client_name: "墨问桌垫".into() }
    }
}
impl Default for MowenCfg {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            base_url: "https://open.mowen.cn".into(),
            auto_publish: false,
            tags_quick: vec!["语音速记".into()],
            tags_morning: vec!["晨报".into()],
            daily_quota: 10,
        }
    }
}
impl Default for MorningCfg {
    fn default() -> Self {
        Self { enabled: true, time: "07:00".into(), latitude: 39.90, longitude: 116.40, ai_digest: true }
    }
}
impl Default for RenderCfg {
    fn default() -> Self {
        Self { font_path: String::new(), font_size: 20.0, image_format: "gray4".into() }
    }
}
impl Default for AppCfg {
    fn default() -> Self {
        Self { data_dir: "data".into() }
    }
}

impl Config {
    pub fn load() -> Result<Config> {
        let raw = std::fs::read_to_string("config.toml")
            .context("找不到 config.toml（可从 config.example.toml 复制一份再填写）")?;
        let cfg: Config = toml::from_str(&raw).context("config.toml 解析失败")?;
        if cfg.mowen.api_key.trim().is_empty() {
            bail!("config.toml 中 [mowen].api_key 为空：请到墨问小程序生成 API-KEY 后填入");
        }
        if !cfg.morning.time.contains(':') {
            bail!("[morning].time 格式应为 HH:MM");
        }
        Ok(cfg)
    }
}
