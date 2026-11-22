mod config;
mod feishu;
mod netbird;
mod notify;
mod relay;
mod service;

use anyhow::Result;
use chrono::{Local, Utc};
use clap::{Parser, Subcommand};
use config::Config;
use feishu::{CardLevel, FeishuCard};
use netbird::{HealthState, LoginMode};
use relay::RelayPayload;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(
    name = "nbg",
    version,
    about = "NetBird 会话过期自动守护 CLI（到期自动弹浏览器 + 飞书私聊登录链接）"
)]
struct Cli {
    /// 配置文件路径（默认 ~/.config/nbg/config.toml）
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 生成配置文件
    Init {
        #[arg(long)]
        force: bool,
        /// 公司邮箱（用于接收飞书机器人私聊通知）
        #[arg(long)]
        email: Option<String>,
        /// Relay 通知接口地址
        #[arg(long)]
        relay_url: Option<String>,
        /// Relay 访问令牌
        #[arg(long)]
        relay_token: Option<String>,
        /// 自定义设备名称
        #[arg(long)]
        device_name: Option<String>,
    },
    /// 查看当前 NetBird 连接状态和剩余过期时间
    Status,
    /// 执行一次检查（过期/断开时触发登录与通知）
    Check,
    /// 作为守护进程持续运行
    Run,
    /// 发送一条测试通知到飞书
    #[command(alias = "test-notify", alias = "test-feishu")]
    Test,
    /// 安装并启动系统后台服务（macOS LaunchAgent / Linux systemd）
    Install,
    /// 卸载系统后台服务
    Uninstall,
}

fn log(msg: &str) {
    let ts = Local::now().format("%Y-%m-%d %H:%M:%S");
    println!("[{ts}] {msg}");
}

fn os_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(target_os = "linux") {
        "Linux"
    } else {
        std::env::consts::OS
    }
}

fn format_duration(secs: i64) -> String {
    let s = secs.max(0);
    let d = s / 86400;
    let h = (s % 86400) / 3600;
    let m = (s % 3600) / 60;
    if d > 0 {
        format!("{d}天 {h}小时 {m}分钟")
    } else if h > 0 {
        format!("{h}小时 {m}分钟")
    } else {
        format!("{m}分钟")
    }
}

fn dispatch_notification(
    cfg: &Config,
    event: &str,
    title: &str,
    level: CardLevel,
    device: &str,
    fqdn: &str,
    netbird_ip: &str,
    message: &str,
    login_url: Option<&str>,
) {
    if !cfg.relay.url.trim().is_empty() {
        let payload = RelayPayload {
            event,
            device_name: device,
            os: os_label(),
            fqdn,
            netbird_ip,
            message,
            login_url,
        };
        match relay::send(&cfg.relay, &payload) {
            Ok(_) => log("已通过飞书机器人推送私聊通知"),
            Err(e) => log(&format!("Relay 推送失败: {e:#}")),
        }
    }

    if !cfg.feishu.webhook_url.trim().is_empty() || feishu::has_direct_app(&cfg.feishu) {
        let card = FeishuCard {
            title,
            level,
            device_name: device,
            fqdn,
            netbird_ip,
            message,
            login_url,
        };
        match feishu::send_card(&cfg.feishu, &card) {
            Ok(_) => log("已通过飞书推送通知卡片"),
            Err(e) => log(&format!("飞书推送失败: {e:#}")),
        }
    }
}

struct WatchState {
    last_notified_at: Option<Instant>,
    last_heartbeat_at: Option<Instant>,
    was_unhealthy: bool,
}

fn maybe_send_heartbeat(
    cfg: &Config,
    device: &str,
    status: &netbird::NetbirdStatus,
    state: &mut WatchState,
    force: bool,
) {
    let due = force
        || state
            .last_heartbeat_at
            .map(|t| t.elapsed() >= Duration::from_secs(300))
            .unwrap_or(true);
    if !due {
        return;
    }
    let exp_str = status.expires_at().map(|dt| dt.to_rfc3339());
    let hb = relay::HeartbeatPayload {
        device_name: device,
        os: os_label(),
        fqdn: &status.fqdn,
        netbird_ip: &status.netbird_ip,
        daemon_status: &status.daemon_status,
        management_connected: status.management.connected,
        expires_at: exp_str.as_deref(),
    };
    if relay::send_heartbeat(&cfg.relay, &hb).is_ok() {
        state.last_heartbeat_at = Some(Instant::now());
    }
}

fn run_once(cfg: &Config, state: &mut WatchState) {
    let bin = cfg.resolved_netbird_bin();
    let status = match netbird::query_status(&bin) {
        Ok(s) => s,
        Err(e) => {
            log(&format!("查询 NetBird 状态失败: {e:#}"));
            return;
        }
    };

    let device = cfg.resolved_device_name(Some(&status.fqdn));
    maybe_send_heartbeat(cfg, &device, &status, state, false);
    let health = status.evaluate(cfg.renew_before_secs);

    match health {
        HealthState::Healthy { expires_at } => {
            if state.was_unhealthy {
                log("NetBird 已恢复正常连接");
                if cfg.notify_on_recovery {
                    let msg = match expires_at {
                        Some(exp) => {
                            let rem = (exp - Utc::now()).num_seconds();
                            format!(
                                "已恢复连接，下次过期：{}（剩余 {}）",
                                exp.with_timezone(&Local).format("%Y-%m-%d %H:%M"),
                                format_duration(rem)
                            )
                        }
                        None => "已恢复连接".to_string(),
                    };
                    dispatch_notification(
                        cfg,
                        "recovered",
                        &format!("✅ NetBird 已恢复 - {device}"),
                        CardLevel::Success,
                        &device,
                        &status.fqdn,
                        &status.netbird_ip,
                        &msg,
                        None,
                    );
                }
                state.was_unhealthy = false;
                state.last_notified_at = None;
            }
        }
        HealthState::ExpiringSoon {
            expires_at,
            remaining_secs,
        } => {
            state.was_unhealthy = true;
            if in_cooldown(state, cfg.notify_cooldown_secs) {
                return;
            }
            let reason = format!(
                "Session 即将过期（剩余 {}，过期时间 {}）",
                format_duration(remaining_secs),
                expires_at.with_timezone(&Local).format("%Y-%m-%d %H:%M")
            );
            log(&format!("检测到需要续期: {reason}"));
            handle_login(cfg, &bin, &device, &status, LoginMode::Extend, &reason, true);
            state.last_notified_at = Some(Instant::now());
        }
        HealthState::NeedsLogin { reason } => {
            state.was_unhealthy = true;
            if in_cooldown(state, cfg.notify_cooldown_secs) {
                return;
            }
            log(&format!("检测到 NetBird 断开/过期: {reason}"));
            handle_login(cfg, &bin, &device, &status, LoginMode::Up, &reason, false);
            state.last_notified_at = Some(Instant::now());
        }
    }
}

fn in_cooldown(state: &WatchState, cooldown_secs: u64) -> bool {
    if let Some(last) = state.last_notified_at {
        if last.elapsed() < Duration::from_secs(cooldown_secs) {
            return true;
        }
    }
    false
}

fn handle_login(
    cfg: &Config,
    bin: &str,
    device: &str,
    status: &netbird::NetbirdStatus,
    mode: LoginMode,
    reason: &str,
    is_warning: bool,
) {
    let event = if is_warning {
        "expiring_soon"
    } else {
        "needs_login"
    };
    let title = if is_warning {
        format!("⚠️ NetBird 即将过期，请续登 - {device}")
    } else {
        format!("🚨 NetBird 已断开，请重新登录 - {device}")
    };

    let outcome = netbird::run_login_flow(
        bin,
        &cfg.management_url,
        mode,
        Duration::from_secs(cfg.login_wait_timeout_secs),
        |url| {
            log(&format!("捕获到 SSO 登录链接: {url}"));

            if cfg.auto_open_browser {
                log("正在自动打开本机浏览器...");
                notify::open_browser(url);
            }
            if cfg.macos_notification {
                notify::desktop_notify(&title, reason);
            }

            dispatch_notification(
                cfg,
                event,
                &title,
                if is_warning {
                    CardLevel::Warning
                } else {
                    CardLevel::Error
                },
                device,
                &status.fqdn,
                &status.netbird_ip,
                reason,
                Some(url),
            );
        },
    );

    match outcome {
        Ok(res) => {
            if res.url.is_none() {
                log(&format!(
                    "未从 netbird 输出中捕获到登录链接（succeeded={}）：{}",
                    res.succeeded, res.output_tail
                ));
                if !res.succeeded {
                    let msg = format!("{reason}\n（自动获取登录链接失败，请手动登录）");
                    dispatch_notification(
                        cfg,
                        event,
                        &title,
                        CardLevel::Error,
                        device,
                        &status.fqdn,
                        &status.netbird_ip,
                        &msg,
                        None,
                    );
                }
            } else if res.succeeded {
                log("SSO 登录已完成！");
            } else {
                log("等待 SSO 登录超时，将在冷却时间后重试");
            }
        }
        Err(e) => {
            log(&format!("执行 netbird 登录命令失败: {e:#}"));
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg_path = cli.config.unwrap_or_else(Config::default_path);

    match cli.command {
        Commands::Init {
            force,
            email,
            relay_url,
            relay_token,
            device_name,
        } => {
            Config::write_template(
                &cfg_path,
                force,
                email.as_deref(),
                relay_url.as_deref(),
                relay_token.as_deref(),
                device_name.as_deref(),
            )?;
            println!("✅ 已生成配置文件: {}", cfg_path.display());
        }
        Commands::Status => {
            let cfg = Config::load(&cfg_path).unwrap_or_default();
            let bin = cfg.resolved_netbird_bin();
            let st = netbird::query_status(&bin)?;
            let device = cfg.resolved_device_name(Some(&st.fqdn));
            println!("Device       : {device}");
            println!("DaemonStatus : {}", st.daemon_status);
            println!(
                "Management   : {} (connected={})",
                st.management.url, st.management.connected
            );
            println!("FQDN         : {}", st.fqdn);
            println!("NetBird IP   : {}", st.netbird_ip);
            match st.expires_at() {
                Some(exp) => {
                    let rem = (exp - Utc::now()).num_seconds();
                    println!(
                        "Expires At   : {} (剩余 {})",
                        exp.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S"),
                        format_duration(rem)
                    );
                }
                None => println!("Expires At   : 无/未登录"),
            }
            println!("Evaluation   : {:?}", st.evaluate(cfg.renew_before_secs));
        }
        Commands::Check => {
            let cfg = Config::load(&cfg_path).unwrap_or_default();
            let mut state = WatchState {
                last_notified_at: None,
                last_heartbeat_at: None,
                was_unhealthy: false,
            };
            run_once(&cfg, &mut state);
        }
        Commands::Run => {
            let cfg = Config::load(&cfg_path)?;
            let device = cfg.resolved_device_name(None);
            log(&format!(
                "netbird-guard 启动 | 设备={device} | 轮询={}s | 提前续期={}s | 自动弹浏览器={} | Relay={} | Webhook={}",
                cfg.check_interval_secs,
                cfg.renew_before_secs,
                cfg.auto_open_browser,
                !cfg.relay.url.trim().is_empty(),
                !cfg.feishu.webhook_url.trim().is_empty(),
            ));
            let mut state = WatchState {
                last_notified_at: None,
                last_heartbeat_at: None,
                was_unhealthy: false,
            };
            loop {
                run_once(&cfg, &mut state);
                thread::sleep(Duration::from_secs(cfg.check_interval_secs.max(10)));
            }
        }
        Commands::Test => {
            let cfg = Config::load(&cfg_path)?;
            let has_feishu =
                !cfg.feishu.webhook_url.trim().is_empty() || feishu::has_direct_app(&cfg.feishu);
            if cfg.relay.url.trim().is_empty() && !has_feishu {
                anyhow::bail!(
                    "请先在 {} 中配置 [feishu] (app_id+app_secret+receive_open_id 或 webhook_url) 或 [relay]",
                    cfg_path.display()
                );
            }
            let st = netbird::query_status(&cfg.resolved_netbird_bin()).ok();
            let fqdn = st.as_ref().map(|s| s.fqdn.as_str()).unwrap_or("");
            let ip = st.as_ref().map(|s| s.netbird_ip.as_str()).unwrap_or("");
            let device = cfg.resolved_device_name(Some(fqdn));
            let msg = match st.as_ref().and_then(|s| s.expires_at()) {
                Some(exp) => format!(
                    "直连飞书测试成功！当前 NetBird 剩余有效期：{}",
                    format_duration((exp - Utc::now()).num_seconds())
                ),
                None => "直连飞书测试成功！这是一条来自 nbg 的测试通知。".to_string(),
            };
            if !cfg.relay.url.trim().is_empty() {
                relay::send(
                    &cfg.relay,
                    &RelayPayload {
                        event: "test",
                        device_name: &device,
                        os: os_label(),
                        fqdn,
                        netbird_ip: ip,
                        message: &msg,
                        login_url: None,
                    },
                )?;
                println!("✅ 已通过飞书机器人发送私聊测试卡片！");
            }
            if has_feishu {
                let card = FeishuCard {
                    title: &format!("🧪 NBG 直连测试通知 · {device}"),
                    level: CardLevel::Success,
                    device_name: &device,
                    fqdn,
                    netbird_ip: ip,
                    message: &msg,
                    login_url: None,
                };
                feishu::send_card(&cfg.feishu, &card)?;
                println!("✅ 已直接通过飞书机器人发送私聊测试卡片！");
            }
        }
        Commands::Install => {
            if !cfg_path.exists() {
                Config::write_template(&cfg_path, false, None, None, None, None)?;
                println!("已自动创建默认配置: {}", cfg_path.display());
            }
            service::install_service(&cfg_path)?;
        }
        Commands::Uninstall => {
            service::uninstall_service()?;
        }
    }

    Ok(())
}
