mod binance_feed;
mod config;
mod edge_detector;
mod polymarket_client;
mod risk_manager;
mod settlement_monitor;
mod trader;
mod types;

use tracing_subscriber::prelude::*;
use tracing_subscriber::{fmt, EnvFilter};

#[tokio::main]
async fn main() {
    // File appender for bot.log
    let file_appender = tracing_appender::rolling::never(".", config::LOG_FILE);
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    let filter = EnvFilter::from_default_env()
        .add_directive("info".parse().unwrap())
        .add_directive("tungstenite=warn".parse().unwrap())
        .add_directive("reqwest=warn".parse().unwrap())
        .add_directive("hyper=warn".parse().unwrap());

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(false))
        .with(fmt::layer().with_target(false).with_ansi(false).with_writer(non_blocking))
        .init();

    let cfg = config::Config::load().expect("Failed to load config");
    let trader = trader::Trader::new(cfg);
    trader.start().await;
}
