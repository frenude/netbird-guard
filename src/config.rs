use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Custom label for this device (defaults to hostname / NetBird FQDN)
    pub device_name: String,
    /// Path to `netbird` binary (auto-detected if empty)
    pub netbird_bin: String,
    /// Optional management URL passed to `netbird up` when disconnected
    pub management_url: String,
    /// How often to poll `netbird status --json` (seconds)
    pub check_interval_secs: u64,
    /// Trigger proactive SSO refresh (`netbird login --extend`) when remaining session
    /// time is <= this value (seconds). Set to 0 to only trigger after expiry/disconnect.
    pub renew_before_secs: i64,
    /// Minimum interval between repeated notifications when login has not been completed (seconds)
    pub notify_cooldown_secs: u64,
    /// How long to keep `netbird login/up` waiting for the user to complete SSO in browser (seconds)
    pub login_wait_timeout_secs: u64,
    /// Automatically open SSO URL in local browser (default: true on macOS, false on Linux)
    pub auto_open_browser: bool,
    /// Show native macOS desktop notification (ignored on Linux)
    pub macos_notification: bool,
    /// Send a notification when NetBird recovers / finishes login
    pub notify_on_recovery: bool,
    /// Company Vercel Feishu Bot Relay (recommended for company-wide DM notifications)
    pub relay: RelayConfig,
    /// Direct Feishu custom bot webhook (optional personal mode)
    pub feishu: FeishuConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RelayConfig {
    /// Vercel relay endpoint, e.g. "https://nb.flow.chat/api/notify"
    pub url: String,
    /// Company email to receive Feishu Bot direct messages
    pub email: String,
    /// Shared relay token
    pub token: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FeishuConfig {
    /// Feishu custom bot webhook URL
    pub webhook_url: String,
    /// Optional signing secret configured in Feishu bot security settings
    pub secret: String,
    /// Optional: Direct Feishu Enterprise App DM mode (no relay server needed for personal use)
    pub app_id: String,
    pub app_secret: String,
    pub receive_open_id: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device_name: String::new(),
            netbird_bin: String::new(),
            management_url: String::new(),
            check_interval_secs: 60,
            renew_before_secs: 0,
            notify_cooldown_secs: 1800,
            login_wait_timeout_secs: 300,
            auto_open_browser: cfg!(target_os = "macos"),
            macos_notification: cfg!(target_os = "macos"),
            notify_on_recovery: false,
            relay: RelayConfig::default(),
            feishu: FeishuConfig::default(),
        }
    }
}

impl Config {
    pub fn default_path() -> PathBuf {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home)
                .join(".config")
                .join("nbg")
                .join("config.toml");
        }
        PathBuf::from("/etc/nbg/config.toml")
    }

    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read config file: {}", path.display()))?;
        let cfg: Self = toml::from_str(&raw)
            .with_context(|| format!("failed to parse TOML config: {}", path.display()))?;
        Ok(cfg)
    }

    pub fn write_template(
        path: &Path,
        force: bool,
        email: Option<&str>,
        relay_url: Option<&str>,
        relay_token: Option<&str>,
        device_name: Option<&str>,
    ) -> Result<()> {
        if path.exists() && !force {
            anyhow::bail!(
                "config already exists at {} (use --force to overwrite)",
                path.display()
            );
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let email = esc(email.unwrap_or(""));
        let relay_url = esc(relay_url.unwrap_or(""));
        let relay_token = esc(relay_token.unwrap_or(""));
        let device_name = esc(device_name.unwrap_or(""));

        let template = format!(
            r#"# netbird-guard 配置文件（macOS / Linux 通用）

# 设备名称（留空则自动使用本机 hostname）
device_name = "{device_name}"

# netbird 可执行文件路径（留空则自动查找 /opt/homebrew/bin/netbird, /usr/bin/netbird 等）
netbird_bin = ""

# 可选：公司自建 NetBird Management 地址（留空使用本机已保存配置）
management_url = ""

# 状态检查间隔（秒）
check_interval_secs = 60

# 提前多久触发续期（秒）。默认 0 = 只有真正到期/断开时才通知，不提前打扰
renew_before_secs = 0

# 同一异常状态下的通知冷却时间（秒），防止重复弹窗/刷屏
notify_cooldown_secs = 1800

# 发起登录后等待你在浏览器完成 SSO 的超时时间（秒）
login_wait_timeout_secs = 300

# 检测到需要登录时是否自动弹出本机浏览器（macOS 默认 true，Linux 服务器默认 false）
auto_open_browser = {auto_open}

# macOS 桌面系统通知（Linux 自动忽略）
macos_notification = {macos_notify}

# 恢复连接后是否再发一条飞书通知（默认 false，只在过期需要登录时发）
notify_on_recovery = false

# 公司统一飞书机器人中转（推荐：直接私聊发给你本人）
[relay]
url = "{relay_url}"
email = "{email}"
token = "{relay_token}"

# 可选：个人自定义群机器人 Webhook（不需要可留空）
[feishu]
webhook_url = ""
secret = ""
"#,
            auto_open = cfg!(target_os = "macos"),
            macos_notify = cfg!(target_os = "macos"),
        );
        fs::write(path, template)?;
        Ok(())
    }

    pub fn resolved_device_name(&self, fqdn: Option<&str>) -> String {
        if !self.device_name.trim().is_empty() {
            return self.device_name.trim().to_string();
        }
        if let Ok(out) = std::process::Command::new("hostname").output() {
            if out.status.success() {
                let h = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !h.is_empty() {
                    return h;
                }
            }
        }
        fqdn.unwrap_or("unknown-host").to_string()
    }

    pub fn resolved_netbird_bin(&self) -> String {
        if !self.netbird_bin.trim().is_empty() {
            return self.netbird_bin.trim().to_string();
        }
        let candidates = [
            "/opt/homebrew/bin/netbird",
            "/usr/local/bin/netbird",
            "/usr/bin/netbird",
            "/sbin/netbird",
        ];
        for c in candidates {
            if Path::new(c).exists() {
                return c.to_string();
            }
        }
        "netbird".to_string()
    }
}
