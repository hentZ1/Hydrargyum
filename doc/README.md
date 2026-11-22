# Hydrargyum — guia de código e operação

Este diretório documenta a parte de áudio local do Hydrargyum: captura, ring buffers, encoder Opus, recepção com NetEq e reprodução. O objetivo é servir como guia de leitura do código e de operação do projeto.

> **Estado atual:** a parte de áudio local está implementada. `webrtc_transport/`, `signaling/` e `call_session/` ainda não existem. `src/main.rs` está vazio; neste momento o projeto é usado como biblioteca e pelos testes.

## Índice

1. [Arquitetura e fluxo de dados](arquitetura.md)
2. [Constantes de áudio (`audio_types`)](audio_types.md)
3. [Entrada e saída de áudio (`audio_io`)](audio_io.md)
4. [Encoder Opus (`audio_dsp/codec.rs`)](codec.md)
5. [Recepção com NetEq (`audio_dsp/jitter_buffer.rs`)](jitter_buffer.md)
6. [Testes](testes.md)
7. [Operação, ajustes e solução de problemas](operacao.md)
8. [Termos técnicos](termos_tecnicos.md)

## Começo rápido

```bash
# Compilar
cargo build

# Rodar toda a suíte de lógica pura
cargo test

# Formatação
cargo fmt --check

# Teste manual de microfone (precisa de placa de som e interação)
cargo test --test hardware_tests -- --ignored --nocapture
```

O teste de hardware é interativo: ele pede silêncio e depois voz. Ele não roda no CI.

## Visão geral em uma tela

```
Envio:
  microfone
    -> callback de entrada do cpal
    -> AudioProducer (ring buffer de captura)
    -> AudioConsumer
    -> AudioEncoder (Opus, frames de 20 ms)
    -> Vec<Vec<u8>> (pacotes Opus)

Recepção:
  pacote Opus + metadados RTP
    -> make_audio_packet
    -> AudioReceiverHandle::push_packet
    -> canal de comandos
    -> thread "audio-receiver"
    -> JitterEngine
    -> um NetEq por participante
    -> NetEqOpusDecoder (Opus -> f32)
    -> AudioProducer (ring buffer de reprodução)
    -> callback de saída do cpal
    -> alto-falante
```

## Invariantes do projeto

| Invariante | Valor | Onde está definido |
|---|---|---|
| Taxa de amostragem | 48.000 Hz | `audio_types::SAMPLE_RATE` |
| Canais | mono (1) | `AUDIO_CONFIG` e `JitterEngine` |
| Formato das amostras | `f32` | todo o pipeline |
| Frame de entrada do Opus | 20 ms = 960 samples | `audio_types::FRAME_MS` e `FRAME_SIZE` |
| Payload type do Opus | 111 | `audio_types::OPUS_PAYLOAD_TYPE` |
| Frame de saída do NetEq | 10 ms = 480 samples | `jitter_buffer::OUT_FRAME_SAMPLES` |
| Política do ring buffer | descartar o dado novo quando cheio | `ring_buff.rs` |
| Underrun de saída | preencher o restante com zero | `stream::fill_underrun` |

Regras que não devem ser quebradas:

- Callbacks de dados do `cpal` não alocam, não travam mutex, não fazem I/O e não usam `println!`.
- O ring buffer tem tamanho fixo; não redimensionar durante a operação.
- O caminho de recepção é exclusivamente NetEq; não existe mais um `AudioDecoder` separado.
- O host (SFU) encaminha payload cifrado sem decodificar; a decodificação é do cliente.
- Constantes de tempo são derivadas da taxa, não números soltos.

## Ordem de leitura sugerida

1. `audio_types` — entenda as unidades e os valores compartilhados.
2. `audio_io` — entenda como o áudio entra e sai do sistema.
3. `codec` — entenda como o áudio capturado vira pacotes Opus.
4. `jitter_buffer` — entenda como os pacotes recebidos viram áudio reproduzível.
5. `testes` — veja o que já é garantido e como rodar cada verificação.
6. `operacao` — use os exemplos de montagem, ajuste e diagnóstico.

## O que este guia não cobre

- Transporte WebRTC, ICE/DTLS/SRTP, signaling e cifragem ponta a ponta: ainda não implementados.
- Seleção de dispositivo de áudio: o código usa sempre o dispositivo padrão do sistema.
- Interface gráfica/Tauri: ainda não faz parte deste repositório.
- Mixagem no host: o host é um SFU; ele encaminha, não mixa.

## Manutenção

Quando o código mudar, atualize o arquivo correspondente e o “Registro de progresso” em `HYDRARGYUM_AUDIO_AGENT.md`. Se uma constante mudar, procure todos os usos com:

```bash
rg -n "SAMPLE_RATE|FRAME_MS|FRAME_SIZE|OPUS_PAYLOAD_TYPE" src tests
```
