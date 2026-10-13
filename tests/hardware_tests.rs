use std::{
    thread::sleep,
    time::{Duration, Instant},
};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use hydrargyum::modules::audio_io::{ring_buff::*, stream::input_stream_builder};

/// Drains the consumer for `dur`, returns the peak |sample| seen and
/// adds the number of samples read to `total`.
fn measure_peak(consumer: &mut AudioConsumer, dur: Duration, total: &mut usize) -> f32 {
    let mut buf = vec![0.0f32; 4096];
    let mut peak = 0.0f32;
    let start = Instant::now();

    while start.elapsed() < dur {
        loop {
            let n = consumer.read(&mut buf);
            if n == 0 {
                break;
            }
            *total += n;
            peak = buf[..n].iter().fold(peak, |m, s| m.max(s.abs()));
        }
        sleep(Duration::from_millis(10));
    }
    peak
}

#[test]
#[ignore] // needs a mic: cargo test --test hardware_tests -- --ignored --nocapture
fn mic_samples_and_voice_are_received() -> anyhow::Result<()> {
    // Informational: which device/config cpal is actually using.
    let host = cpal::default_host();
    let device = host.default_input_device().expect("no input device");
    let config = device.default_input_config()?;
    println!(
        "device: {:?} | {} Hz | {} ch | {:?}",
        device.description(),
        config.sample_rate(),
        config.channels(),
        config.sample_format()
    );
    let samples_per_sec = config.sample_rate() as usize * config.channels() as usize;

    let (producer, mut consumer) = ring_buffer(samples_per_sec); // ~1 s of headroom
    let stream = input_stream_builder(producer)?;
    stream.play()?;

    let mut total = 0usize;
    let t0 = Instant::now();

    // Phase 0: discard the startup transient.
    measure_peak(&mut consumer, Duration::from_millis(500), &mut total);

    // Phase 1: noise floor (stay quiet).
    println!(">>> FIQUE EM SILÊNCIO (1.5 s)...");
    let noise = measure_peak(&mut consumer, Duration::from_millis(1500), &mut total);

    // Phase 2: voice.
    println!(">>> FALE AGORA (3 s)...");
    let voice = measure_peak(&mut consumer, Duration::from_secs(3), &mut total);

    let elapsed = t0.elapsed().as_secs_f32();
    drop(stream);

    let expected = (samples_per_sec as f32 * elapsed) as usize;
    println!("samples: {total} (esperado ~{expected}) | ruído: {noise:.5} | voz: {voice:.5}");

    // OK 1: samples are flowing continuously (not just the first 100 ms).
    assert!(
        total as f32 > expected as f32 * 0.9,
        "perdeu samples: recebeu {total}, esperado ~{expected}"
    );

    // OK 2: the mic reacts to the voice.
    assert!(
        voice > 0.01 && voice > noise * 5.0,
        "mic não respondeu à voz (ruído {noise:.5}, voz {voice:.5})"
    );

    Ok(())
}
