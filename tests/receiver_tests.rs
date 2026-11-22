use hydrargyum::modules::{
    audio_dsp::jitter_buffer::AudioReceiver, audio_io::ring_buff::ring_buffer,
    audio_types::FRAME_SIZE,
};

#[test]
fn spawn_starts_and_stops_without_hardware() {
    let (producer, _consumer) = ring_buffer(FRAME_SIZE * 4);

    let receiver = AudioReceiver::spawn(producer).expect("receiver should spawn");
    receiver.handle().leave(1);

    drop(receiver);
}
