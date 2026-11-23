use anyhow::Context;
use clap::Parser;
use hydrargyum::modules::call_session::{CallSession, SessionConfig};
use hydrargyum::modules::webrtc_transport::TransportConfig;

#[derive(Debug, Parser)]
#[command(
    name = "hydrargyum",
    version,
    about = "Chamada de voz P2P em malha completa (2 a 8 participantes)"
)]
struct Args {
    /// URL WebSocket do servidor Rendezvous.
    #[arg(long, default_value = "ws://127.0.0.1:9000")]
    server: String,

    /// Nome da sala.
    #[arg(long, default_value = "geral")]
    room: String,

    /// Nome exibido para os demais participantes.
    #[arg(long, default_value = "Hydrargyum")]
    name: String,

    /// Executa sem captura/reprodução local (útil para testes e servidores).
    #[arg(long)]
    no_audio: bool,

    /// Não usa STUN; adequado para testes em loopback.
    #[arg(long)]
    loopback: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse();
    let transport = if args.loopback {
        TransportConfig::loopback()
    } else {
        TransportConfig::default()
    };

    let config = SessionConfig {
        rendezvous_url: args.server,
        room: args.room,
        display_name: args.name,
        enable_audio: !args.no_audio,
        transport,
        ..SessionConfig::default()
    };

    let (session, shutdown) = CallSession::connect(config)
        .await
        .context("não foi possível entrar na sala")?;

    log::info!(
        "peer local {} (áudio {})",
        session.peer_id(),
        if session.audio_enabled() {
            "ativo"
        } else {
            "desativado"
        }
    );

    let mut task = tokio::spawn(session.run());
    tokio::select! {
        result = &mut task => {
            result.context("tarefa da sessão falhou")??;
        }
        _ = tokio::signal::ctrl_c() => {
            log::info!("encerrando sessão...");
            shutdown.request();
            task.await.context("tarefa da sessão falhou")??;
        }
    }

    Ok(())
}
