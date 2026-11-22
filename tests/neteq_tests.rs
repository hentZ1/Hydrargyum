use hydrargyum::modules::{
    audio_dsp::{
        codec::AudioEncoder,
        jitter_buffer::{JitterEngine, OUT_FRAME_SAMPLES, make_audio_packet},
    },
    audio_io::ring_buff::ring_buffer,
    audio_types::{FRAME_SIZE, OPUS_PAYLOAD_TYPE, SAMPLE_RATE},
};

const FREQ_HZ: f32 = 1_000.0;
const PACKET_COUNT: usize = 100;
const WARMUP_SAMPLES: usize = SAMPLE_RATE as usize;
const SKIP_SAMPLES: usize = SAMPLE_RATE as usize;
const WINDOW_SAMPLES: usize = SAMPLE_RATE as usize / 2;
const MAX_LAG_SAMPLES: usize = (SAMPLE_RATE as usize * 250) / 1000;

fn sine(len: usize, freq: f32) -> Vec<f32> {
    (0..len)
        .map(|i| {
            let t = i as f32 / SAMPLE_RATE as f32;
            0.5 * (2.0 * std::f32::consts::PI * freq * t).sin()
        })
        .collect()
}

fn encode_sine(packets: usize) -> (Vec<Vec<u8>>, Vec<f32>) {
    let total = WARMUP_SAMPLES + FRAME_SIZE * packets;
    let input = sine(total, FREQ_HZ);

    let (mut producer, consumer) = ring_buffer(total);
    let mut encoder = AudioEncoder::new(consumer).unwrap();

    let written = producer.write(&input);
    assert_eq!(written, total);

    let encoded = encoder.process().unwrap();
    assert_eq!(encoded.len(), packets);

    (encoded, input[WARMUP_SAMPLES..].to_vec())
}

fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

fn normalized_correlation(a: &[f32], b: &[f32]) -> f32 {
    let mean_a = a.iter().sum::<f32>() / a.len() as f32;
    let mean_b = b.iter().sum::<f32>() / b.len() as f32;

    let mut dot = 0.0f64;
    let mut norm_a = 0.0f64;
    let mut norm_b = 0.0f64;

    for (x, y) in a.iter().zip(b) {
        let x = (*x - mean_a) as f64;
        let y = (*y - mean_b) as f64;
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    (dot / (norm_a.sqrt() * norm_b.sqrt())) as f32
}

fn best_correlation(output: &[f32], reference: &[f32]) -> f32 {
    let out = &output[SKIP_SAMPLES..SKIP_SAMPLES + WINDOW_SAMPLES];
    let mut best = -1.0f32;

    for lag in 0..=MAX_LAG_SAMPLES {
        let start = SKIP_SAMPLES - lag;
        let expected = &reference[start..start + WINDOW_SAMPLES];
        best = best.max(normalized_correlation(out, expected));
    }

    best
}

#[test]
fn neteq_decodes_1khz_sine_with_correlation() {
    let (packets, reference) = encode_sine(PACKET_COUNT);

    let mut engine = JitterEngine::new();
    let mut output = Vec::with_capacity(PACKET_COUNT * 2 * OUT_FRAME_SAMPLES);
    let mut frame = [0.0f32; OUT_FRAME_SAMPLES];

    for (i, payload) in packets.into_iter().enumerate() {
        let sequence_number = i as u16;
        let timestamp = (i * FRAME_SIZE) as u32;
        let packet = make_audio_packet(
            sequence_number,
            timestamp,
            1,
            OPUS_PAYLOAD_TYPE,
            i == 0,
            payload,
        );

        engine.insert(1, packet);
        engine.mix_frame(&mut frame);
        output.extend_from_slice(&frame);
        engine.mix_frame(&mut frame);
        output.extend_from_slice(&frame);
    }

    let correlation = best_correlation(&output, &reference);
    let output_rms = rms(&output[SKIP_SAMPLES..SKIP_SAMPLES + WINDOW_SAMPLES]);

    assert!(
        output_rms > 0.01,
        "NetEq output is silent (rms {output_rms})"
    );
    assert!(
        correlation > 0.7,
        "NetEq output does not correlate with the 1 kHz input (correlation {correlation})"
    );
}
