use thiserror::Error;

#[derive(Error, Debug)]
pub enum AudioError {
    #[error("no input device available")]
    NoInputDevice,

    #[error("no output device available")]
    NoOutputDevice,

    #[error("audio stream error")]
    Stream(#[from] cpal::Error),

    #[error("opus codec error")]
    Opus(#[from] opus::Error),
}
