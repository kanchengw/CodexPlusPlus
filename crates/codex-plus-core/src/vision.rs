/// VLM (Vision Language Model) analysis for text-only models.
/// Batches images into groups, sends each batch as one API call.
use serde_json::Value;

pub struct VlmConfig {
    pub api_key: String,
    pub model: String,
    pub base_url: String,
}

const BATCH_SIZE: usize = 5;

pub fn should_process(model: &str, model_vlm_json: &str) -> bool {
    let Ok(map) = serde_json::from_str::<std::collections::BTreeMap<String, bool>>(model_vlm_json)
    else {
        return false;
    };
    map.get(model).copied().unwrap_or(false)
}

/// Collect all image URLs, remove image blocks.
pub fn collect_and_strip(messages: &mut [Value]) -> Vec<String> {
    let mut urls = Vec::new();
    for msg in messages.iter_mut() {
        let Some(content) = msg.get_mut("content") else { continue };
        let Some(parts) = content.as_array_mut() else { continue };
        let mut i = 0;
        while i < parts.len() {
            let kind = parts[i].get("type").and_then(Value::as_str).unwrap_or("");
            if kind == "image_url" || kind == "input_image" {
                if let Some(url) = parts[i]
                    .pointer("/image_url/url")
                    .or_else(|| parts[i].pointer("/image_url"))
                    .and_then(Value::as_str)
                    .filter(|u| !u.is_empty())
                {
                    urls.push(url.to_string());
                }
                parts.remove(i);
            } else { i += 1; }
        }
    }
    urls
}

/// Call VLM API with a batch of images. Reuses http_client for proxy/timeout.
async fn call_vlm_batch(urls: &[String], config: &VlmConfig) -> Result<String, String> {
    let client = crate::http_client::proxied_client("").map_err(|e| format!("client: {}", e))?;
    let url = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));
    let mut parts: Vec<Value> = urls
        .iter()
        .map(|u| serde_json::json!({"type": "image_url", "image_url": {"url": u}}))
        .collect();
    parts.push(serde_json::json!({"type": "text", "text": "Describe all images in detail."}));
    let body = serde_json::json!({
        "model": config.model,
        "messages": [{"role": "user", "content": parts}],
        "stream": false, "max_tokens": 2048,
    });
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", config.api_key))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("request failed: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!(
            "VLM API {}: {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        ));
    }
    let data: Value = resp
        .json()
        .await
        .map_err(|e| format!("parse failed: {}", e))?;
    data["choices"][0]["message"]["content"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| "no content".to_string())
}

/// Analyze all images in batches of BATCH_SIZE.
pub async fn analyze_all(urls: &[String], config: &VlmConfig) -> Result<String, String> {
    let mut results: Vec<String> = Vec::new();
    let mut failures: usize = 0;
    for batch in urls.chunks(BATCH_SIZE) {
        match call_vlm_batch(batch, config).await {
            Ok(text) => results.push(text),
            Err(e) => {
                failures += 1;
                results.push(format!(
                    "[Batch of {} images: VLM failed - {}]",
                    batch.len(),
                    e
                ));
            }
        }
    }
    if results.is_empty() {
        return Err("all VLM calls failed".to_string());
    }
    if failures > 0 {
        results.push(format!("其中 {} 批图片 Router VLM 调用失败。请在回复中包含 \"Router VLM 调用失败，未能识别图片内容\"", failures));
    }
    Ok(results.join("\n\n---\n\n"))
}

/// Inject analysis result into the last user message.
pub fn inject_analysis(messages: &mut [Value], result: &Result<String, String>) {
    let text = match result {
        Ok(c) => c.clone(),
        Err(_) => "用户发送了图片，但是 Router VLM 调用失败。请在回复中包含 \"Router VLM 调用失败，未能识别图片内容\"".to_string(),
    };
    for msg in messages.iter_mut().rev() {
        if msg.get("role").and_then(Value::as_str) == Some("user") {
            if let Some(Value::Array(parts)) = msg.get_mut("content") {
                parts.push(serde_json::json!({"type": "text", "text": text}));
            }
            break;
        }
    }
}

/// Strip images from the last user message and replace them with VLM analysis.
///
/// Historical images in older messages are stripped silently (not re-analyzed).
/// Only images from the latest user message are sent to VLM.
///
/// 1. Collect image URLs from the latest user message only
/// 2. Strip all images from all messages
/// 3. Call VLM API to analyze those images
/// 4. Inject the text description into the last user message
pub async fn strip_image_blocks(messages: &mut [Value], vlm_config: &VlmConfig) {
    // 1. Collect ALL image URLs and strip ALL images from ALL messages
    let all_urls = collect_and_strip(messages);
    if all_urls.is_empty() {
        return;
    }
    // 2. Analyze all images (including historical ones) and inject descriptions.
    // Re-analyzing old images ensures VLM context survives Codex's compression.
    let result = analyze_all(&all_urls, vlm_config).await;
    inject_analysis(messages, &result);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_process_returns_true_when_model_in_vlm_json() {
        assert!(should_process("gpt-4", r#"{"gpt-4":true}"#));
    }

    #[test]
    fn should_process_returns_false_when_model_not_in_vlm_json() {
        assert!(!should_process("claude-3", r#"{"gpt-4":true}"#));
    }

    #[test]
    fn should_process_returns_false_when_model_marked_false() {
        assert!(!should_process("gpt-4", r#"{"gpt-4":false}"#));
    }

    #[test]
    fn should_process_returns_false_for_empty_json() {
        assert!(!should_process("gpt-4", "{}"));
    }

    #[test]
    fn should_process_returns_false_for_invalid_json() {
        assert!(!should_process("gpt-4", "not-json"));
    }

    #[test]
    fn should_process_returns_false_for_empty_string() {
        assert!(!should_process("gpt-4", ""));
    }

    #[test]
    fn collect_and_strip_removes_image_url_blocks() {
        let mut messages = vec![serde_json::json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "hello"},
                {"type": "image_url", "image_url": {"url": "https://example.com/img.png"}},
            ]
        })];
        let urls = collect_and_strip(&mut messages);
        assert_eq!(urls, vec!["https://example.com/img.png"]);
        let parts = messages[0]["content"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["type"], "text");
    }

    #[test]
    fn collect_and_strip_handles_input_image_blocks() {
        let mut messages = vec![serde_json::json!({
            "role": "user",
            "content": [
                {"type": "input_image", "image_url": {"url": "data:image/png;base64,abc"}},
                {"type": "text", "text": "desc"},
            ]
        })];
        let urls = collect_and_strip(&mut messages);
        assert_eq!(urls, vec!["data:image/png;base64,abc"]);
    }

    #[test]
    fn collect_and_strip_returns_empty_when_no_images() {
        let mut messages = vec![serde_json::json!({
            "role": "user",
            "content": [{"type": "text", "text": "hello"}]
        })];
        let urls = collect_and_strip(&mut messages);
        assert!(urls.is_empty());
        assert_eq!(messages[0]["content"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn inject_analysis_adds_text_to_last_user_message() {
        let mut messages = vec![
            serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "ok"}]}),
            serde_json::json!({"role": "user", "content": [{"type": "text", "text": "hi"}]}),
        ];
        inject_analysis(&mut messages, &Ok("image description".to_string()));
        let parts = messages[1]["content"].as_array().unwrap();
        assert_eq!(parts.last().unwrap()["type"], "text");
        assert_eq!(parts.last().unwrap()["text"], "image description");
    }

    #[test]
    fn inject_analysis_adds_placeholder_on_error() {
        let mut messages = vec![serde_json::json!({
            "role": "user",
            "content": [{"type": "text", "text": "hi"}]
        })];
        inject_analysis(&mut messages, &Err("failed".to_string()));
        let parts = messages[0]["content"].as_array().unwrap();
        let last = parts.last().unwrap();
        assert_eq!(last["type"], "text");
        assert!(last["text"].as_str().unwrap().contains("Router VLM"));
    }
}
