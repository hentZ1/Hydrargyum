use cpal::traits::StreamTrait;

use crate::modules::audio_io::{ring_buff::*, stream::input_stream_builder};
pub fn test() -> Result<(), anyhow::Error> {
    let _encoder = opus::Encoder::new(
        cpal::SAMPLE_RATE_48K,
        opus::Channels::Mono,
        opus::Application::Voip,
    );
    let (producer, _consumer) = ring_buffer(4800);
    let stream = input_stream_builder(producer)?;

    stream.play()?;

    Ok(())
}
