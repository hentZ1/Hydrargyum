use clap::Parser;
use tokio::net::TcpListener;

#[derive(Debug, Parser)]
#[command(
    name = "hydrargyum-rendezvous",
    version,
    about = "Servidor Rendezvous mínimo para o handshake WebRTC"
)]
struct Args {
    /// Endereço TCP em que o servidor WebSocket escuta.
    #[arg(long, default_value = "127.0.0.1:9000")]
    bind: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();
    let listener = TcpListener::bind(&args.bind).await?;
    log::info!("Rendezvous ouvindo em ws://{}", listener.local_addr()?);

    hydrargyum::modules::signaling::serve(listener).await?;
    Ok(())
}
