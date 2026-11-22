use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use log;
use neteq::{AudioPacket, NetEq, NetEqConfig, NetEqError, RtpHeader};
use opus::{Channels, Decoder};
use ringbuf::traits::{Observer, Producer};

use crate::modules::audio_io::ring_buff::AudioProducer;
use crate::modules::audio_types::{FRAME_MS, OPUS_PAYLOAD_TYPE, SAMPLE_RATE};

pub const OUT_FRAME_SAMPLES: usize = (SAMPLE_RATE / 100) as usize;

const TARGET_FILL_MS: usize = 40;
const TARGET_FILL_SAMPLES: usize = (SAMPLE_RATE as usize / 1000) * TARGET_FILL_MS;
const MAX_DELAY_MS: u32 = 250;
const COMMAND_QUEUE_CAPACITY: usize = 64;
const IDLE_SLEEP: Duration = Duration::from_millis(2);
const MAX_OPUS_FRAME_MS: usize = 120;
const MAX_OPUS_FRAME_SAMPLES: usize = (SAMPLE_RATE as usize * MAX_OPUS_FRAME_MS) / 1000;

pub type ParticipantId = u32;

struct NetEqOpusDecoder {
    decoder: Decoder,
    sample_rate: u32,
    channels: u8,
    pcm: Vec<f32>,
}

impl NetEqOpusDecoder {
    fn new(sample_rate: u32, channels: u8) -> Result<Self, String> {
        let opus_channels = match channels {
            1 => Channels::Mono,
            2 => Channels::Stereo,
            n => return Err(format!("unsupported channel count: {n}")),
        };
        let decoder = Decoder::new(sample_rate, opus_channels).map_err(|e| e.to_string())?;

        Ok(Self {
            decoder,
            sample_rate,
            channels,
            pcm: vec![0.0; MAX_OPUS_FRAME_SAMPLES * channels as usize],
        })
    }
}

impl neteq::codec::AudioDecoder for NetEqOpusDecoder {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn channels(&self) -> u8 {
        self.channels
    }

    fn decode(&mut self, encoded: &[u8]) -> neteq::Result<Vec<f32>> {
        let decoded = self
            .decoder
            .decode_float(encoded, &mut self.pcm, false)
            .map_err(|e| NetEqError::DecoderError(e.to_string()))?;
        let total = decoded * self.channels as usize;

        Ok(self.pcm[..total].to_vec())
    }
}

pub fn make_audio_packet(
    sequence_number: u16,
    timestamp: u32,
    ssrc: u32,
    payload_type: u8,
    marker: bool,
    payload: Vec<u8>,
) -> AudioPacket {
    let header = RtpHeader::new(sequence_number, timestamp, ssrc, payload_type, marker);
    AudioPacket::new(header, payload, SAMPLE_RATE, 1, FRAME_MS)
}

pub fn add_into(dst: &mut [f32], src: &[f32]) {
    for (d, s) in dst.iter_mut().zip(src) {
        *d += *s;
    }
}

pub fn clamp_in_place(samples: &mut [f32]) {
    for s in samples {
        *s = s.clamp(-1.0, 1.0);
    }
}

pub struct JitterEngine {
    config: NetEqConfig,
    peers: HashMap<ParticipantId, NetEq>,
}

impl Default for JitterEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl JitterEngine {
    pub fn new() -> Self {
        let config = NetEqConfig {
            sample_rate: SAMPLE_RATE,
            channels: 1,
            max_delay_ms: MAX_DELAY_MS,
            ..Default::default()
        };
        Self {
            config,
            peers: HashMap::new(),
        }
    }

    fn new_peer(config: &NetEqConfig) -> Result<NetEq, String> {
        let mut neteq = NetEq::new(config.clone()).map_err(|e| e.to_string())?;
        let decoder = NetEqOpusDecoder::new(SAMPLE_RATE, 1)?;
        neteq.register_decoder(OPUS_PAYLOAD_TYPE, Box::new(decoder));
        Ok(neteq)
    }

    pub fn insert(&mut self, id: ParticipantId, packet: AudioPacket) {
        let neteq = match self.peers.entry(id) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => match Self::new_peer(&self.config) {
                Ok(neteq) => entry.insert(neteq),
                Err(err) => {
                    log::warn!("participante {id}: falha ao criar o NetEq: {err}");
                    return;
                }
            },
        };
        if let Err(err) = neteq.insert_packet(packet) {
            log::warn!("participante {id}: pacote rejeitado: {err}");
        }
    }

    pub fn remove(&mut self, id: ParticipantId) {
        self.peers.remove(&id);
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }

    pub fn mix_frame(&mut self, out: &mut [f32]) {
        out.fill(0.0);
        for (id, neteq) in self.peers.iter_mut() {
            match neteq.get_audio() {
                Ok(frame) => add_into(out, &frame.samples),
                Err(err) => log::warn!("participante {id}: get_audio falhou: {err}"),
            }
        }
        clamp_in_place(out);
    }
}

enum Command {
    Packet(ParticipantId, AudioPacket),
    Leave(ParticipantId),
}

#[derive(Clone)]
pub struct AudioReceiverHandle {
    tx: mpsc::SyncSender<Command>,
}

impl AudioReceiverHandle {
    pub fn push_packet(&self, id: ParticipantId, packet: AudioPacket) -> bool {
        match self.tx.try_send(Command::Packet(id, packet)) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
        }
    }

    pub fn leave(&self, id: ParticipantId) {
        let _ = self.tx.send(Command::Leave(id));
    }
}

pub struct AudioReceiver {
    handle: AudioReceiverHandle,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl AudioReceiver {
    pub fn spawn(playback: AudioProducer) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::sync_channel(COMMAND_QUEUE_CAPACITY);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::Builder::new()
            .name("audio-receiver".into())
            .spawn(move || run(playback, rx, worker_stop))?;
        Ok(Self {
            handle: AudioReceiverHandle { tx },
            stop,
            worker: Some(worker),
        })
    }

    pub fn handle(&self) -> AudioReceiverHandle {
        self.handle.clone()
    }
}

impl Drop for AudioReceiver {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn drain_commands(engine: &mut JitterEngine, rx: &mpsc::Receiver<Command>) {
    while let Ok(command) = rx.try_recv() {
        match command {
            Command::Packet(id, packet) => engine.insert(id, packet),
            Command::Leave(id) => engine.remove(id),
        }
    }
}

fn run(mut playback: AudioProducer, rx: mpsc::Receiver<Command>, stop: Arc<AtomicBool>) {
    let mut engine = JitterEngine::new();
    let mut frame = [0.0f32; OUT_FRAME_SAMPLES];

    while !stop.load(Ordering::Acquire) {
        drain_commands(&mut engine, &rx);

        while !engine.is_empty()
            && playback.inner.occupied_len() < TARGET_FILL_SAMPLES
            && playback.inner.vacant_len() >= OUT_FRAME_SAMPLES
        {
            engine.mix_frame(&mut frame);
            playback.inner.push_slice(&frame);
        }

        thread::sleep(IDLE_SLEEP);
    }
}
