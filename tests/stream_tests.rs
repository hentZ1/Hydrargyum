use hydrargyum::modules::audio_io::stream::fill_underrun;

#[test]
fn partial_read_zeroes_the_tail() {
    let mut data = [9.0f32; 8];

    fill_underrun(&mut data, 3);

    assert_eq!(&data[..3], &[9.0; 3]);
    assert_eq!(&data[3..], &[0.0; 5]);
}

#[test]
fn full_read_leaves_the_buffer_unchanged() {
    let mut data = [9.0f32; 8];
    let len = data.len();

    fill_underrun(&mut data, len);

    assert_eq!(data, [9.0; 8]);
}

#[test]
fn empty_read_silences_everything() {
    let mut data = [9.0f32; 8];

    fill_underrun(&mut data, 0);

    assert_eq!(data, [0.0; 8]);
}

#[test]
fn filled_beyond_len_is_clamped() {
    let mut data = [9.0f32; 8];

    fill_underrun(&mut data, 99);

    assert_eq!(data, [9.0; 8]);
}
