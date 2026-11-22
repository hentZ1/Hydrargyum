# Encoder Opus (`audio_dsp/codec.rs`)

O `codec.rs` contém o `AudioEncoder`, responsável por transformar amostras `f32` capturadas em pacotes Opus de 20 ms. Ele não lê hardware e não envia pela rede: recebe um `AudioConsumer` e devolve `Vec<Vec<u8>>`.

## Visão geral

```rust
pub struct AudioEncoder {
    encoder: Encoder,
    consumer: AudioConsumer,
    accumulator: Vec<f32>,
    scratch: Vec<f32>,
    out: Vec<u8>,
    warmup: usize,
}
```

| Campo | Papel |
|---|---|
| `encoder` | Encoder Opus (`opus::Encoder`) |
| `consumer` | Lado de leitura do ring de captura |
| `accumulator` | Amostras que ainda não formaram um frame completo |
| `scratch` | Buffer temporário para drenar o ring em blocos de 1024 |
| `out` | Buffer de saída do Opus, com 4000 bytes |
| `warmup` | Quantas amostras ainda faltam para terminar o descarte inicial |

Constantes internas:

| Constante | Valor | Papel |
|---|---|---|
| `MAX_PACKET` | 4000 bytes | Tamanho máximo do buffer de saída do Opus |
| `WARMUP_SAMPLES` | `SAMPLE_RATE` = 48.000 | Primeiro segundo descartado |
| `READ_CHUNK` | 1024 | Quantas amostras são lidas do ring por vez |

## Construção

```rust
pub fn new(consumer: AudioConsumer) -> Result<Self, AudioError>
```

- Cria `opus::Encoder::new(SAMPLE_RATE, Channels::Mono, Application::Voip)`.
- `Application::Voip` é o modo do Opus otimizado para voz: prioriza inteligibilidade e baixa latência em vez de fidelidade musical.
- Inicializa o acumulador com capacidade para três frames (2880 amostras).
- `scratch` tem 1024 amostras.
- `out` tem 4000 bytes.
- `warmup` começa em 48.000 amostras.

Se o Opus não puder ser inicializado, retorna `AudioError::Opus`.

## `process()`

```rust
pub fn process(&mut self) -> Result<Vec<Vec<u8>>, AudioError>
```

O algoritmo tem duas fases.

### Fase 1: drenar o ring e descartar o warmup

```rust
loop {
    let n = self.consumer.read(&mut self.scratch);
    if n == 0 {
        break;
    }

    let skip = self.warmup.min(n);
    self.warmup -= skip;
    self.accumulator.extend_from_slice(&self.scratch[skip..n]);
}
```

- Lê o ring em blocos de até 1024 amostras.
- Enquanto `warmup > 0`, descarta as primeiras amostras.
- O warmup é descontado mesmo que um bloco seja parcial.
- Depois do warmup, as amostras vão para o acumulador.
- O loop termina quando o ring está vazio naquele instante.

O warmup de 1 segundo existe para evitar que o transiente inicial do codec/stream seja enviado como áudio. **Consequência operacional:** ao iniciar a captura, o primeiro segundo é perdido de propósito. Se você testar e não ouvir nada no primeiro segundo, isso é esperado.

### Fase 2: codificar frames completos

```rust
while self.accumulator.len() >= FRAME_SIZE {
    let len = self.encoder.encode_float(
        &self.accumulator[..FRAME_SIZE],
        &mut self.out,
    )?;
    packets.push(self.out[..len].to_vec());
    self.accumulator.drain(..FRAME_SIZE);
}
```

- Enquanto houver pelo menos 960 amostras, codifica exatamente 960.
- `encode_float` retorna quantos bytes escreveu em `out`.
- Copia os bytes para um `Vec<u8>` novo.
- Remove as 960 amostras do acumulador.
- Repete até sobrar menos de 960.
- Se não houver frame completo, retorna um `Vec` vazio.

O resto (menos de 960 amostras) fica no acumulador para a próxima chamada. Isso garante que nenhuma amostra seja perdida entre chamadas.

## Exemplo de uso

```rust
use hydrargyum::modules::{
    audio_dsp::codec::AudioEncoder,
    audio_io::{
        ring_buff::ring_buffer,
        stream::input_stream_builder,
    },
};
use cpal::traits::StreamTrait;

let (capture_producer, capture_consumer) = ring_buffer(4800);
let input_stream = input_stream_builder(capture_producer)?;
let mut encoder = AudioEncoder::new(capture_consumer)?;

input_stream.play()?;

loop {
    let packets = encoder.process()?;
    for packet in packets {
        // Enviar `packet` para a rede (futuro webrtc_transport).
        let _ = packet;
    }

    std::thread::sleep(std::time::Duration::from_millis(20));
}
```

Observações:

- `process()` deve ser chamado em uma thread normal, **nunca** no callback do `cpal`.
- Ele aloca (`Vec` de pacotes e cópia de cada payload) e faz `drain` no acumulador; não é uma função de tempo real.
- O loop acima dorme 20 ms, mas `process()` drena tudo o que estiver disponível. Se a thread atrasar, ele pode devolver vários pacotes de uma vez.
- O primeiro segundo de áudio é descartado pelo warmup.

## Por que 20 ms e 960 samples

- `FRAME_MS = 20` e `SAMPLE_RATE = 48.000` vêm de `audio_types`.
- `FRAME_SIZE = 48.000 × 20 / 1.000 = 960`.
- O Opus suporta frames de 2,5 a 60 ms; 20 ms é um equilíbrio comum entre latência e overhead de pacote.
- Cada pacote Opus representa 20 ms de áudio; o timestamp RTP deve avançar 960 por pacote.

## Erros

| Erro | Origem |
|---|---|
| `AudioError::Opus` | Falha ao criar o encoder ou codificar um frame |

O `?` em `encode_float` converte `opus::Error` para `AudioError::Opus` via `#[from]`.

## Testes

`tests/codec_tests.rs` cobre:

1. **Warmup e um pacote por frame:** gera 10 frames, espera 10 pacotes não vazios e com no máximo 4000 bytes.
2. **Nada antes de um frame completo:** com warmup + 959 amostras, `process()` devolve vazio; ao adicionar 1 amostra, devolve 1 pacote.
3. **Resto entre chamadas:** com warmup + 1 frame + meio frame, devolve 1 pacote; ao adicionar o outro meio, devolve mais 1.

Esses testes usam uma senoide de 440 Hz apenas para gerar sinal; o teste de decodificação com 1 kHz é separado, em `neteq_tests.rs`.

## Limitações atuais

- Não há controle de taxa de bits (bitrate) nem de complexidade do Opus.
- Não há tratamento de perda no encoder; isso é papel do NetEq no receptor.
- Não há reamostragem: a entrada precisa estar em 48 kHz.
- O encoder não envia nada; o transporte é responsabilidade futura.
