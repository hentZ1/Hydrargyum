# Constantes de áudio (`audio_types`)

O módulo `src/modules/audio_types/mod.rs` é a fonte única dos valores compartilhados entre captura, codec, NetEq e transporte futuro. Ele não depende de `cpal`, Opus ou rede.

## Conteúdo

```rust
pub const SAMPLE_RATE: u32 = 48_000;
pub const FRAME_MS: u32 = 20;
pub const FRAME_SIZE: usize = (SAMPLE_RATE as usize * FRAME_MS as usize) / 1000;
pub const OPUS_PAYLOAD_TYPE: u8 = 111;
```

| Constante | Valor | Significado |
|---|---|---|
| `SAMPLE_RATE` | 48.000 | Amostras por segundo, por canal. É a taxa do Opus, do NetEq e do `cpal` |
| `FRAME_MS` | 20 | Duração de um frame Opus em milissegundos |
| `FRAME_SIZE` | 960 | Número de amostras de um frame: `48.000 × 20 / 1.000` |
| `OPUS_PAYLOAD_TYPE` | 111 | Identificador do payload Opus no RTP |

## Por que centralizar

Antes, `SAMPLE_RATE` e `FRAME_SIZE` ficavam em `codec.rs`, o payload type ficava em `jitter_buffer.rs` e o `cpal` usava `cpal::SAMPLE_RATE_48K`. Isso permitia que os valores divergissem sem o compilador reclamar.

Agora:

- `audio_io::AUDIO_CONFIG` usa `SAMPLE_RATE`.
- `AudioEncoder` usa `SAMPLE_RATE` e `FRAME_SIZE`.
- `JitterEngine` usa `SAMPLE_RATE`, `FRAME_MS` e `OPUS_PAYLOAD_TYPE`.
- Os testes importam os mesmos valores.

Se um valor mudar, todos os consumidores enxergam a mudança.

## Onde cada constante é usada

### `SAMPLE_RATE`

- `audio_io::AUDIO_CONFIG.sample_rate`.
- `AudioEncoder::new` (Opus).
- `NetEqConfig.sample_rate`.
- `NetEqOpusDecoder::new`.
- `AudioPacket::new` (via `make_audio_packet`).
- `OUT_FRAME_SAMPLES` e `TARGET_FILL_SAMPLES`.
- Testes de codec e NetEq.

### `FRAME_MS`

- `AudioPacket::new` em `make_audio_packet`, como duração do pacote.
- Cálculo de `FRAME_SIZE`.

### `FRAME_SIZE`

- Quantidade de amostras que o `AudioEncoder` entrega ao Opus por chamada de `encode_float`.
- Quantidade de amostras que o encoder remove do acumulador após cada pacote.
- Tamanho dos buffers de teste e do ring de reprodução em `receiver_tests`.

### `OPUS_PAYLOAD_TYPE`

- Registro do decoder no NetEq: `neteq.register_decoder(OPUS_PAYLOAD_TYPE, ...)`.
- Teste de NetEq, que monta pacotes com esse payload type.

O payload type precisa bater com o que o transporte/SDP usar. Se o outro lado enviar outro número, o NetEq não encontra decoder e o áudio não sai. Ainda não há negociação dinâmica de payload type.

## Relação com `AUDIO_CONFIG`

`src/modules/audio_io/mod.rs` define:

```rust
pub const AUDIO_CONFIG: cpal::StreamConfig = cpal::StreamConfig {
    channels: 1,
    sample_rate: SAMPLE_RATE,
    buffer_size: cpal::BufferSize::Default,
};
```

Observações:

- `channels: 1` é um valor separado, não uma constante de `audio_types`. O pipeline inteiro assume mono. Se o projeto passar a suportar estéreo, isso precisa ser decidido e propagado para Opus, NetEq, mixagem e ring buffers.
- `buffer_size: BufferSize::Default` deixa o `cpal` escolher o tamanho interno. O tamanho do ring buffer do projeto é independente disso.
- A configuração é fixa: não há negociação com o dispositivo. Se o dispositivo padrão não suportar 48 kHz mono, a criação do stream falha com `AudioError::Stream`.

## O que acontece se eu mudar cada valor

### Mudar `SAMPLE_RATE`

Não é uma mudança local. Opus, NetEq, `cpal`, os buffers e os testes assumem 48 kHz. Além disso, o payload Opus tem clock rate de 48 kHz no RTP. Mudar a taxa exigiria:

- confirmar suporte do dispositivo;
- reconfigurar encoder/decoder;
- ajustar `FRAME_SIZE` e `OUT_FRAME_SAMPLES`;
- ajustar timestamps RTP;
- revisar testes e latência.

Para o estado atual, trate 48 kHz como parte do contrato.

### Mudar `FRAME_MS`

O Opus aceita frames de 2,5, 5, 10, 20, 40 e 60 ms (com restrições). O projeto usa 20 ms. Mudar `FRAME_MS` muda `FRAME_SIZE`, o número de pacotes por segundo, os timestamps e o comportamento do NetEq. Só faça isso com uma decisão explícita e testes de latência.

### Mudar `OPUS_PAYLOAD_TYPE`

É uma mudança de protocolo. O emissor, o receptor e o SDP precisam usar o mesmo número. O valor 111 é apenas o padrão atual do projeto; não há negociação.

## Como verificar duplicatas

```bash
rg -n "48_000|48000|FRAME_MS|FRAME_SIZE|OPUS_PAYLOAD_TYPE|SAMPLE_RATE" src tests
```

Os únicos lugares que devem conter os valores literais são `audio_types` e os testes que verificam o contrato. O resto deve importar as constantes.

## Testes relacionados

- `tests/codec_tests.rs` usa `SAMPLE_RATE` e `FRAME_SIZE` para montar as senoides e verificar quantos pacotes saem.
- `tests/neteq_tests.rs` usa `SAMPLE_RATE`, `FRAME_SIZE` e `OPUS_PAYLOAD_TYPE` para montar os pacotes e comparar a saída.
- `tests/receiver_tests.rs` usa `FRAME_SIZE` para dimensionar o ring de reprodução.
