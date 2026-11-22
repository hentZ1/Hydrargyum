use hydrargyum::modules::{
    audio_dsp::codec::AudioEncoder,
    audio_io::ring_buff::ring_buffer,
    audio_types::{FRAME_SIZE, SAMPLE_RATE},
};

const WARMUP: usize = SAMPLE_RATE as usize;

fn sine(len: usize) -> Vec<f32> {
    (0..len)
        .map(|i| {
            let t = i as f32 / SAMPLE_RATE as f32;
            0.5 * (2.0 * std::f32::consts::PI * 440.0 * t).sin()
        })
        .collect()
}

fn encode_sine(frames: usize) -> Vec<Vec<u8>> {
    let total = WARMUP + FRAME_SIZE * frames;
    let (mut producer, consumer) = ring_buffer(total);
    let mut encoder = AudioEncoder::new(consumer).unwrap();

    let written = producer.write(&sine(total));
    assert_eq!(written, total);

    encoder.process().unwrap()
}

#[test]
fn encoder_discards_warmup_and_emits_one_packet_per_frame() {
    let packets = encode_sine(10);

    assert_eq!(packets.len(), 10);
    assert!(packets.iter().all(|p| !p.is_empty() && p.len() <= 4000));
}

#[test]
fn encoder_returns_nothing_before_a_full_frame() {
    let (mut producer, consumer) = ring_buffer(WARMUP + FRAME_SIZE);
    let mut encoder = AudioEncoder::new(consumer).unwrap();

    producer.write(&sine(WARMUP + FRAME_SIZE - 1));
    assert!(encoder.process().unwrap().is_empty());

    producer.write(&sine(1));
    assert_eq!(encoder.process().unwrap().len(), 1);
}

#[test]
fn encoder_keeps_remainder_between_rounds() {
    let total = WARMUP + FRAME_SIZE + FRAME_SIZE / 2;
    let (mut producer, consumer) = ring_buffer(total + FRAME_SIZE);
    let mut encoder = AudioEncoder::new(consumer).unwrap();

    producer.write(&sine(total));
    assert_eq!(encoder.process().unwrap().len(), 1);

    producer.write(&sine(FRAME_SIZE / 2));
    assert_eq!(encoder.process().unwrap().len(), 1);
}
