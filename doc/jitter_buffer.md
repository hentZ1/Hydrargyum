# Recepção com NetEq (`audio_dsp/jitter_buffer.rs`)

Este arquivo é o coração da recepção. Ele mantém um NetEq por participante, decodifica Opus para `f32`, mistura os participantes e entrega frames de 10 ms ao ring de reprodução.

## Visão geral

```
AudioReceiverHandle::push_packet
    -> canal de comandos (capacidade 64)
    -> thread "audio-receiver"
    -> JitterEngine
         -> NetEq do participante A -> NetEqOpusDecoder
         -> NetEq do participante B -> NetEqOpusDecoder
         -> ...
    -> mix_frame (soma + clamp)
    -> AudioProducer (ring de reprodução)
```

## Constantes

| Constante | Valor | Papel |
|---|---|---|
| `OUT_FRAME_SAMPLES` | 480 | 10 ms de saída a 48 kHz |
| `TARGET_FILL_MS` | 40 | Alvo de preenchimento do ring de reprodução |
| `TARGET_FILL_SAMPLES` | 1920 | `40 ms × 48 kHz` |
| `MAX_DELAY_MS` | 250 | Atraso máximo configurado no NetEq |
| `COMMAND_QUEUE_CAPACITY` | 64 | Capacidade da fila de comandos |
| `IDLE_SLEEP` | 2 ms | Sleep da thread quando não há trabalho |
| `MAX_OPUS_FRAME_MS` | 120 | Maior frame Opus que o decoder pode receber |
| `MAX_OPUS_FRAME_SAMPLES` | 5760 | `120 ms × 48 kHz` |

`OUT_FRAME_SAMPLES` é público porque os testes e a aplicação precisam saber o tamanho do frame de saída.

## `NetEqOpusDecoder`

O NetEq não decodifica Opus sozinho. Ele chama um decoder registrado por payload type. `NetEqOpusDecoder` é o adaptador entre a trait `neteq::codec::AudioDecoder` e o `opus::Decoder`.

```rust
struct NetEqOpusDecoder {
    decoder: Decoder,
    sample_rate: u32,
    channels: u8,
    pcm: Vec<f32>,
}
```

### Construção

```rust
fn new(sample_rate: u32, channels: u8) -> Result<Self, String>
```

- Aceita 1 canal (`Channels::Mono`) ou 2 (`Channels::Stereo`).
- Rejeita qualquer outro número com uma mensagem de erro.
- Cria `opus::Decoder::new(sample_rate, channels)`.
- Aloca `pcm` com `MAX_OPUS_FRAME_SAMPLES × channels` amostras. Isso cobre frames Opus de até 120 ms.

No projeto, o decoder é sempre criado com `SAMPLE_RATE` e 1 canal.

### Trait implementada

```rust
impl neteq::codec::AudioDecoder for NetEqOpusDecoder {
    fn sample_rate(&self) -> u32;
    fn channels(&self) -> u8;
    fn decode(&mut self, encoded: &[u8]) -> neteq::Result<Vec<f32>>;
}
```

`decode` faz:

```rust
let decoded = self.decoder.decode_float(encoded, &mut self.pcm, false)?;
let total = decoded * self.channels as usize;
Ok(self.pcm[..total].to_vec())
```

- `decode_float` devolve amostras **por canal**.
- `total` multiplica por canais para obter o total de amostras.
- `false` significa **sem FEC** (Forward Error Correction).
- O resultado é copiado para um `Vec` porque a trait do NetEq exige `Vec<f32>`.

### PLC e FEC

A trait `neteq::codec::AudioDecoder` não expõe PLC nem FEC. Ela só tem `sample_rate`, `channels` e `decode`. Portanto:

- O adaptador não recebe um parâmetro de “pacote perdido”.
- O NetEq decide internamente quando usar expansão/ocultação de perda.
- O adaptador chama `decode_float(..., false)` e não tenta FEC.
- Se o NetEq passar um slice vazio, o Opus interpreta como PLC (concealment) e devolve um frame de ocultação.

## `make_audio_packet`

```rust
pub fn make_audio_packet(
    sequence_number: u16,
    timestamp: u32,
    ssrc: u32,
    payload_type: u8,
    marker: bool,
    payload: Vec<u8>,
) -> AudioPacket
```

Monta:

```rust
RtpHeader::new(sequence_number, timestamp, ssrc, payload_type, marker)
AudioPacket::new(header, payload, SAMPLE_RATE, 1, FRAME_MS)
```

| Campo | Significado |
|---|---|
| `sequence_number` | Ordem dos pacotes RTP; o NetEq usa para detectar perda/reordenação |
| `timestamp` | Posição no tempo em samples; deve avançar 960 por pacote de 20 ms |
| `ssrc` | Identificador da fonte (stream) |
| `payload_type` | Deve ser `OPUS_PAYLOAD_TYPE` (111) para achar o decoder |
| `marker` | Marca o início de um talkspurt; o primeiro pacote costuma usar `true` |
| `payload` | Bytes Opus |

O `AudioPacket` já recebe `SAMPLE_RATE`, 1 canal e `FRAME_MS`. O NetEq usa esses metadados para calcular duração e jitter.

## `add_into` e `clamp_in_place`

```rust
pub fn add_into(dst: &mut [f32], src: &[f32]);
pub fn clamp_in_place(samples: &mut [f32]);
```

- `add_into` soma `src` em `dst` usando `zip`, então não entra em pânico se os tamanhos forem diferentes; soma apenas o mínimo.
- `clamp_in_place` limita cada amostra a `[-1.0, 1.0]`.

Essas funções são usadas na mixagem de vários participantes.

## `JitterEngine`

```rust
pub struct JitterEngine {
    config: NetEqConfig,
    peers: HashMap<ParticipantId, NetEq>,
}
```

- `ParticipantId = u32`.
- Cada participante tem seu próprio `NetEq`.
- O `NetEqConfig` usa `SAMPLE_RATE`, 1 canal, `max_delay_ms = 250` e os demais valores padrão.
- O `Default` chama `new()`.

### `insert(id, packet)`

1. Procura o NetEq do participante.
2. Se não existir, cria um novo com `new_peer`.
3. `new_peer` registra `NetEqOpusDecoder` no payload type 111.
4. Insere o pacote no NetEq.
5. Se a criação falhar, loga um aviso e descarta o pacote.
6. Se o NetEq rejeitar o pacote, loga um aviso.

A criação é preguiçosa: o NetEq só nasce quando chega o primeiro pacote daquele participante.

### `remove(id)`

Remove o NetEq do participante. O próximo pacote com o mesmo id cria um NetEq novo, sem histórico.

### `is_empty()` e `peer_count()`

- `is_empty()`: não há participantes.
- `peer_count()`: quantos NetEqs existem.

### `mix_frame(out)`

```rust
pub fn mix_frame(&mut self, out: &mut [f32])
```

1. Zera `out`.
2. Para cada participante, chama `neteq.get_audio()`.
3. Soma o frame retornado em `out`.
4. Se `get_audio` falhar, loga e continua.
5. Aplica `clamp` em `out`.

Detalhes importantes:

- `get_audio()` devolve 10 ms (480 samples) por chamada.
- `mix_frame` **sempre avança** cada NetEq em 10 ms, mesmo que `out` seja menor ou maior que 480. O chamador deve passar um buffer de 480 samples.
- A soma pode passar de `1.0`; o clamp evita clipping no `f32`, mas a mixagem de muitos participantes ainda pode distorcer.
- Se um participante não tiver áudio, o NetEq pode devolver expansão/silêncio; isso faz parte do comportamento do jitter buffer.

## `AudioReceiverHandle`

```rust
#[derive(Clone)]
pub struct AudioReceiverHandle {
    tx: mpsc::SyncSender<Command>,
}
```

É a interface usada pela thread de rede (ou por um teste) para falar com o receiver.

### `push_packet(id, packet) -> bool`

```rust
match self.tx.try_send(Command::Packet(id, packet)) {
    Ok(()) => true,
    Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
}
```

- **Não bloqueia.**
- Retorna `true` se o comando entrou na fila.
- Retorna `false` se a fila está cheia ou se o receiver foi derrubado.
- Quando retorna `false`, o pacote é perdido. A aplicação pode contar/descartar ou tentar de novo depois.

### `leave(id)`

```rust
let _ = self.tx.send(Command::Leave(id));
```

- Usa `send`, que **pode bloquear** se a fila estiver cheia.
- É uma operação de controle (participante saiu), não o caminho de cada pacote.
- O erro de desconexão é ignorado.

## `AudioReceiver`

```rust
pub struct AudioReceiver {
    handle: AudioReceiverHandle,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
```

### `spawn(playback)`

```rust
pub fn spawn(playback: AudioProducer) -> std::io::Result<Self>
```

- Cria um `sync_channel(COMMAND_QUEUE_CAPACITY)`.
- Cria um `AtomicBool` de parada.
- Cria a thread com nome `audio-receiver`.
- A thread recebe o `AudioProducer` de reprodução, o receptor do canal e o flag de parada.
- Retorna `AudioReceiver` ou um erro de `std::io` se a thread não puder ser criada.

### `handle()`

Devolve um clone do `AudioReceiverHandle`. O clone pode ser enviado para a thread de rede.

### `Drop`

```rust
self.stop.store(true, Ordering::Release);
if let Some(worker) = self.worker.take() {
    let _ = worker.join();
}
```

- Sinaliza parada.
- Espera a thread terminar.
- Depois do drop, o `AudioReceiverHandle` existente não consegue mais enviar (canal desconectado).

**Operação:** mantenha o `AudioReceiver` vivo durante a chamada. Se ele for dropado, a reprodução para.

## Loop da thread (`run`)

```rust
fn run(mut playback: AudioProducer, rx: mpsc::Receiver<Command>, stop: Arc<AtomicBool>) {
    let mut engine = JitterEngine::new();
    let mut frame = [0.0f32; OUT_FRAME_SAMPLES];

    while !stop.load(Ordering::Acquire) {
        drain_commands(&mut engine, &rx);

        while !engine.is_empty()
            && playback.inner.occupied_len() < TARGET_FILL_SAMPLES
            && playback.inner.vacant_len() >= OUT_FRAME_SAMPLES
        {
            engine.mix_frame(&mut frame);
            playback.inner.push_slice(&frame);
        }

        thread::sleep(IDLE_SLEEP);
    }
}
```

Passo a passo:

1. Cria um `JitterEngine` e um frame fixo de 480 samples.
2. Enquanto não houver parada:
   - drena **todos** os comandos disponíveis (`try_recv`);
   - enquanto houver participantes, o ring de reprodução estiver abaixo de 40 ms e houver espaço para 480 samples, gera um frame e escreve;
   - dorme 2 ms.
3. O `Drop` do receiver seta `stop` e faz `join`.

O loop não aloca um buffer novo por frame: `frame` é fixo. O NetEq internamente pode alocar.

## Fluxo de uso

```rust
use hydrargyum::modules::{
    audio_dsp::jitter_buffer::{AudioReceiver, make_audio_packet},
    audio_io::{ring_buff::ring_buffer, stream::output_stream_builder},
    audio_types::OPUS_PAYLOAD_TYPE,
};
use cpal::traits::StreamTrait;

let (playback_producer, playback_consumer) = ring_buffer(4800);
let output_stream = output_stream_builder(playback_consumer)?;
output_stream.play()?;

let receiver = AudioReceiver::spawn(playback_producer)?;
let handle = receiver.handle();

let packet = make_audio_packet(
    0,       // sequence_number
    0,       // timestamp
    1,       // ssrc
    OPUS_PAYLOAD_TYPE,
    true,    // marker
    opus_payload,
);

if !handle.push_packet(42, packet) {
    // Fila cheia ou receiver derrubado: pacote descartado.
}

// Quando o participante sair:
handle.leave(42);

// Mantenha `receiver` vivo enquanto a chamada existir.
```

## Requisitos dos pacotes

Para o NetEq funcionar:

- O payload type precisa ser 111 (ou o mesmo registrado).
- O timestamp deve avançar em samples (960 por pacote de 20 ms).
- O sequence number deve ser monotônico, com wrap-around correto.
- O SSRC deve identificar a fonte.
- O payload deve ser Opus válido.
- O primeiro pacote de um talkspurt deve usar `marker = true`.

Se o timestamp estiver errado, o NetEq pode considerar os pacotes muito atrasados/adiantados e descartá-los. Se o payload type estiver errado, não há decoder registrado.

## Testes

- `tests/neteq_tests.rs` codifica uma senoide de 1 kHz, passa pelo `JitterEngine` e verifica correlação e RMS. É o teste que prova que o adaptador Opus real funciona.
- `tests/receiver_tests.rs` instancia o `AudioReceiver` com um ring real e derruba, sem hardware.

## Limitações atuais

- Não há negociação de payload type; 111 é fixo.
- Não há limite explícito de participantes.
- A mixagem soma todos os participantes e faz clamp; não há ganho por participante, ducking ou VAD.
- Não há estatísticas expostas de perda/jitter.
- Não há integração com RTP real ainda; o transporte futuro deve montar os `AudioPacket`s.
- A fila de comandos é pequena (64); rajadas grandes podem descartar pacotes.
