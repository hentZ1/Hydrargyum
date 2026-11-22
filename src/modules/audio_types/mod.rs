pub const SAMPLE_RATE: u32 = 48_000;
pub const FRAME_MS: u32 = 20;
pub const FRAME_SIZE: usize = (SAMPLE_RATE as usize * FRAME_MS as usize) / 1000;
pub const OPUS_PAYLOAD_TYPE: u8 = 111;
