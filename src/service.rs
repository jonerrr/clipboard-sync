use std::io::Write;
use tokio::process::Command;

#[cfg(target_os = "linux")]
pub async fn create(
    service_name: String,
    cmd: String,
    environment: Vec<(String, String)>,
) -> Result<(), std::io::Error> {
    println!("Installing background service using systemd");

    let exe_path = std::env::current_exe()?.display().to_string();

    let config_dir = Command::new("systemd-path")
        .arg("user-configuration")
        .output()
        .await?
        .stdout;
    // remove the newline at the end
    let config_dir = &config_dir[..config_dir.len() - 1];

    let config_dir = format!(
        "{}/systemd/user",
        String::from_utf8(config_dir.to_vec()).unwrap()
    );
    // check if the directory exists and if not, create it
    if !std::path::Path::new(&config_dir).exists() {
        std::fs::create_dir(&config_dir)?;
    }

    let env_formatted = environment
        .iter()
        .map(|(key, value)| format!(r#"Environment="{}={}""#, key, value))
        .collect::<Vec<String>>()
        .join("\n");

    let service: String = format!(
        r#"
[Unit]
Description={service_name}
After=network-online.target
Wants=network-online.target

StartLimitIntervalSec=500
StartLimitBurst=5

[Service]
Restart=on-failure
RestartSec=5s
ExecStart={exe_path} {cmd}
{env_formatted}
[Install]
WantedBy=multi-user.target"#
    );

    // let service_path = format!("/etc/systemd/user/{}.service", service_name);
    let service_path = format!("{}/{}.service", config_dir, service_name);
    let mut file = std::fs::File::create(&service_path)?;
    file.write_all(service.as_bytes())?;

    // check if selinux is enabled
    let selinux_enabled = Command::new("getenforce")
        .output()
        .await?
        .stdout
        .starts_with(b"Enforcing");
    if selinux_enabled {
        println!("WARNING: SELinux is enabled. This may prevent the service from starting.");
    }

    Command::new("systemctl")
        .arg("--user")
        .arg("daemon-reload")
        .output()
        .await?;

    Command::new("systemctl")
        .arg("--user")
        .arg("start")
        .arg(&service_name)
        .output()
        .await?;

    println!("Would you like to enable the service so it runs on boot? (Y/n)");

    if declined()? {
        println!("Not enabling service.\nTo enable the service on startup, run `systemctl enable {service_name}`.");
        return Ok(());
    }

    Command::new("systemctl")
        .arg("--user")
        .arg("enable")
        .arg(&service_name)
        .output()
        .await?;
    println!("Service enabled.");

    Ok(())
}

// TODO create launchtl plist for macos
#[cfg(target_os = "macos")]
pub async fn create(
    service_name: String,
    cmd: String,
    environment: Vec<(String, String)>,
) -> Result<(), std::io::Error> {
    println!("Installing as launchd daemon.");

    let exe_path = std::env::current_exe()?.display().to_string();

    let env_formatted = environment
        .iter()
        .map(|(key, value)| format!(r#"<key>{}</key><string>{}</string>"#, key, value))
        .collect::<Vec<String>>()
        .join("");

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>KeepAlive</key><true/><key>Label</key><string>com.cync.{cmd}</string><key>ProgramArguments</key><array><string>{exe_path}</string><string>{cmd}</string></array><key>EnvironmentVariables</key><dict>{env_formatted}</dict><key>RunAtLoad</key><true/></dict></plist>"#
    );

    println!("{}", plist);

    let service_path = format!("com.cync.{}.plist", cmd);
    let mut file = std::fs::File::create(&service_path)?;
    file.write_all(plist.as_bytes())?;

    Ok(())
}

#[cfg(target_os = "windows")]
pub async fn create(
    service_name: String,
    cmd: String,
    environment: Vec<(String, String)>,
) -> Result<(), std::io::Error> {
    todo!("windows service creation");
    Ok(())
}

fn declined() -> Result<bool, std::io::Error> {
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(input.trim().to_lowercase() == "n")
}
