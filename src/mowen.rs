//! 墨问 OpenAPI 客户端。
//! 文档：https://mowen.apifox.cn （创建/编辑/设置/上传）
//!
//! 注意：墨问 API 是纯写入通道，没有读取接口；本地留档见 archive.rs。

use anyhow::{anyhow, bail, Context, Result};
use reqwest::Client;
use serde_json::{json, Value};

pub struct MowenClient {
    http: Client,
    base: String,
    key: String,
}

#[derive(Debug)]
pub struct MowenError {
    pub status: u16,
    pub reason: String,
    pub message: String,
}

impl std::fmt::Display for MowenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "墨问 API 错误 {}: {} ({})", self.status, self.reason, self.message)
    }
}
impl std::error::Error for MowenError {}

/// 上传成功的文件信息（本地上传 / URL 上传共用）
#[derive(Debug, Clone)]
#[allow(dead_code)] // url/name 供调试与后续图片场景使用
pub struct UploadedFile {
    pub file_id: String,
    pub url: String,
    pub name: String,
}

/// 动态条目（关注作者流 / 发现动态流 共用；created_at 统一毫秒）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FeedItem {
    pub note_id: String,
    /// 一行展示文案（「作者 · 标题」或「♥ xx 赞了「yy」」等）
    pub display: String,
    pub author_name: String,
    pub created_at: i64,
}

impl MowenClient {
    pub fn new(base: &str, api_key: &str) -> Self {
        Self {
            http: Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .expect("reqwest client"),
            base: base.trim_end_matches('/').to_string(),
            key: api_key.to_string(),
        }
    }

    async fn post_json(&self, path: &str, body: Value) -> Result<Value> {
        let resp = self
            .http
            .post(format!("{}{}", self.base, path))
            .bearer_auth(&self.key)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("请求 {} 网络失败", path))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        let val: Value = serde_json::from_str(&text)
            .with_context(|| format!("{} 返回非 JSON：{}", path, truncate(&text, 200)))?;
        if !(200..300).contains(&status) {
            // 墨问错误结构：{code, reason, message, meta}
            let reason = val["reason"].as_str().unwrap_or("UNKNOWN").to_string();
            let msg = val["message"].as_str().unwrap_or("").to_string();
            return Err(MowenError { status, reason, message: msg }.into());
        }
        Ok(val)
    }

    /// 笔记创建。`body` 为 NoteAtom doc，返回 noteId。
    pub async fn create_note(
        &self,
        body: &Value,
        auto_publish: bool,
        tags: &[String],
    ) -> Result<String> {
        let mut settings = json!({});
        if auto_publish {
            settings["autoPublish"] = json!(true);
        }
        if !tags.is_empty() {
            settings["tags"] = json!(tags);
        }
        let val = self
            .post_json(
                "/api/open/api/v1/note/create",
                json!({ "body": body, "settings": settings }),
            )
            .await?;
        val["noteId"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("笔记创建成功但响应缺少 noteId：{val}"))
    }

    /// 拉取指定作者的公开笔记（scene=1 + targetUid + filter=6）。
    /// 未文档化接口：网关实测可用，响应含 users map（作者昵称）。
    pub async fn author_public_notes(&self, uid: &str, size: u32) -> Result<(String, Vec<FeedItem>)> {
        let val = self
            .post_json(
                "/api/open/api/v1/note/list",
                json!({
                    "scene": 1,
                    "pager": { "offset": 0, "size": size },
                    "params": { "targetUid": uid, "filter": 6 }
                }),
            )
            .await?;
        let author_name = val["users"][uid]["base"]["name"]
            .as_str()
            .unwrap_or("墨友")
            .to_string();
        let mut items = Vec::new();
        if let Some(ids) = val["noteIds"].as_array() {
            for id in ids {
                let Some(nid) = id.as_str() else { continue };
                let Some(n) = val["notes"].get(nid) else { continue };
                let base = &n["base"];
                let title = base["title"].as_str().unwrap_or("(无标题)");
                let ts_s: i64 = base["createdAt"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0);
                items.push(FeedItem {
                    note_id: nid.to_string(),
                    display: format!("{author_name} · {title}"),
                    author_name: author_name.clone(),
                    created_at: ts_s * 1000,
                });
            }
        }
        Ok((author_name, items))
    }

    /// 步骤一：获取本地上传授权。返回原始 form map（含上传端点与全部表单字段）。
    async fn upload_prepare(&self, file_type: i32, file_name: &str) -> Result<serde_json::Map<String, Value>> {        let val = self
            .post_json(
                "/api/open/api/v1/upload/prepare",
                json!({ "fileType": file_type, "fileName": file_name }),
            )
            .await?;
        val["form"]
            .as_object()
            .cloned()
            .ok_or_else(|| anyhow!("upload/prepare 响应缺少 form 字段：{val}"))
    }

    /// 本地文件上传（两步）：prepare 拿授权 → 向上传端点投递 multipart 表单。
    /// `file_type`: 1 图片 / 2 音频 / 3 PDF
    ///
    /// 实测（2026-10）：form 里含 `endpoint`（上传网关）与 `x:file_id`（预分配文件ID），
    /// 其余键值对原样作为表单字段提交，file 字段放最后。
    pub async fn upload_local(&self, file_type: i32, file_name: &str, bytes: Vec<u8>, mime: &str) -> Result<UploadedFile> {
        let form = self.upload_prepare(file_type, file_name).await?;

        let endpoint = form
            .get("endpoint")
            .and_then(|v| v.as_str())
            .filter(|s| s.starts_with("http"))
            .map(|s| s.to_string())
            .or_else(|| {
                // 兜底：任意 http 开头的值
                form.values()
                    .filter_map(|v| v.as_str())
                    .find(|s| s.starts_with("http"))
                    .map(|s| s.to_string())
            })
            .ok_or_else(|| anyhow!("upload/prepare 返回的 form 里找不到上传端点：{form:?}"))?;

        let preallocated_id = form
            .get("x:file_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let mut req = self.http.post(&endpoint);
        let mut form_builder = reqwest::multipart::Form::new();
        // 字段按 key 排序保证稳定，全部原样提交
        let mut fields: Vec<(String, String)> = form
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
            .filter(|(k, _)| k != "endpoint")
            .collect();
        fields.sort_by(|a, b| a.0.cmp(&b.0));
        for (k, v) in fields {
            form_builder = form_builder.text(k, v);
        }
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(file_name.to_string())
            .mime_str(mime)
            .with_context(|| format!("非法 mime: {mime}"))?;
        form_builder = form_builder.part("file", part);
        req = req.multipart(form_builder);

        let resp = req.send().await.with_context(|| format!("向 {endpoint} 投递文件失败"))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        eprintln!("[mowen] 投递响应 {} : {}", status, truncate(&text, 500));
        if !(200..300).contains(&status) {
            bail!("文件投递失败 HTTP {} ：{}", status, truncate(&text, 200));
        }
        let file_id = if !preallocated_id.is_empty() {
            preallocated_id
        } else {
            let val: Value = serde_json::from_str(&text)
                .with_context(|| format!("投递响应非 JSON：{}", truncate(&text, 200)))?;
            find_first_key(&val, &["fileId", "file_id", "id", "uuid", "fileUID", "file_uid"])
                .ok_or_else(|| anyhow!("投递响应里找不到文件 ID：{}", truncate(&text, 300)))?
        };
        let url = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| find_first_key(&v, &["url", "path"]))
            .unwrap_or_default();
        Ok(UploadedFile { file_id, url, name: file_name.to_string() })
    }
}

/// 深度优先查找多个候选键名中第一个字符串值
fn find_first_key(v: &Value, keys: &[&str]) -> Option<String> {
    match v {
        Value::Object(map) => {
            for k in keys {
                if let Some(s) = map.get(*k).and_then(|x| x.as_str()) {
                    return Some(s.to_string());
                }
            }
            for (_, child) in map {
                if let Some(s) = find_first_key(child, keys) {
                    return Some(s);
                }
            }
            None
        }
        Value::Array(arr) => arr.iter().find_map(|c| find_first_key(c, keys)),
        _ => None,
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}
