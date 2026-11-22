use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn install_service(config_path: &Path) -> Result<()> {
    let bin_path = std::env::current_exe().context("failed to resolve current binary path")?;
    let config_abs = fs::canonicalize(config_path).unwrap_or_else(|_| config_path.to_path_buf());

    #[cfg(target_os = "macos")]
    {
        install_launchd(&bin_path, &config_abs)
    }
    #[cfg(target_os = "linux")]
    {
        install_systemd(&bin_path, &config_abs)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (bin_path, config_abs);
        anyhow::bail!("service installation is only supported on macOS and Linux")
    }
}

pub fn uninstall_service() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let plist = launchd_plist_path()?;
        let _ = Command::new("launchctl")
            .args(["unload", "-w"])
            .arg(&plist)
            .status();
        if plist.exists() {
            fs::remove_file(&plist)?;
        }
        println!("已卸载 macOS LaunchAgent: {}", plist.display());
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        let unit = PathBuf::from("/etc/systemd/system/netbird-guard.service");
        let _ = Command::new("systemctl")
            .args(["disable", "--now", "netbird-guard"])
            .status();
        if unit.exists() {
            fs::remove_file(&unit)?;
        }
        let _ = Command::new("systemctl").arg("daemon-reload").status();
        println!("已卸载 systemd 服务: {}", unit.display());
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        anyhow::bail!("service uninstallation is only supported on macOS and Linux")
    }
}

#[cfg(target_os = "macos")]
fn launchd_plist_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME env not set")?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("LaunchAgents")
        .join("com.frenude.netbird-guard.plist"))
}

#[cfg(target_os = "macos")]
fn install_launchd(bin_path: &Path, config_path: &Path) -> Result<()> {
    let home = std::env::var("HOME").context("HOME env not set")?;
    let log_dir = PathBuf::from(&home).join("Library").join("Logs");
    fs::create_dir_all(&log_dir)?;
    let log_file = log_dir.join("netbird-guard.log");

    let plist_path = launchd_plist_path()?;
    if let Some(p) = plist_path.parent() {
        fs::create_dir_all(p)?;
    }

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.frenude.netbird-guard</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bin}</string>
        <string>run</string>
        <string>--config</string>
        <string>{cfg}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
    </dict>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>
"#,
        bin = bin_path.display(),
        cfg = config_path.display(),
        log = log_file.display(),
    );

    fs::write(&plist_path, plist)?;
    let _ = Command::new("launchctl")
        .args(["unload"])
        .arg(&plist_path)
        .status();
    let status = Command::new("launchctl")
        .args(["load", "-w"])
        .arg(&plist_path)
        .status()
        .context("failed to run launchctl load")?;
    if !status.success() {
        anyhow::bail!("launchctl load failed with exit code {:?}", status.code());
    }

    println!("✅ 已安装并启动 macOS 后台守护进程");
    println!("   Plist: {}", plist_path.display());
    println!("   日志 : {}", log_file.display());
    Ok(())
}

#[cfg(target_os = "linux")]
fn install_systemd(bin_path: &Path, config_path: &Path) -> Result<()> {
    let unit_path = PathBuf::from("/etc/systemd/system/netbird-guard.service");
    let unit = format!(
        r#"[Unit]
Description=NetBird Session Watchdog & Feishu Notifier
After=network-online.target netbird.service
Wants=network-online.target

[Service]
Type=simple
ExecStart={bin} run --config {cfg}
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target
"#,
        bin = bin_path.display(),
        cfg = config_path.display(),
    );

    fs::write(&unit_path, unit).with_context(|| {
        format!(
            "failed to write {} (run with sudo on Linux)",
            unit_path.display()
        )
    })?;
    Command::new("systemctl").arg("daemon-reload").status()?;
    let st = Command::new("systemctl")
        .args(["enable", "--now", "netbird-guard"])
        .status()?;
    if !st.success() {
        anyhow::bail!("systemctl enable --now failed");
    }

    println!("✅ 已安装并启动 Linux systemd 服务");
    println!("   Unit: {}", unit_path.display());
    println!("   日志: journalctl -u netbird-guard -f");
    Ok(())
}
