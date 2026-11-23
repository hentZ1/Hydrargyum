use bytes::{Bytes, BytesMut};

use crate::modules::protocol::{CHAT_CHANNEL_LABEL, CONTROL_CHANNEL_LABEL, ControlMessage};

use super::TransportError;

/// Papel de um DataChannel. O MVP usa apenas `Control`; `Application` fica
/// reservado para o chat futuro, sem alterar o ciclo de vida das conexões.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelRole {
    Control,
    Application,
}

pub fn channel_role(label: &str) -> Option<ChannelRole> {
    match label {
        CONTROL_CHANNEL_LABEL => Some(ChannelRole::Control),
        CHAT_CHANNEL_LABEL => Some(ChannelRole::Application),
        _ => None,
    }
}

pub fn encode_control(message: &ControlMessage) -> Result<Bytes, TransportError> {
    Ok(Bytes::from(serde_json::to_vec(message)?))
}

pub fn decode_control(data: &[u8]) -> Result<ControlMessage, TransportError> {
    Ok(serde_json::from_slice(data)?)
}

pub fn encode_control_mut(message: &ControlMessage) -> Result<BytesMut, TransportError> {
    Ok(BytesMut::from(encode_control(message)?.as_ref()))
}
