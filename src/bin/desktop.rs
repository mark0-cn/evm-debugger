fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let runtime = tokio::runtime::Runtime::new()?;
    let handle = runtime.block_on(evm_debugger::start_server("127.0.0.1:0"))?;
    let url = format!("http://{}", handle.addr);

    let event_loop = tao::event_loop::EventLoop::new();
    let window = tao::window::WindowBuilder::new()
        .with_title("EVM Debugger")
        .build(&event_loop)?;
    let _webview = wry::WebViewBuilder::new().with_url(&url).build(&window)?;

    let mut server = Some(handle);
    event_loop.run(move |event, _, control_flow| {
        *control_flow = tao::event_loop::ControlFlow::Wait;
        if let tao::event::Event::WindowEvent { event, .. } = event {
            if matches!(event, tao::event::WindowEvent::CloseRequested) {
                if let Some(s) = server.take() {
                    s.abort();
                }
                *control_flow = tao::event_loop::ControlFlow::Exit;
            }
        }
    });
}
