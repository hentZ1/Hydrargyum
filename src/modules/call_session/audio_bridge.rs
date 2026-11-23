//! Acoplamento entre os módulos de áudio existentes e a sessão de chamada.
//!
//! Este arquivo é a única fronteira que conhece `cpal`, `AudioEncoder` e
//! `AudioReceiver`. Nenhuma lógica interna de `audio_io`/`audio_dsp` é alterada:
//! apenas os ring buffers públicos e os handles são conectados.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use cpal::traits::StreamTrait;
use thiserror::Error;
use tokio::sync::mpsc;

use crate::modules::audio_dsp::codec::AudioEncoder;
use crate::modules::audio_dsp::jitter_buffer::{AudioReceiver, AudioReceiverHandle};
use crate::modules::audio_io::error::AudioError;
use crate::modules::audio_io::ring_buff::{AudioConsumer, AudioProducer, ring_buffer};
use crate::modules::audio_io::stream::{input_stream_builder, output_stream_builder};
use crate::modules::audio_types::SAMPLE_RATE;

const INPUT_RING_SAMPLES: usize = SAMPLE_RATE as usize;
const OUTPUT_RING_SAMPLES: usize = SAMPLE_RATE as usize;
const ENCODER_INTERVAL: Duration = Duration::from_millis(10);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Error)]
pub enum AudioBridgeError {
    #[error("falha no módulo de áudio: {0}")]
    Audio(#[from] AudioError),

    #[error("não foi possível criar a thread de áudio: {0}")]
    Thread(#[from] std::io::Error),

    #[error("timeout ao iniciar os dispositivos de áudio")]
    StartupTimeout,

    #[error("dispositivo de áudio: {0}")]
    Device(String),
}

/// Mantém os streams locais vivos e entrega pacotes Opus codificados.
///
/// A thread de áudio é dona dos `cpal::Stream` (que não são `Send` em todas as
/// plataformas) e do encoder. A `AudioReceiver`, que decodifica e mistura, roda
/// em sua própria thread interna.
pub struct AudioBridge {
    receiver: Option<AudioReceiver>,
    receiver_handle: Option<AudioReceiverHandle>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl AudioBridge {
    /// Inicia captura, reprodução e codificação. Retorna também o receiver dos
    /// pacotes Opus que devem ser enviados à malha.
    pub fn start() -> Result<(Self, mpsc::UnboundedReceiver<Vec<u8>>), AudioBridgeError> {
        let (input_producer, input_consumer) = ring_buffer(INPUT_RING_SAMPLES);
        let (playback_producer, playback_consumer) = ring_buffer(OUTPUT_RING_SAMPLES);

        let receiver = AudioReceiver::spawn(playback_producer)?;
        let receiver_handle = receiver.handle();

        let (packet_tx, packet_rx) = mpsc::unbounded_channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let (ready_tx, ready_rx) = sync_channel::<Result<(), String>>(1);

        let worker = thread::Builder::new()
            .name("hydrargyum-audio".to_owned())
            .spawn(move || {
                if let Err(error) = run_audio_thread(
                    input_producer,
                    input_consumer,
                    playback_consumer,
                    packet_tx,
                    worker_stop,
                    ready_tx,
                ) {
                    log::error!("áudio: thread encerrada: {error}");
                }
            })?;

        let startup = ready_rx
            .recv_timeout(STARTUP_TIMEOUT)
            .map_err(|_| AudioBridgeError::StartupTimeout)?;

        if let Err(error) = startup {
            stop.store(true, Ordering::Release);
            let _ = worker.join();
            return Err(AudioBridgeError::Device(error));
        }

        Ok((
            Self {
                receiver: Some(receiver),
                receiver_handle: Some(receiver_handle),
                stop,
                worker: Some(worker),
            },
            packet_rx,
        ))
    }

    /// Sessão sem dispositivos locais (testes, CI e clientes headless).
    pub fn disabled() -> Self {
        Self {
            receiver: None,
            receiver_handle: None,
            stop: Arc::new(AtomicBool::new(false)),
            worker: None,
        }
    }

    pub fn receiver_handle(&self) -> Option<AudioReceiverHandle> {
        self.receiver_handle.clone()
    }

    pub fn is_enabled(&self) -> bool {
        self.receiver_handle.is_some()
    }
}

impl Drop for AudioBridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        // `receiver` é liberado depois deste bloco e encerra sua thread.
        let _ = self.receiver.take();
    }
}

fn run_audio_thread(
    input_producer: AudioProducer,
    input_consumer: AudioConsumer,
    playback_consumer: AudioConsumer,
    packet_tx: mpsc::UnboundedSender<Vec<u8>>,
    stop: Arc<AtomicBool>,
    ready_tx: SyncSender<Result<(), String>>,
) -> Result<(), String> {
    let setup = (|| -> Result<_, String> {
        let input_stream =
            input_stream_builder(input_producer).map_err(|error| error.to_string())?;
        let output_stream =
            output_stream_builder(playback_consumer).map_err(|error| error.to_string())?;
        let encoder = AudioEncoder::new(input_consumer).map_err(|error| error.to_string())?;
        input_stream.play().map_err(|error| error.to_string())?;
        output_stream.play().map_err(|error| error.to_string())?;
        Ok((input_stream, output_stream, encoder))
    })();

    let (_input_stream, _output_stream, mut encoder) = match setup {
        Ok(resources) => {
            let _ = ready_tx.send(Ok(()));
            resources
        }
        Err(error) => {
            let _ = ready_tx.send(Err(error.clone()));
            return Err(error);
        }
    };

    while !stop.load(Ordering::Acquire) {
        match encoder.process() {
            Ok(packets) => {
                for packet in packets {
                    if packet_tx.send(packet).is_err() {
                        return Ok(());
                    }
                }
            }
            Err(error) => return Err(format!("falha ao codificar áudio: {error}")),
        }
        thread::sleep(ENCODER_INTERVAL);
    }

    Ok(())
}
