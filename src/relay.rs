use crate::config::RelayConfig;
use anyhow::{Context, Result};
use serde_json::{json, Value};

pub struct RelayPayload<'a> {
    pub event: &'a str, // "expiring_soon" | "needs_login" | "recovered" | "test"
    pub device_name: &'a str,
    pub os: &'a str,
    pub fqdn: &'a str,
    pub netbird_ip: &'a str,
    pub message: &'a str,
    pub login_url: Option<&'a str>,
}

pub fn send(cfg: &RelayConfig, payload: &RelayPayload<'_>) -> Result<()> {
    let url = cfg.url.trim();
    if url.is_empty() {
        return Ok(());
    }
    let email = cfg.email.trim();
    let token = cfg.token.trim();
    if email.is_empty() && token.is_empty() {
        anyhow::bail!("relay.url 已配置，但未设置 relay.token（请先访问官网使用飞书登录获取安装命令）");
    }

    let body = json!({
        "email": email,
        "event": payload.event,
        "device_name": payload.device_name,
        "os": payload.os,
        "fqdn": payload.fqdn,
        "netbird_ip": payload.netbird_ip,
        "message": payload.message,
        "login_url": payload.login_url,
    });

    let mut req = ureq::post(url).timeout(std::time::Duration::from_secs(15));
    if !cfg.token.trim().is_empty() {
        req = req.set("X-Guard-Token", cfg.token.trim());
    }

    let resp = match req.send_json(body) {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let text = r.into_string().unwrap_or_default();
            anyhow::bail!("relay returned HTTP {code}: {text}");
        }
        Err(e) => return Err(e).context("failed to POST to relay server"),
    };

    let val: Value = resp.into_json().unwrap_or_else(|_| json!({"ok": true}));
    if val.get("ok").and_then(|v| v.as_bool()) == Some(false) {
        anyhow::bail!("relay error: {}", val);
    }
    Ok(())
}

pub struct HeartbeatPayload<'a> {
    pub device_name: &'a str,
    pub os: &'a str,
    pub fqdn: &'a str,
    pub netbird_ip: &'a str,
    pub daemon_status: &'a str,
    pub management_connected: bool,
    pub expires_at: Option<&'a str>,
}

pub fn send_heartbeat(cfg: &RelayConfig, payload: &HeartbeatPayload<'_>) -> Result<()> {
    let base = cfg.url.trim();
    let token = cfg.token.trim();
    if base.is_empty() || token.is_empty() {
        return Ok(());
    }
    let hb_url = if let Some(prefix) = base.strip_suffix("/api/notify") {
        format!("{prefix}/api/heartbeat")
    } else {
        format!("{}/api/heartbeat", base.trim_end_matches('/'))
    };

    let body = json!({
        "device_name": payload.device_name,
        "os": payload.os,
        "fqdn": payload.fqdn,
        "netbird_ip": payload.netbird_ip,
        "daemon_status": payload.daemon_status,
        "management_connected": payload.management_connected,
        "expires_at": payload.expires_at,
    });

    let _ = ureq::post(&hb_url)
        .timeout(std::time::Duration::from_secs(10))
        .set("X-Guard-Token", token)
        .send_json(body)?;
    Ok(())
}
