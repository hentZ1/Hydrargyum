use cpal::traits::{DeviceTrait, HostTrait};

pub fn input() -> anyhow::Result<cpal::Stream> {
    let host = cpal::default_host();

    let device = host
        .default_input_device()
        .expect("no input device available");

    let mut supported_configs_range = device
        .supported_input_configs()
        .expect("error while querying configs");

    let supported_config = supported_configs_range
        .next()
        .expect("no supported config?!")
        .with_max_sample_rate();

    let config = supported_config.into();

    let stream = device.build_input_stream(
        config,
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            // aqui entra o producer.push() do ring buffer
        },
        move |err| {
            eprintln!("input stream error: {err}");
        },
        None,
    )?;

    Ok(stream)
}

pub fn output() -> anyhow::Result<cpal::Stream> {
    let host = cpal::default_host();

    let device = host
        .default_output_device()
        .expect("no output device available");

    let mut supported_configs_range = device
        .supported_output_configs()
        .expect("error while querying configs");

    let supported_config = supported_configs_range
        .next()
        .expect("no supported config?!")
        .with_max_sample_rate();

    let config = supported_config.into();

    let stream = device.build_output_stream(
        config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            // aqui entra o consumer.pop() do ring buffer
        },
        move |err| {
            eprintln!("output stream error: {err}");
        },
        None,
    )?;

    Ok(stream)
}
