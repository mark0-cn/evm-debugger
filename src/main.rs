#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let bind_addr =
        std::env::var("EVM_DEBUGGER_BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".to_string());
    evm_debugger::run_server(&bind_addr).await
}
