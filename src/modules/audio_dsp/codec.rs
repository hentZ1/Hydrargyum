use opus::{Application, Channels, Encoder};

use crate::modules::audio_io::{error::AudioError, ring_buff::AudioConsumer};
use crate::modules::audio_types::{FRAME_SIZE, SAMPLE_RATE};

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
