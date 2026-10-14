use opus::{Application, Channels, Decoder, Encoder};

use crate::modules::audio_io::{
    error::AudioError,
    ring_buff::{AudioConsumer, AudioProducer},
};

pub const SAMPLE_RATE: u32 = 48_000;
pub const FRAME_SIZE: usize = (SAMPLE_RATE as usize * 20) / 1000;
const MAX_PACKET: usize = 4000;
const WARMUP_SAMPLES: usize = SAMPLE_RATE as usize;
const READ_CHUNK: usize = 1024;

pub struct AudioEncoder {
    encoder: Encoder,
    consumer: AudioConsumer,
    accumulator: Vec<f32>,
    scratch: Vec<f32>,
    out: Vec<u8>,
    warmup: usize,
}

impl AudioEncoder {
    pub fn new(consumer: AudioConsumer) -> Result<Self, AudioError> {
        let encoder = Encoder::new(SAMPLE_RATE, Channels::Mono, Application::Voip)?;

        Ok(Self {
            encoder,
            consumer,
            accumulator: Vec::with_capacity(FRAME_SIZE * 3),
            scratch: vec![0.0; READ_CHUNK],
            out: vec![0u8; MAX_PACKET],
            warmup: WARMUP_SAMPLES,
        })
    }

    pub fn process(&mut self) -> Result<Vec<Vec<u8>>, AudioError> {
        loop {
            let n = self.consumer.read(&mut self.scratch);
            if n == 0 {
                break;
            }

            let skip = self.warmup.min(n);
            self.warmup -= skip;
            self.accumulator.extend_from_slice(&self.scratch[skip..n]);
        }

        let mut packets = Vec::new();

        while self.accumulator.len() >= FRAME_SIZE {
            let len = self
                .encoder
                .encode_float(&self.accumulator[..FRAME_SIZE], &mut self.out)?;
            packets.push(self.out[..len].to_vec());
            self.accumulator.drain(..FRAME_SIZE);
        }

        Ok(packets)
    }
}

pub struct AudioDecoder {
    decoder: Decoder,
    producer: AudioProducer,
    pcm: Vec<f32>,
    dropped: usize,
}

impl AudioDecoder {
    pub fn new(producer: AudioProducer) -> Result<Self, AudioError> {
        let decoder = Decoder::new(SAMPLE_RATE, Channels::Mono)?;

        Ok(Self {
            decoder,
            producer,
            pcm: vec![0.0; FRAME_SIZE],
            dropped: 0,
        })
    }

    pub fn decode(&mut self, packet: Option<&[u8]>) -> Result<(), AudioError> {
        let input = packet.unwrap_or(&[]);
        let n = self.decoder.decode_float(input, &mut self.pcm, false)?;

        let written = self.producer.write(&self.pcm[..n]);
        self.dropped += n - written;

        Ok(())
    }

    pub fn dropped(&self) -> usize {
        self.dropped
    }
}
