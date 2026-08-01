mod cdp_client;

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use cdp_client::CdpClient;

#[derive(Parser)]
#[command(
    name = "remo",
    about = "Remote control bridge for iOS devices (real CDP)"
)]
struct Cli {
    /// Verbosity for the CLI. Use `remo -v devices` or `remo devices -v`.
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

/// Shared target-selection flags for every command that dials a device.
#[derive(clap::Args)]
struct Target {
    /// Device address (host:port). For simulator: 127.0.0.1:9930
    #[arg(short, long, default_value = "127.0.0.1:9930")]
    addr: SocketAddr,

    /// USB device ID (from `remo devices`). Overrides --addr.
    #[arg(short, long)]
    device: Option<u32>,
}

impl Target {
    async fn dial(&self) -> Result<CdpClient> {
        match self.device {
            Some(device_id) => CdpClient::connect_usb(device_id).await,
            None => CdpClient::connect_tcp(self.addr).await,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// List connected iOS devices (USB + Bonjour), resolved to dialable
    /// `ws://` CDP URLs.
    Devices,

    /// List capabilities registered on a device (`Remo.listCapabilities`).
    Capabilities {
        #[command(flatten)]
        target: Target,
    },

    /// Call a capability on a device (`Remo.invoke`).
    Call {
        #[command(flatten)]
        target: Target,

        /// Capability name to invoke.
        capability: String,

        /// JSON parameters (optional).
        #[arg(default_value = "{}")]
        params: String,

        /// Timeout in seconds.
        #[arg(short, long, default_value = "10")]
        timeout: u64,
    },

    /// Take a screenshot of the device (`Page.captureScreenshot`).
    Screenshot {
        #[command(flatten)]
        target: Target,

        /// Output file path.
        #[arg(short, long, default_value = "screenshot.jpg")]
        output: String,

        /// Image format: jpeg or png.
        #[arg(short, long, default_value = "jpeg")]
        format: String,

        /// JPEG quality (0.0 - 1.0).
        #[arg(short, long, default_value = "0.8")]
        quality: f64,
    },

    /// Show device and app information.
    Info {
        #[command(flatten)]
        target: Target,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_filter(cli.verbose)))
        .init();

    match cli.command {
        Command::Devices => cmd_devices().await?,
        Command::Capabilities { target } => cmd_capabilities(&target).await?,
        Command::Call {
            target,
            capability,
            params,
            timeout,
        } => cmd_call(&target, &capability, &params, timeout).await?,
        Command::Screenshot {
            target,
            output,
            format,
            quality,
        } => cmd_screenshot(&target, &output, &format, quality).await?,
        Command::Info { target } => cmd_info(&target).await?,
    }

    Ok(())
}

fn log_filter(verbose: u8) -> &'static str {
    match verbose {
        0 => "remo=warn",
        1 => "remo=info",
        2 => "remo=debug",
        _ => "remo=trace",
    }
}

async fn cmd_devices() -> Result<()> {
    println!("USB devices:");
    match remo_usbmuxd::list_devices().await {
        Ok(devices) if !devices.is_empty() => {
            for (device_id, dev) in devices {
                println!(
                    "  [{device_id}] {} (dial with `remo <command> --device {device_id}`)",
                    dev.serial
                );
            }
        }
        Ok(_) => println!("  (none found)"),
        Err(e) => println!("  usbmuxd unavailable: {e}"),
    }

    println!("\nBonjour devices (scanning for 3 seconds)...");
    match remo_bonjour::ServiceBrowser::browse(remo_bonjour::SERVICE_TYPE) {
        Ok((_browser, mut rx)) => {
            let mut found = Vec::new();
            let _ = tokio::time::timeout(Duration::from_secs(3), async {
                while let Some(event) = rx.recv().await {
                    if let remo_bonjour::BrowseEvent::Found(service) = event {
                        found.push(service);
                    }
                }
            })
            .await;

            if found.is_empty() {
                println!("  (none found)");
            }
            for service in found {
                match service.socket_addr() {
                    Some(addr) => {
                        println!(
                            "  {} -> ws://{addr}/devtools/page/1 (dial with --addr {addr})",
                            service.name
                        );
                    }
                    None => {
                        println!(
                            "  {} -> could not resolve {}:{}",
                            service.name, service.host, service.port
                        );
                    }
                }
            }
        }
        Err(e) => println!("  Bonjour unavailable: {e}"),
    }

    Ok(())
}

async fn cmd_capabilities(target: &Target) -> Result<()> {
    let mut client = target.dial().await?;
    let mut names = client.list_capabilities().await?;
    names.sort();
    println!("{}", serde_json::to_string_pretty(&names)?);
    Ok(())
}

async fn cmd_call(target: &Target, capability: &str, params: &str, timeout: u64) -> Result<()> {
    let params: serde_json::Value = serde_json::from_str(params)?;
    let mut client = target.dial().await?;

    println!("Calling '{capability}'...");
    let result = cdp_client::with_timeout(
        Duration::from_secs(timeout),
        client.invoke_capability(capability, params),
    )
    .await?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

async fn cmd_screenshot(target: &Target, output: &str, format: &str, quality: f64) -> Result<()> {
    let mut client = target.dial().await?;

    let bytes = cdp_client::with_timeout(
        Duration::from_secs(15),
        client.capture_screenshot(format, quality),
    )
    .await?;

    std::fs::write(output, &bytes)?;
    println!("Screenshot saved to {output} ({} bytes)", bytes.len());
    Ok(())
}

async fn cmd_info(target: &Target) -> Result<()> {
    let mut client = target.dial().await?;

    let device_data = cdp_client::with_timeout(
        Duration::from_secs(5),
        client.invoke_capability("__device_info", serde_json::json!({})),
    )
    .await?;
    let app_data = cdp_client::with_timeout(
        Duration::from_secs(5),
        client.invoke_capability("__app_info", serde_json::json!({})),
    )
    .await?;

    println!("=== Device ===");
    println!(
        "  Name:    {}",
        device_data["name"].as_str().unwrap_or("unknown")
    );
    println!(
        "  Model:   {}",
        device_data["model"].as_str().unwrap_or("unknown")
    );
    println!(
        "  OS:      {} {}",
        device_data["system_name"].as_str().unwrap_or("?"),
        device_data["system_version"].as_str().unwrap_or("?")
    );
    println!(
        "  Screen:  {:.0}x{:.0} @{:.0}x",
        device_data["screen_width"].as_f64().unwrap_or(0.0),
        device_data["screen_height"].as_f64().unwrap_or(0.0),
        device_data["screen_scale"].as_f64().unwrap_or(1.0),
    );

    println!("\n=== App ===");
    println!(
        "  Name:    {}",
        app_data["display_name"].as_str().unwrap_or("unknown")
    );
    println!(
        "  Bundle:  {}",
        app_data["bundle_id"].as_str().unwrap_or("unknown")
    );
    println!(
        "  Version: {} ({})",
        app_data["version"].as_str().unwrap_or("?"),
        app_data["build"].as_str().unwrap_or("?")
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{log_filter, Cli};
    use clap::Parser;

    #[test]
    fn default_logging_is_quiet() {
        assert_eq!(log_filter(0), "remo=warn");
    }

    #[test]
    fn verbose_flags_increase_logging_detail() {
        assert_eq!(log_filter(1), "remo=info");
        assert_eq!(log_filter(2), "remo=debug");
        assert_eq!(log_filter(3), "remo=trace");
    }

    #[test]
    fn verbose_flag_is_accepted_after_subcommand() {
        let cli = Cli::try_parse_from(["remo", "devices", "-v"]).expect("devices -v should parse");

        assert_eq!(cli.verbose, 1);
    }
}
