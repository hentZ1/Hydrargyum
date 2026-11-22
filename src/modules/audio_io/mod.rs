use crate::modules::audio_types::SAMPLE_RATE;

pub mod error;
pub mod ring_buff;
pub mod stream;

pub const AUDIO_CONFIG: cpal::StreamConfig = cpal::StreamConfig {
    channels: 1,
    sample_rate: SAMPLE_RATE,
    buffer_size: cpal::BufferSize::Default,
};
