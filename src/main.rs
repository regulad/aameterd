use std::io::IsTerminal;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use aameterd::Meter;
use anyhow::Context;
use clap::Parser;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tracing_subscriber::EnvFilter;

/// Metering proxy for aaoracled: serves the same HTTP contract, forwards
/// every request to a real aaoracled, and records each one into SQLite.
#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// Address to serve the aaoracled contract on.
    #[arg(long, env = "AAMETERD_LISTEN", default_value = "127.0.0.1:8182")]
    listen: SocketAddr,

    /// Address to serve aameterd's own contract on: live counts read from
    /// the metering database.
    #[arg(long, env = "AAMETERD_STATS_LISTEN", default_value = "127.0.0.1:8183")]
    stats_listen: SocketAddr,

    /// Base URL of the real aaoracled (usually an SSH tunnel to the device).
    #[arg(long, env = "AAMETERD_UPSTREAM", default_value = "http://127.0.0.1:8181")]
    upstream: reqwest::Url,

    /// SQLite database to meter into; created if missing.
    #[arg(long, env = "AAMETERD_DB", default_value = "aameterd.sqlite3")]
    db: PathBuf,

    /// Seconds to wait on aaoracled before answering 502. Key creation
    /// includes a round trip to Apple, so keep this generous.
    #[arg(long, env = "AAMETERD_UPSTREAM_TIMEOUT", default_value_t = 120)]
    upstream_timeout: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_ansi(std::io::stdout().is_terminal())
        .init();
    let args = Args::parse();

    let meter = Meter::open(&args.db).with_context(|| format!("opening {}", args.db.display()))?;
    let stats = aameterd::stats_router(meter.clone());
    let proxy = aameterd::router(args.upstream.as_str(), Duration::from_secs(args.upstream_timeout), meter)
        .context("building upstream client")?;

    // Both are bound before "up" is logged, so it means both are answering.
    let proxy_listener = bind(args.listen).await?;
    let stats_listener = bind(args.stats_listen).await?;
    tracing::info!(
        listen = %args.listen,
        stats_listen = %args.stats_listen,
        upstream = %args.upstream,
        db = %args.db.display(),
        "aameterd up"
    );

    // One signal stops both servers, each after draining its own connections.
    let (stop, stopped) = watch::channel(());
    tokio::spawn(async move {
        shutdown_signal().await;
        _ = stop.send(());
    });
    let shutdown = || {
        let mut stopped = stopped.clone();
        async move { _ = stopped.changed().await }
    };
    tokio::try_join!(
        axum::serve(proxy_listener, proxy.into_make_service_with_connect_info::<SocketAddr>())
            .with_graceful_shutdown(shutdown())
            .into_future(),
        axum::serve(stats_listener, stats).with_graceful_shutdown(shutdown()).into_future(),
    )?;
    Ok(())
}

async fn bind(addr: SocketAddr) -> anyhow::Result<TcpListener> {
    TcpListener::bind(addr).await.with_context(|| format!("binding {addr}"))
}

/// Ctrl-C, or SIGTERM on Unix (what `docker stop` sends; as a container's
/// PID 1, an unhandled SIGTERM is ignored and the stop times out to SIGKILL).
async fn shutdown_signal() {
    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => _ = sigterm.recv().await,
            Err(_) => std::future::pending().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        () = terminate => {}
    }
}
