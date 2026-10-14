pub mod error;
pub mod ring_buff;
pub mod stream;

pub const AUDIO_CONFIG: cpal::StreamConfig = cpal::StreamConfig {
    channels: 1,
    sample_rate: cpal::SAMPLE_RATE_48K,
    buffer_size: cpal::BufferSize::Default,
};

pub struct StreamUtils {}
