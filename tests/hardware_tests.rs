use std::{
    thread::sleep,
    time::{Duration, Instant},
};

use cpal::traits::StreamTrait;
use hydrargyum::modules::audio_io::{AUDIO_CONFIG, ring_buff::*, stream::input_stream_builder};

const WINDOW: Duration = Duration::from_millis(100);

fn measure_window(consumer: &mut AudioConsumer, total: &mut usize) -> (f32, f32) {
    let mut buf = vec![0.0f32; 4096];
    let mut window: Vec<f32> = Vec::new();
    let start = Instant::now();

    while start.elapsed() < WINDOW {
        loop {
            let n = consumer.read(&mut buf);
            if n == 0 {
                break;
            }
            *total += n;
            window.extend_from_slice(&buf[..n]);
        }
        sleep(Duration::from_millis(5));
    }

    if window.is_empty() {
        return (0.0, 0.0);
    }
    let peak = window.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let mean = window.iter().sum::<f32>() / window.len() as f32;
    let rms = (window.iter().map(|s| (s - mean).powi(2)).sum::<f32>() / window.len() as f32).sqrt();
    (peak, rms)
}

fn run_phase(
    label: &str,
    consumer: &mut AudioConsumer,
    windows: usize,
    total: &mut usize,
) -> Vec<f32> {
    let mut rms_list = Vec::with_capacity(windows);
    for i in 0..windows {
        let (peak, rms) = measure_window(consumer, total);
        println!("[{label} {:>4} ms] peak {peak:.5} | rms {rms:.5}", i * 100);
        rms_list.push(rms);
    }
    rms_list
}

fn median(v: &[f32]) -> f32 {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    s[s.len() / 2]
}

#[test]
#[ignore]
fn mic_samples_and_voice_are_received() -> anyhow::Result<()> {
    let samples_per_sec = AUDIO_CONFIG.sample_rate as usize * AUDIO_CONFIG.channels as usize;
    println!(
        "config: {} Hz | {} ch",
        AUDIO_CONFIG.sample_rate, AUDIO_CONFIG.channels
    );

    let (producer, mut consumer) = ring_buffer(samples_per_sec);
    let stream = input_stream_builder(producer)?;
    stream.play()?;

    let mut total = 0usize;
    let t0 = Instant::now();

    run_phase("transiente", &mut consumer, 5, &mut total);

    println!(">>> FIQUE EM SILÊNCIO (1.5 s), sem tocar no teclado...");
    let noise_rms = run_phase("silêncio  ", &mut consumer, 15, &mut total);

    println!(">>> FALE AGORA (3 s), de forma contínua...");
    let voice_rms = run_phase("voz       ", &mut consumer, 30, &mut total);

    let elapsed = t0.elapsed().as_secs_f32();
    drop(stream);

    let expected = samples_per_sec as f32 * elapsed;
    let noise = median(&noise_rms);
    let voice = voice_rms.iter().cloned().fold(0.0f32, f32::max);
    println!(
        "samples: {total} (esperado ~{expected:.0}) | ruído (mediana rms): {noise:.5} | voz (max rms): {voice:.5}"
    );

    let ratio = total as f32 / expected;
    assert!(
        (0.9..=1.1).contains(&ratio),
        "contagem fora da faixa: recebeu {total}, esperado ~{expected:.0} (ratio {ratio:.2})"
    );

    assert!(
        voice > 0.005 && voice > noise * 5.0,
        "mic não respondeu à voz (ruído {noise:.5}, voz {voice:.5})"
    );

    Ok(())
}
