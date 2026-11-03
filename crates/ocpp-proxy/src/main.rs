use std::process::ExitCode;

use ocpp_proxy::{spawn, ProxyOptions};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();
    let options = match ProxyOptions::from_env() {
        Ok(options) => options,
        Err(err) => {
            eprintln!("failed to load config: {err}");
            return ExitCode::from(1);
        }
    };
    let running = match spawn(options).await {
        Ok(running) => running,
        Err(err) => {
            eprintln!("failed to start ocpp proxy: {err}");
            return ExitCode::from(1);
        }
    };
    tracing::info!(address = %running.addr, "ocpp proxy listening");
    let _ = tokio::signal::ctrl_c().await;
    running.shutdown().await;
    ExitCode::SUCCESS
}
