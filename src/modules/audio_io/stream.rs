use cpal::{
    InputCallbackInfo, OutputCallbackInfo, Stream,
    traits::{DeviceTrait, HostTrait},
};

use crate::modules::audio_io::{devices::*, error::AudioError, ring_buff::*};

pub fn input_stream_builder(mut producer: AudioProducer) -> Result<Stream, AudioError> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or(AudioError::NoInputDevice)?;

    let supported_input_config = input_config()?;
    let stream_input_config = supported_input_config.config();

    let input_stream = device.build_input_stream(
        stream_input_config,
        move |data: &[f32], _info: &InputCallbackInfo| {
            producer.write(data);
        },
        move |err: cpal::Error| eprintln!("error on the stream input {:?}", err),
        None,
    )?;

    Ok(input_stream)
}

pub fn output_stream_builder(mut consumer: AudioConsumer) -> Result<Stream, AudioError> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or(AudioError::NoOutputDevice)?;

    let supported_output_config = output_config()?;
    let stream_ouput_config = supported_output_config.config();

    let output_stream = device.build_output_stream(
        stream_ouput_config,
        move |data: &mut [f32], _info: &OutputCallbackInfo| {
            consumer.read(data);
        },
        move |err: cpal::Error| eprintln!("error on the stream output {:?}", err),
        None,
    )?;

    Ok(output_stream)
}
