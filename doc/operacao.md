# Operação, ajustes e solução de problemas

Este é o guia prático para compilar, testar e montar um fluxo local de áudio com o código atual. O projeto ainda não tem binário nem interface; a operação é feita por biblioteca e pelos testes.

## Pré-requisitos

- Rust com edition 2024 (toolchain recente).
- Linux, macOS ou Windows com áudio funcional.
- No Linux, o `cpal` usa o host padrão disponível (por exemplo, PipeWire/ALSA). O dispositivo padrão precisa suportar 48 kHz mono.
- Microfone para o teste de hardware.
- Nenhum servidor ou rede é necessário para a parte local.

Dependências principais (veja `Cargo.toml`):

| Crate | Versão | Papel |
|---|---|---|
| `cpal` | 0.18.2 | Entrada/saída de hardware |
| `opus` | 0.4.0 | Codec de voz |
| `neteq` | 0.9.1, sem feature `native` | Jitter buffer |
| `ringbuf` | 0.5.1 | Ring buffers SPSC |
| `thiserror` | 2.0.20 | Erros tipados |
| `log` | 0.4.34 | Logging |
| `webrtc` | 0.21.0 | Transporte futuro; ainda não usado |
| `anyhow` | 1.0.104 | Apenas dev-dependency do teste de hardware |

## Compilar e testar

```bash
cargo build
cargo test
cargo fmt --check
```

O `cargo test` padrão roda 9 testes de lógica e ignora o teste de hardware. Resultado esperado:

- 3 testes de codec;
- 1 de NetEq;
- 1 de receiver;
- 4 de stream;
- 1 de hardware ignorado.

## Teste manual de microfone

```bash
cargo test --test hardware_tests -- --ignored --nocapture
```

O teste:

1. Abre o microfone padrão em 48 kHz mono.
2. Pede silêncio por 1,5 s.
3. Pede fala contínua por 3 s.
4. Verifica se a contagem de amostras está próxima do esperado.
5. Verifica se a voz é pelo menos 5× mais forte que o ruído de fundo.

Se falhar:

- confirme que o microfone é o dispositivo padrão;
- verifique permissões de microfone no sistema;
- reduza ruído de fundo;
- confirme que o dispositivo suporta 48 kHz mono;
- rode com `--nocapture` para ver as medições.

## Não existe binário ainda

`src/main.rs` está vazio. `cargo run` não inicia uma chamada nem uma interface. Para operar o áudio, você precisa:

- usar a biblioteca em um binário próprio; ou
- criar um exemplo/binário que monte os streams e o pipeline.

O restante desta página mostra como montar esse fluxo.

## Montando um fluxo local

### 1. Criar os ring buffers

```rust
use hydrargyum::modules::audio_io::ring_buff::ring_buffer;

// 4800 samples = 100 ms a 48 kHz.
let (capture_producer, capture_consumer) = ring_buffer(4800);
let (playback_producer, playback_consumer) = ring_buffer(4800);
```

Escolha o tamanho conforme a latência desejada. Rings maiores toleram mais atraso, mas aumentam a latência.

### 2. Criar os streams

```rust
use cpal::traits::StreamTrait;
use hydrargyum::modules::audio_io::stream::{
    input_stream_builder,
    output_stream_builder,
};

let input_stream = input_stream_builder(capture_producer)?;
let output_stream = output_stream_builder(playback_consumer)?;

input_stream.play()?;
output_stream.play()?;
```

Mantenha `input_stream` e `output_stream` vivos. Criar não inicia; `play()` inicia.

### 3. Criar o encoder

```rust
use hydrargyum::modules::audio_dsp::codec::AudioEncoder;

let mut encoder = AudioEncoder::new(capture_consumer)?;
```

O encoder descarta o primeiro segundo de captura (warmup).

### 4. Criar o receiver

```rust
use hydrargyum::modules::audio_dsp::jitter_buffer::AudioReceiver;

let receiver = AudioReceiver::spawn(playback_producer)?;
let handle = receiver.handle();
```

Mantenha `receiver` vivo. O `handle` pode ser clonado e enviado para a thread de rede.

### 5. Loop de envio

Em uma thread normal (não no callback do `cpal`):

```rust
loop {
    let packets = encoder.process()?;
    for packet in packets {
        // Futuro: cifrar, montar RTP e enviar.
        let _ = packet;
    }
    std::thread::sleep(std::time::Duration::from_millis(20));
}
```

`process()` devolve zero ou mais pacotes. Se a thread atrasar, ele pode devolver vários de uma vez.

### 6. Loop de recepção

Quando chegar um payload Opus da rede:

```rust
use hydrargyum::modules::{
    audio_dsp::jitter_buffer::make_audio_packet,
    audio_types::OPUS_PAYLOAD_TYPE,
};

let packet = make_audio_packet(
    sequence_number,
    timestamp,
    ssrc,
    OPUS_PAYLOAD_TYPE,
    marker,
    opus_payload,
);

if !handle.push_packet(participant_id, packet) {
    // Fila cheia ou receiver derrubado; o pacote foi descartado.
}
```

Quando o participante sair:

```rust
handle.leave(participant_id);
```

### 7. Encerrar

- Dropa `receiver` para parar a thread e fazer `join`.
- Dropa os streams para parar captura/reprodução.
- Dropa o encoder.

## Ajustes e o que cada um afeta

### Constantes de protocolo (`audio_types`)

| Constante | Onde mexer | Efeito |
|---|---|---|
| `SAMPLE_RATE` | `audio_types/mod.rs` | Muda taxa de Opus, NetEq, `cpal` e timestamps; mudança grande |
| `FRAME_MS` | `audio_types/mod.rs` | Muda duração do pacote, `FRAME_SIZE` e taxa de pacotes |
| `FRAME_SIZE` | derivado | Não editar diretamente; é calculado |
| `OPUS_PAYLOAD_TYPE` | `audio_types/mod.rs` | Precisa bater com o transporte/SDP |

### `audio_io`

| Item | Onde | Efeito |
|---|---|---|
| `channels` | `AUDIO_CONFIG` | Hoje 1; mudar exige suporte em Opus/NetEq/mixagem |
| `buffer_size` | `AUDIO_CONFIG` | `BufferSize::Default` deixa o `cpal` escolher |
| Tamanho do ring | chamador de `ring_buffer` | Latência e tolerância a atraso |
| `fill_underrun` | `stream.rs` | Preenche underrun com zero; não aumentar o buffer |

### `codec`

| Item | Onde | Efeito |
|---|---|---|
| `WARMUP_SAMPLES` | `codec.rs` | Quanto áudio inicial é descartado; hoje 1 s |
| `READ_CHUNK` | `codec.rs` | Tamanho da leitura do ring; hoje 1024 |
| `MAX_PACKET` | `codec.rs` | Buffer de saída do Opus; hoje 4000 bytes |
| `Application::Voip` | `codec.rs` | Modo de voz do Opus |

### `jitter_buffer`

| Item | Onde | Efeito |
|---|---|---|
| `TARGET_FILL_MS` | `jitter_buffer.rs` | Alvo do ring de reprodução; mais = mais latência/tolerância |
| `MAX_DELAY_MS` | `jitter_buffer.rs` | Atraso máximo do NetEq; mais = mais tolerância a jitter |
| `COMMAND_QUEUE_CAPACITY` | `jitter_buffer.rs` | Rajadas de pacotes antes de descartar |
| `IDLE_SLEEP` | `jitter_buffer.rs` | CPU vs. latência de polling |
| `MAX_OPUS_FRAME_MS` | `jitter_buffer.rs` | Maior frame que o decoder aceita |
| `OUT_FRAME_SAMPLES` | derivado de `SAMPLE_RATE` | Frame de saída de 10 ms |

## Solução de problemas

### `NoInputDevice`

Não há dispositivo de entrada padrão. Verifique o sistema:

```bash
# Linux: liste dispositivos de áudio (exemplo com PipeWire/PulseAudio)
pactl list short sources
```

Depois configure o padrão no sistema. O código não escolhe dispositivo.

### `NoOutputDevice`

Mesma ideia, para saída:

```bash
pactl list short sinks
```

### `AudioError::Stream`

O dispositivo existe, mas não aceitou a configuração fixa (48 kHz mono) ou o stream falhou. Verifique se o dispositivo suporta 48 kHz mono. Não há fallback automático.

### Não sai áudio

Checklist:

1. O `input_stream.play()` e o `output_stream.play()` foram chamados?
2. Os streams continuam vivos?
3. O `AudioReceiver` continua vivo?
4. O `AudioEncoder` já passou do warmup de 1 segundo?
5. O payload type é 111?
6. O timestamp avança 960 por pacote de 20 ms?
7. O sequence number está coerente?
8. O `AudioReceiverHandle::push_packet` está retornando `true`?
9. O ring de reprodução está recebendo frames? (O receiver só escreve se houver participantes e espaço.)
10. O decoder Opus recebeu payload válido?

### Estalos, cortes ou silêncios

- Underrun na saída: o ring de reprodução esvaziou. Aumente o ring ou `TARGET_FILL_MS`, ou reduza a carga da CPU.
- Ring de captura cheio: o encoder não está consumindo rápido o suficiente. Aumente o ring ou chame `process()` com mais frequência.
- Fila de comandos cheia: `push_packet` retorna `false`. A rede pode estar entregando rápido demais ou a thread do receiver está travada.
- `IDLE_SLEEP` de 2 ms adiciona latência de polling; diminuir pode ajudar, mas aumenta CPU.

### Avisos do NetEq não aparecem

O projeto usa `log`, mas não inicializa logger. Sem um logger, `log::warn!` é descartado. Em um binário, adicione um logger, por exemplo `env_logger` ou `tracing-subscriber`, e configure o nível. Isso não é dependência atual do projeto.

### Teste de hardware falha

- O microfone padrão está correto?
- Permissão de microfone concedida?
- O dispositivo suporta 48 kHz mono?
- O ambiente estava silencioso na fase de silêncio?
- Você falou durante a fase de voz?
- A contagem de amostras ficou fora de 90%–110%? Pode ser problema de driver/relógio.

### `push_packet` sempre `false`

- O `AudioReceiver` foi dropado? O canal fica desconectado.
- A fila de 64 comandos encheu? A thread do receiver pode estar travada ou lenta.
- Verifique se `receiver` está vivo e se a thread não entrou em pânico.

## Logging

O código usa `log::warn!` em `JitterEngine` para:

- falha ao criar NetEq;
- pacote rejeitado;
- falha em `get_audio`.

Para ver esses avisos, o binário precisa inicializar um logger. Exemplo conceitual:

```rust
// Requer adicionar env_logger ao Cargo.toml.
env_logger::init();
```

O projeto atual não inclui `env_logger`; isso é uma decisão para o binário/aplicação.

## Regras de tempo real

Nos callbacks de dados do `cpal`:

- não alocar;
- não bloquear em mutex;
- não fazer I/O;
- não usar `println!`;
- apenas ler/escrever no ring buffer.

O encoder, o NetEq e o receiver rodam fora dos callbacks de dados. Eles podem alocar, mas não devem bloquear a thread de áudio.

## Checklist de integração

Antes de considerar o fluxo local pronto:

- [ ] `cargo build` passa.
- [ ] `cargo test` passa.
- [ ] `cargo test --test hardware_tests -- --ignored --nocapture` passa com microfone.
- [ ] Captura e reprodução usam rings de tamanho fixo.
- [ ] O encoder não roda no callback do `cpal`.
- [ ] O `AudioReceiver` fica vivo durante a chamada.
- [ ] Payload type, timestamp e sequence number estão coerentes.
- [ ] Underrun de saída é silêncio, não lixo.
- [ ] Logs estão configurados no binário, se você quiser ver avisos.

## Próximos passos

A parte local está fechada. O próximo bloco é `webrtc_transport/`:

- RTP/ICE/DTLS/SRTP;
- offer/answer e candidatos ICE via `signaling/`;
- tracks de áudio;
- SFU no host;
- cifragem ponta a ponta.

O transporte deve receber/entregar bytes Opus e não deve conhecer `cpal` nem ring buffers. Veja [Arquitetura](arquitetura.md) e a seção 8 de `HYDRARGYUM_AUDIO_AGENT.md`.
