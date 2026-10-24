//! 可选 AI 能力：语音转写 + 标题整理 + 晨报小语。
//! 走 OpenAI 兼容接口（OpenAI / 智谱 / 任何兼容服务），未配置时全部优雅降级。

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::config::AiCfg;

pub struct Ai {
    cfg: AiCfg,
    http: reqwest::Client,
}

/// 速记整理结果
#[derive(Debug, Clone)]
pub struct QuickDigest {
    pub title: String,
    pub body_text: String,
}

impl Ai {
    pub fn new(cfg: AiCfg) -> Self {
        Self { cfg, http: reqwest::Client::builder().timeout(std::time::Duration::from_secs(120)).build().unwrap() }
    }

    pub fn asr_usable(&self) -> bool {
        self.cfg.asr_usable()
    }

    pub fn chat_usable(&self) -> bool {
        self.cfg.chat_usable()
    }

    /// WAV 语音转文字
    pub async fn transcribe(&self, wav: Vec<u8>, file_name: &str) -> Result<String> {
        let part = reqwest::multipart::Part::bytes(wav)
            .file_name(file_name.to_string())
            .mime_str("audio/wav")?;
        let form = reqwest::multipart::Form::new()
            .text("model", self.cfg.asr_model.clone())
            .part("file", part);
        let resp = self
            .http
            .post(format!("{}/audio/transcriptions", self.cfg.base_url.trim_end_matches('/')))
            .bearer_auth(&self.cfg.api_key)
            .multipart(form)
            .send()
            .await
            .context("转写请求失败")?;
        let status = resp.status();
        let val: Value = resp.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            anyhow::bail!("转写失败 {}: {}", status, val);
        }
        Ok(val["text"].as_str().unwrap_or("").trim().to_string())
    }

    /// 速记整理：给转写文本起标题（≤16 字）、顺一下正文
    pub async fn digest_quick_note(&self, transcript: &str) -> Result<QuickDigest> {
        let prompt = format!(
            "以下是一段语音速记的转写文字。请：1) 起一个不超过 14 个字的标题；2) 把正文轻度整理成通顺的段落（保留原意与口语细节，不要扩写，不添加小标题）。\n\
            只输出 JSON：{{\"title\":\"...\",\"body\":\"...\"}}\n\n转写文字：\n{transcript}"
        );
        let content = self.chat(&prompt).await?;
        let json_part = extract_json(&content);
        let v: Value = serde_json::from_str(&json_part).context("AI 返回的 JSON 无法解析")?;
        Ok(QuickDigest {
            title: v["title"].as_str().unwrap_or("语音速记").chars().take(16).collect(),
            body_text: v["body"].as_str().unwrap_or(transcript).to_string(),
        })
    }

    async fn chat(&self, prompt: &str) -> Result<String> {
        let resp = self
            .http
            .post(format!("{}/chat/completions", self.cfg.base_url.trim_end_matches('/')))
            .bearer_auth(&self.cfg.api_key)
            .json(&json!({
                "model": self.cfg.chat_model,
                "messages": [ { "role": "user", "content": prompt } ],
                "temperature": 0.7
            }))
            .send()
            .await
            .context("AI 请求失败")?;
        let status = resp.status();
        let val: Value = resp.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            anyhow::bail!("AI 调用失败 {}: {}", status, val);
        }
        Ok(val["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string())
    }
}

/// 从模型输出中抠出 JSON（容忍 ```json 包裹与前后废话）
fn extract_json(s: &str) -> String {
    let s = s.trim();
    if let Some(start) = s.find('{') {
        if let Some(end) = s.rfind('}') {
            if end > start {
                return s[start..=end].to_string();
            }
        }
    }
    s.to_string()
}
