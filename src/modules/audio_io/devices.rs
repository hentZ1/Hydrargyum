// o arquivo devices serve pra pegar os dispositivos disponiveis no sistema do usuario-
// e já deixar pronto a configuração para o uso da criação das streams de audio

// # OPCIONAL
// let host = cpal::default_host(); a logica disso aqui é dup mais de uma vez atraves dos modulos de
// audio se alguem quiser fazer uma função auxiliar para buildar o host é viavel, mas eu neste
// momento não penso como isso poderia deixar a duplicação menor inves de deixar apenas mais bonito

// o arquivo error só serve para criar erros personalizados para melhor tratamento de erro-
use crate::modules::audio_io::error::AudioError;
use cpal::traits::{DeviceTrait, HostTrait};

pub fn input_config() -> Result<cpal::SupportedStreamConfig, AudioError> {
    let host = cpal::default_host();

    let input_device = host
        .default_input_device()
        .ok_or(AudioError::NoInputDevice)?;

    let mut sup_config_range = input_device.supported_input_configs()?;

    let input_config = sup_config_range
        .next()
        .ok_or(AudioError::NoSupportedInputConfig)?
        .with_max_sample_rate();

    Ok(input_config)
}

pub fn output_config() -> Result<cpal::SupportedStreamConfig, AudioError> {
    let host = cpal::default_host();

    let output_device = host
        .default_output_device()
        .ok_or(AudioError::NoOutputDevice)?;

    let mut sup_config_range = output_device.supported_output_configs()?;

    let output_config = sup_config_range
        .next()
        .ok_or(AudioError::NoSupportedOutputConfig)?
        .with_max_sample_rate();

    Ok(output_config)
}
