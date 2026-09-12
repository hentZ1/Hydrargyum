use thiserror::Error;

#[derive(Error, Debug)]
pub enum AudioError {
    #[error("no input device available")]
    NoInputDevice,

    #[error("error while querying configs")]
    SupportedStreamConfigsError(#[from] cpal::Error),

    #[error("no supported config")]
    NoSupportedInputConfig,

    //output errors
    #[error("no output device available")]
    NoOutputDevice,

    #[error("no supported output config")]
    NoSupportedOutputConfig,
}
