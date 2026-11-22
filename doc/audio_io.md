# Entrada e saída de áudio (`audio_io`)

O módulo `audio_io` é a fronteira com o hardware. Ele contém a configuração fixa dos streams, o ring buffer que liga as threads e os builders de entrada e saída.

## Arquivos

| Arquivo | Papel |
|---|---|
| `mod.rs` | `AUDIO_CONFIG`: 48 kHz, mono, buffer padrão |
| `ring_buff.rs` | `AudioProducer`, `AudioConsumer` e `ring_buffer` |
| `stream.rs` | `input_stream_builder`, `output_stream_builder` e `fill_underrun` |
| `error.rs` | `AudioError` |

## Configuração (`mod.rs`)

```rust
pub const AUDIO_CONFIG: cpal::StreamConfig = cpal::StreamConfig {
    channels: 1,
    sample_rate: SAMPLE_RATE,
    buffer_size: cpal::BufferSize::Default,
};
```

- `channels: 1`: o projeto é mono de ponta a ponta.
- `sample_rate: SAMPLE_RATE`: 48 kHz, vindo de `audio_types`.
- `buffer_size: BufferSize::Default`: o `cpal` decide o tamanho interno do bloco de callback.
- A mesma configuração é usada na entrada e na saída.
- Não há seleção de dispositivo: os builders usam `default_input_device()` e `default_output_device()`.

Se o dispositivo padrão não suportar essa configuração, `build_input_stream`/`build_output_stream` retornam um `cpal::Error`, convertido para `AudioError::Stream`.

## Ring buffer (`ring_buff.rs`)

### API

```rust
pub fn ring_buffer(buffer_size: usize) -> (AudioProducer, AudioConsumer);

impl AudioProducer {
    pub fn write(&mut self, samples: &[f32]) -> usize;
}

impl AudioConsumer {
    pub fn read(&mut self, destination: &mut [f32]) -> usize;
}
```

### Como funciona

`ring_buffer(n)` cria um `HeapRb::<f32>` com capacidade `n` e o divide em duas metades:

- `AudioProducer`: lado de escrita, usado pelo callback de entrada ou pelo `AudioReceiver`.
- `AudioConsumer`: lado de leitura, usado pelo `AudioEncoder` ou pelo callback de saída.

As metades usam `Caching<Arc<SharedRb<Heap<f32>>>, ...>`, ou seja, compartilham o mesmo buffer através de um `Arc` e mantêm índices locais de cache para reduzir sincronização.

É um ring **SPSC**: single-producer, single-consumer. Cada metade deve pertencer a uma única thread. Não clone `AudioProducer` nem `AudioConsumer`.

### Semântica de escrita

`write` chama `push_slice` e retorna quantas amostras foram aceitas:

- Se houver espaço para todas, retorna `samples.len()`.
- Se houver espaço parcial, grava a parte que cabe e retorna esse número.
- Se estiver cheio, retorna `0`.

**Política de overflow:** o dado novo é descartado; o dado antigo permanece. Não existe sobrescrita (`push_overwrite` não é usado). Isso evita que o áudio “salte” para frente quando a aplicação atrasa.

O callback de entrada ignora o valor retornado. Se o ring de captura encher, as amostras excedentes são perdidas.

### Semântica de leitura

`read` chama `pop_slice` e retorna quantas amostras foram lidas:

- Se houver menos amostras que `destination.len()`, lê o que existe e retorna esse número.
- Se não houver nada, retorna `0`.
- O restante de `destination` **não é limpo** por `read`; quem chama precisa tratar o restante.

No callback de saída, `fill_underrun` faz esse tratamento.

### Tamanho

O tamanho é definido pelo chamador e não pode ser redimensionado depois. A documentação usa 100 ms = 4800 samples como referência para captura e reprodução, mas o código não impõe esse valor.

Escolha do tamanho:

- Pequeno demais: underrun na saída ou perda de captura em picos de agendamento.
- Grande demais: aumenta a latência e o tempo para reagir a mudanças.
- O ring não corrige taxa: se produtor e consumidor tiverem taxas diferentes, ele enche ou esvazia.

### Exemplo

```rust
use hydrargyum::modules::audio_io::ring_buff::ring_buffer;

let (mut producer, mut consumer) = ring_buffer(4800);

let written = producer.write(&[0.0; 960]);
assert_eq!(written, 960);

let mut out = [0.0; 480];
let read = consumer.read(&mut out);
assert_eq!(read, 480);
```

## Streams (`stream.rs`)

### Entrada

```rust
pub fn input_stream_builder(mut producer: AudioProducer) -> Result<Stream, AudioError>
```

1. Obtém `cpal::default_host()`.
2. Pega `default_input_device()`; se não existir, retorna `AudioError::NoInputDevice`.
3. Cria um stream de entrada com `AUDIO_CONFIG`.
4. No callback de dados, chama `producer.write(data)`.
5. O callback de erro usa `eprintln!`.
6. Retorna o `Stream`; ele ainda não está rodando.

Para iniciar:

```rust
use cpal::traits::StreamTrait;

let stream = input_stream_builder(producer)?;
stream.play()?;
```

O `Stream` precisa continuar vivo enquanto a captura existir. Se ele for dropado, a captura para.

### Saída

```rust
pub fn output_stream_builder(mut consumer: AudioConsumer) -> Result<Stream, AudioError>
```

1. Obtém o host e o dispositivo de saída padrão.
2. Cria um stream de saída com `AUDIO_CONFIG`.
3. No callback de dados, chama `consumer.read(data)`.
4. `read` retorna quantas amostras foram preenchidas.
5. `fill_underrun(data, filled)` preenche o restante com zero.
6. Retorna o `Stream`; é preciso chamar `play()`.

Sem `fill_underrun`, o restante do buffer poderia conter amostras antigas e produzir ruído. Com ele, underrun vira silêncio.

### `fill_underrun`

```rust
pub fn fill_underrun(data: &mut [f32], filled: usize) {
    let filled = filled.min(data.len());
    data[filled..].fill(0.0);
}
```

- Se `filled` for maior que `data.len()`, é limitado ao tamanho.
- Se `filled == data.len()`, nada muda.
- Se `filled == 0`, todo o buffer vira zero.
- Se `filled` for parcial, só a cauda vira zero.

É uma função pura, testada sem hardware em `tests/stream_tests.rs`.

### Regras dos callbacks

Os callbacks de dados rodam em threads de tempo real do `cpal`. Eles devem:

- escrever/ler apenas o ring buffer;
- evitar alocação;
- evitar mutex;
- evitar I/O;
- evitar `println!`.

O código atual segue isso no callback de dados. O callback de erro usa `eprintln!`, que é um caminho de erro, não o caminho normal de áudio. Ainda assim, em produção é melhor encaminhar para um logger.

## Erros (`error.rs`)

```rust
pub enum AudioError {
    NoInputDevice,
    NoOutputDevice,
    Stream(#[from] cpal::Error),
    Opus(#[from] opus::Error),
}
```

| Variante | Quando ocorre |
|---|---|
| `NoInputDevice` | Não há dispositivo de entrada padrão |
| `NoOutputDevice` | Não há dispositivo de saída padrão |
| `Stream` | Falha ao criar/configurar o stream `cpal` |
| `Opus` | Falha ao criar ou usar o encoder/decoder Opus |

`#[from]` permite usar `?` em funções que retornam `AudioError` quando o erro é `cpal::Error` ou `opus::Error`.

## Como operar

### Captura

```rust
use hydrargyum::modules::{
    audio_dsp::codec::AudioEncoder,
    audio_io::{ring_buff::ring_buffer, stream::input_stream_builder},
};
use cpal::traits::StreamTrait;

let (capture_producer, capture_consumer) = ring_buffer(4800);
let input_stream = input_stream_builder(capture_producer)?;
let mut encoder = AudioEncoder::new(capture_consumer)?;

input_stream.play()?;
// Em loop, em uma thread normal:
let packets = encoder.process()?;
```

### Reprodução

```rust
use hydrargyum::modules::audio_io::{
    ring_buff::ring_buffer,
    stream::output_stream_builder,
};
use cpal::traits::StreamTrait;

let (playback_producer, playback_consumer) = ring_buffer(4800);
let output_stream = output_stream_builder(playback_consumer)?;
output_stream.play()?;
// O AudioReceiver deve receber `playback_producer`.
```

Mantenha os dois streams vivos e chame `play()` em ambos. Criar o stream não inicia a captura/reprodução.

## Problemas comuns

| Sintoma | Causa provável | O que fazer |
|---|---|---|
| `NoInputDevice` | Nenhum microfone padrão | Configurar um dispositivo padrão no sistema |
| `NoOutputDevice` | Nenhuma saída padrão | Configurar uma saída padrão |
| `Stream` ao criar | Dispositivo não suporta 48 kHz mono | Verificar a configuração do dispositivo; não há fallback/negociação |
| Sem captura | `stream.play()` não foi chamado ou o stream foi dropado | Manter o `Stream` vivo e chamar `play()` |
| Sem reprodução | Ring de reprodução vazio | Verificar receiver, payload type e se o output stream está tocando |
| Estalos/ruído | Underrun ou ring pequeno | Aumentar ring/alvo de preenchimento; investigar CPU |
| Áudio “some” na captura | Ring cheio; overflow descarta o novo | Aumentar ring ou consumir com mais frequência |
| Atraso alto | Ring/alvo grandes | Reduzir buffers com cuidado para não gerar underrun |
