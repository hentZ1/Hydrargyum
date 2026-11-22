use crate::modules::audio_io::{AUDIO_CONFIG, error::AudioError, ring_buff::*};
use cpal::{
    InputCallbackInfo, OutputCallbackInfo, Stream,
    traits::{DeviceTrait, HostTrait},
};

pub fn input_stream_builder(mut producer: AudioProducer) -> Result<Stream, AudioError> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or(AudioError::NoInputDevice)?;

    let input_stream = device.build_input_stream(
        AUDIO_CONFIG,
        move |data: &[f32], _info: &InputCallbackInfo| {
            producer.write(data);
        },
        move |err: cpal::Error| eprintln!("error on the stream input {:?}", err),
        None,
    )?;

    Ok(input_stream)
}

pub fn fill_underrun(data: &mut [f32], filled: usize) {
    let filled = filled.min(data.len());
    data[filled..].fill(0.0);
}

pub fn output_stream_builder(mut consumer: AudioConsumer) -> Result<Stream, AudioError> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or(AudioError::NoOutputDevice)?;

    let output_stream = device.build_output_stream(
        AUDIO_CONFIG,
        move |data: &mut [f32], _info: &OutputCallbackInfo| {
            let filled = consumer.read(data);
            fill_underrun(data, filled);
        },
        move |err: cpal::Error| eprintln!("error on the stream output {:?}", err),
        None,
    )?;

    Ok(output_stream)
}
