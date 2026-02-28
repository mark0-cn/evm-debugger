#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let handle = evm_debugger::start_server("127.0.0.1:0").await?;
    let url = format!("http://{}", handle.addr);
    let _ = webbrowser::open(&url);
    handle.wait().await
}
