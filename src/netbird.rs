use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use regex::Regex;
use serde::Deserialize;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetbirdStatus {
    #[serde(default)]
    pub daemon_status: String,
    #[serde(default)]
    pub management: ManagementStatus,
    #[serde(default)]
    pub netbird_ip: String,
    #[serde(default)]
    pub fqdn: String,
    #[serde(default)]
    pub session_expires_at: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagementStatus {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub connected: bool,
    #[serde(default)]
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthState {
    /// Connected and session has plenty of time left (or has no expiry)
    Healthy {
        expires_at: Option<DateTime<Utc>>,
    },
    /// Connected, but session expires within `renew_before_secs`
    ExpiringSoon {
        expires_at: DateTime<Utc>,
        remaining_secs: i64,
    },
    /// Disconnected / NeedsLogin / expired
    NeedsLogin {
        reason: String,
    },
}

impl NetbirdStatus {
    pub fn expires_at(&self) -> Option<DateTime<Utc>> {
        let raw = self.session_expires_at.as_deref()?.trim();
        if raw.is_empty() || raw.starts_with("0001-01-01") {
            return None;
        }
        DateTime::parse_from_rfc3339(raw)
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
    }

    pub fn evaluate(&self, renew_before_secs: i64) -> HealthState {
        let connected = self.daemon_status.eq_ignore_ascii_case("Connected")
            && self.management.connected;

        if !connected {
            let reason = if !self.management.error.trim().is_empty() {
                format!("{} ({})", self.daemon_status, self.management.error.trim())
            } else if !self.daemon_status.trim().is_empty() {
                format!("状态异常: {}", self.daemon_status)
            } else {
                "Management 未连接".to_string()
            };
            return HealthState::NeedsLogin { reason };
        }

        if let Some(exp) = self.expires_at() {
            let remaining = (exp - Utc::now()).num_seconds();
            if remaining <= 0 {
                return HealthState::NeedsLogin {
                    reason: format!("Session 已于 {} 过期", exp.format("%Y-%m-%d %H:%M:%S UTC")),
                };
            }
            if renew_before_secs > 0 && remaining <= renew_before_secs {
                return HealthState::ExpiringSoon {
                    expires_at: exp,
                    remaining_secs: remaining,
                };
            }
            return HealthState::Healthy {
                expires_at: Some(exp),
            };
        }

        HealthState::Healthy { expires_at: None }
    }
}

pub fn query_status(netbird_bin: &str) -> Result<NetbirdStatus> {
    let out = Command::new(netbird_bin)
        .args(["status", "--json"])
        .output()
        .with_context(|| format!("failed to execute `{netbird_bin} status --json`"))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        anyhow::bail!(
            "netbird status failed (exit {:?}): {}{}",
            out.status.code(),
            stderr.trim(),
            stdout.trim()
        );
    }

    let status: NetbirdStatus = serde_json::from_slice(&out.stdout)
        .with_context(|| "failed to parse JSON from `netbird status --json`")?;
    Ok(status)
}

pub enum LoginMode {
    /// Active tunnel, refresh SSO session in place without tearing down WireGuard
    Extend,
    /// Disconnected / expired, bring tunnel up and run SSO login
    Up,
}

pub struct LoginOutcome {
    pub url: Option<String>,
    pub succeeded: bool,
    pub output_tail: String,
}

/// Spawns `netbird login --extend --no-browser` or `netbird up --no-browser`,
/// streams its output to capture the SSO URL immediately, invokes `on_url` as soon as
/// the URL appears, and waits up to `timeout` for login completion.
pub fn run_login_flow<F>(
    netbird_bin: &str,
    management_url: &str,
    mode: LoginMode,
    timeout: Duration,
    mut on_url: F,
) -> Result<LoginOutcome>
where
    F: FnMut(&str),
{
    let mut cmd = Command::new(netbird_bin);
    match mode {
        LoginMode::Extend => {
            cmd.args(["login", "--extend", "--no-browser"]);
        }
        LoginMode::Up => {
            cmd.args(["up", "--no-browser"]);
            if !management_url.trim().is_empty() {
                cmd.args(["--management-url", management_url.trim()]);
            }
        }
    }

    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn `{netbird_bin}` login process"))?;

    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    let (tx, rx) = mpsc::channel::<String>();
    let tx_err = tx.clone();

    let h_out = thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    let h_err = thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = tx_err.send(line);
        }
    });

    let url_re = Regex::new(r#"https?://[^\s"'<>]+"#).unwrap();
    let deadline = Instant::now() + timeout;
    let mut captured_url: Option<String> = None;
    let mut recent_lines: Vec<String> = Vec::new();
    let mut succeeded = false;

    loop {
        while let Ok(line) = rx.try_recv() {
            let trimmed = line.trim().to_string();
            if !trimmed.is_empty() {
                if recent_lines.len() >= 20 {
                    recent_lines.remove(0);
                }
                recent_lines.push(trimmed.clone());
            }
            if captured_url.is_none() {
                if let Some(m) = url_re.find(&line) {
                    let url = m.as_str().trim_end_matches(&['.', ',', ')', ']'][..]).to_string();
                    // Ignore management-url echo if it's just the bare server root without path/query
                    if url.contains("http") && (url.len() > 28 || url.contains('?')) {
                        on_url(&url);
                        captured_url = Some(url);
                    }
                }
            }
        }

        if let Some(exit) = child.try_wait()? {
            succeeded = exit.success();
            // Drain any remaining lines briefly
            while let Ok(line) = rx.recv_timeout(Duration::from_millis(50)) {
                if !line.trim().is_empty() {
                    recent_lines.push(line.trim().to_string());
                }
                if captured_url.is_none() {
                    if let Some(m) = url_re.find(&line) {
                        let url = m.as_str().trim_end_matches(&['.', ',', ')', ']'][..]).to_string();
                        on_url(&url);
                        captured_url = Some(url);
                    }
                }
            }
            break;
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break;
        }

        thread::sleep(Duration::from_millis(150));
    }

    let _ = h_out.join();
    let _ = h_err.join();

    Ok(LoginOutcome {
        url: captured_url,
        succeeded,
        output_tail: recent_lines.join("\n"),
    })
}
