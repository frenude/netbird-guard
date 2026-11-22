use crate::config::FeishuConfig;
use anyhow::{Context, Result};
use base64::Engine;
use chrono::Local;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

pub enum CardLevel {
    Warning,
    Error,
    Success,
}

pub struct FeishuCard<'a> {
    pub title: &'a str,
    pub level: CardLevel,
    pub device_name: &'a str,
    pub fqdn: &'a str,
    pub netbird_ip: &'a str,
    pub message: &'a str,
    pub login_url: Option<&'a str>,
}

fn sign(timestamp: i64, secret: &str) -> Result<String> {
    // Feishu custom bot spec: HMAC-SHA256 with key = "{timestamp}\n{secret}", empty message
    let key = format!("{timestamp}\n{secret}");
    let mut mac =
        HmacSha256::new_from_slice(key.as_bytes()).context("invalid HMAC key for Feishu sign")?;
    mac.update(b"");
    let digest = mac.finalize().into_bytes();
    Ok(base64::engine::general_purpose::STANDARD.encode(digest))
}

pub fn has_direct_app(cfg: &FeishuConfig) -> bool {
    !cfg.app_id.trim().is_empty()
        && !cfg.app_secret.trim().is_empty()
        && !cfg.receive_open_id.trim().is_empty()
}

fn build_card_json(card: &FeishuCard<'_>) -> Value {
    let template = match card.level {
        CardLevel::Warning => "orange",
        CardLevel::Error => "red",
        CardLevel::Success => "green",
    };

    let os_label = if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(target_os = "linux") {
        "Linux"
    } else {
        std::env::consts::OS
    };

    let now_str = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

    let mut md = format!(
        "**设备**：`{}` ({})\n**状态**：{}\n",
        card.device_name, os_label, card.message
    );
    if !card.fqdn.is_empty() {
        md.push_str(&format!("**FQDN**：`{}`\n", card.fqdn));
    }
    if !card.netbird_ip.is_empty() {
        md.push_str(&format!("**NetBird IP**：`{}`\n", card.netbird_ip));
    }
    md.push_str(&format!("**时间**：{}", now_str));

    let mut elements: Vec<Value> = vec![json!({
        "tag": "div",
        "text": {
            "tag": "lark_md",
            "content": md
        }
    })];

    if let Some(login_url) = card.login_url {
        elements.push(json!({ "tag": "hr" }));
        elements.push(json!({
            "tag": "action",
            "actions": [
                {
                    "tag": "button",
                    "text": {
                        "tag": "plain_text",
                        "content": "🔗 点击登录 NetBird"
                    },
                    "type": "primary",
                    "multi_url": {
                        "url": login_url,
                        "pc_url": login_url,
                        "android_url": login_url,
                        "ios_url": login_url
                    }
                }
            ]
        }));
        elements.push(json!({
            "tag": "note",
            "elements": [
                {
                    "tag": "plain_text",
                    "content": format!("如按钮无法打开，请复制链接：{}", login_url)
                }
            ]
        }));
    }

    json!({
        "config": { "wide_screen_mode": true },
        "header": {
            "title": {
                "tag": "plain_text",
                "content": card.title
            },
            "template": template
        },
        "elements": elements
    })
}

fn send_via_app_dm(cfg: &FeishuConfig, card_json: &Value) -> Result<()> {
    let auth_resp: Value = ureq::post(
        "https://open.feishu.cn/open-apis/auth/v3/tenant_access_token/internal",
    )
    .timeout(std::time::Duration::from_secs(15))
    .send_json(json!({
        "app_id": cfg.app_id.trim(),
        "app_secret": cfg.app_secret.trim(),
    }))
    .context("failed to request Feishu tenant_access_token")?
    .into_json()
    .context("invalid JSON from Feishu auth")?;

    let token = auth_resp
        .get("tenant_access_token")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Feishu auth error: {}", auth_resp))?;

    let send_resp: Value = ureq::post(
        "https://open.feishu.cn/open-apis/im/v1/messages?receive_id_type=open_id",
    )
    .timeout(std::time::Duration::from_secs(15))
    .set("Authorization", &format!("Bearer {token}"))
    .send_json(json!({
        "receive_id": cfg.receive_open_id.trim(),
        "msg_type": "interactive",
        "content": card_json.to_string(),
    }))
    .context("failed to send Feishu app DM")?
    .into_json()
    .unwrap_or_else(|_| json!({"code": 0}));

    let code = send_resp.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
    if code != 0 {
        anyhow::bail!("Feishu app DM error: {}", send_resp);
    }
    Ok(())
}

pub fn send_card(cfg: &FeishuConfig, card: &FeishuCard<'_>) -> Result<()> {
    let card_json = build_card_json(card);

    if has_direct_app(cfg) {
        send_via_app_dm(cfg, &card_json)?;
    }

    let url = cfg.webhook_url.trim();
    if url.is_empty() {
        return Ok(());
    }

    let mut payload = json!({
        "msg_type": "interactive",
        "card": card_json
    });

    if !cfg.secret.trim().is_empty() {
        let ts = chrono::Utc::now().timestamp();
        let sig = sign(ts, cfg.secret.trim())?;
        payload["timestamp"] = json!(ts.to_string());
        payload["sign"] = json!(sig);
    }

    let resp = ureq::post(url)
        .timeout(std::time::Duration::from_secs(15))
        .send_json(payload)
        .with_context(|| "failed to POST to Feishu webhook")?;

    let body: Value = resp
        .into_json()
        .unwrap_or_else(|_| json!({"code": 0}));
    let code = body
        .get("code")
        .or_else(|| body.get("StatusCode"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if code != 0 {
        anyhow::bail!("Feishu webhook returned error: {}", body);
    }
    Ok(())
}
