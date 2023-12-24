use std::io::Write;
use tokio::process::Command;

//TODO make sure executable has correct permissions

#[cfg(any(target_os = "linux"))]
pub async fn create(
    service_name: String,
    cmd: String,
    environment: Vec<String>,
) -> Result<(), std::io::Error> {
    println!("Installing background service using systemd");

    let exe_path = std::env::current_exe()?.display().to_string();

    //TODO fix formatting here
    let mut env_string = String::new();
    for env in environment {
        env_string.push_str(&format!(r#"Environment="{}""#, env));
        env_string.push('\n');
    }

    let service = format!(
        r#"
[Unit]
Description={}
After=network-online.target
Wants=network-online.target

StartLimitIntervalSec=500
StartLimitBurst=5

[Service]
Restart=on-failure
RestartSec=5s
ExecStart={} {}
{}
[Install]
WantedBy=multi-user.target"#,
        service_name, exe_path, cmd, env_string
    );

    let service_path = format!("/etc/systemd/system/{}.service", service_name);
    let mut file = std::fs::File::create(&service_path)?;
    file.write_all(service.as_bytes())?;

    // check if selinux is enabled
    // let selinux_enabled = Command::new("getenforce")
    //     .output()
    //     .await?
    //     .stdout
    //     .starts_with(b"Enforcing");
    // if selinux_enabled {
    //     println!(
    //         "SELinux is enabled. Would you like to change the file's security context so it works with systemd (otherwise the service will probably not work)? (Y/n)"
    //     );
    //     if declined()? {
    //         println!("Not adding bin_t to the file's security context.");
    //     }

    //     Command::new("chcon")
    //         .arg("-t")
    //         .arg("bin_t")
    //         .arg(&service_path)
    //         .output()
    //         .await?;
    //     println!("bin_t added to file's security context.");
    // }
    //TODO: automatically do all of this stuff instead of asking

    println!(
        "Service installed to {}\nWould you like to reload the systemd daemon? (Y/n)",
        service_path
    );
    if declined()? {
        println!("Not reloading systemd daemon. To reload manually, run `systemctl daemon-reload`.\nTo start the service, run `systemctl start {service_name}`.\nTo enable the service on startup, run `systemctl enable {service_name}`.");
        return Ok(());
    }

    Command::new("systemctl")
        .arg("daemon-reload")
        .output()
        .await?;
    println!("Systemd daemon reloaded.\nWould you like to start the service? (Y/n)");

    if declined()? {
        println!("Not starting service.\nTo start the service, run `systemctl start {service_name}`.\nTo enable the service on startup, run `systemctl enable {service_name}`.");
        return Ok(());
    }

    Command::new("systemctl")
        .arg("start")
        .arg(&service_name)
        .output()
        .await?;

    println!(
        "Systemd service started.\nWould you like to enable the service so it runs on boot? (Y/n)"
    );

    if declined()? {
        println!("Not enabling service.\nTo enable the service on startup, run `systemctl enable {service_name}`.");
        return Ok(());
    }

    Command::new("systemctl")
        .arg("enable")
        .arg(&service_name)
        .output()
        .await?;
    println!("Service enabled.");

    Ok(())
}

// TODO create launchtl plist for macos
#[cfg(any(target_os = "macos"))]
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
        r#"<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>KeepAlive</key><true/><key>Label</key><string>com.clipboard-sync.{cmd}</string><key>ProgramArguments</key><array><string>{exe_path}</string><string>{cmd}</string></array><key>EnvironmentVariables</key><dict>{env_formatted}</dict><key>RunAtLoad</key><true/></dict></plist>"#
    );

    println!("{}", plist);

    let service_path = format!("com.clipboard-sync.{}.plist", cmd);
    let mut file = std::fs::File::create(&service_path)?;
    file.write_all(plist.as_bytes())?;

    Ok(())
}

#[cfg(any(target_os = "windows"))]
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
