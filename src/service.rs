// use std::io::Write;
// use tokio::process::Command;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn daemon(service_name: String) {
    println!("Running in background");

    let daemonize = daemonize::Daemonize::new()
        .pid_file(format!("/tmp/{}.pid", service_name))
        .working_directory("/tmp")
        .user("root")
        .group("root");

    match daemonize.start() {
        Ok(_) => println!("Success, daemonized"),
        Err(e) => eprintln!("Error, {}", e),
    }
}

//TODO make sure executable has correct permissions

// #[cfg(any(target_os = "linux"))]
// pub async fn create(args: String, service_name: String) -> Result<(), std::io::Error> {
//     println!("Installing background service using systemd");

//     let exe_path = std::env::current_exe()?.display().to_string();

//     let service = format!(
//         r#"
// [Unit]
// Description={}
// After=network-online.target
// Wants=network-online.target

// StartLimitIntervalSec=500
// StartLimitBurst=5

// [Service]
// Restart=on-failure
// RestartSec=5s
// ExecStart={} {}

// [Install]
// WantedBy=multi-user.target"#,
//         service_name, exe_path, args
//     );

//     let service_path = format!("/etc/systemd/system/{}.service", service_name);
//     let mut file = std::fs::File::create(&service_path)?;
//     file.write_all(service.as_bytes())?;

//     println!(
//         "Service installed to {}\nWould you like to reload the systemd daemon? (y/n)",
//         service_path
//     );

//     let mut input = String::new();
//     std::io::stdin().read_line(&mut input)?;
//     if input.trim() != "y" {
//         println!("Not reloading systemd daemon. To reload manually, run `systemctl daemon-reload`.\nTo start the service, run `systemctl start {service_name}`.\nTo enable the service on startup, run `systemctl enable {service_name}`.");
//         return Ok(());
//     }
//     Command::new("systemctl")
//         .arg("daemon-reload")
//         .output()
//         .await?;
//     println!("Systemd daemon reloaded.\nWould you like to start the service? (y/n)");

//     let mut input = String::new();
//     std::io::stdin().read_line(&mut input)?;
//     if input.trim() != "y" {
//         println!("Not starting service.\nTo start the service, run `systemctl start {service_name}`.\nTo enable the service on startup, run `systemctl enable {service_name}`.");
//         return Ok(());
//     }
//     Command::new("systemctl")
//         .arg("start")
//         .arg(&service_name)
//         .output()
//         .await?;

//     println!(
//         "Systemd service started.\nWould you like to enable the service so it runs on boot? (y/n)"
//     );

//     let mut input = String::new();
//     std::io::stdin().read_line(&mut input)?;
//     if input.trim() != "y" {
//         println!("Not enabling service.\nTo enable the service on startup, run `systemctl enable {service_name}`.");
//         return Ok(());
//     }
//     Command::new("systemctl")
//         .arg("enable")
//         .arg(&service_name)
//         .output()
//         .await?;
//     println!("Service enabled.");

//     Ok(())
// }

// #[cfg(any(target_os = "macos"))]
// fn create() -> Result<(), ()> {
//     todo!("create service on macos");
//     Ok(())
// }

// #[cfg(any(target_os = "windows"))]
// fn create() -> Result<(), ()> {
//     todo!("create service on windows");
//     Ok(())
// }
