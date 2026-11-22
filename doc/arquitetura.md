# Arquitetura e fluxo de dados

Este documento mostra como os módulos se encaixam, quais threads existem e onde o áudio é transformado. A parte de rede ainda não está implementada; o que existe é o caminho local de captura, codificação, recepção e reprodução.

## Estrutura de módulos

```
src/
├── lib.rs                         # expõe `modules`
├── main.rs                        # vazio; não há binário operacional ainda
└── modules/
    ├── mod.rs                     # registra audio_dsp, audio_io e audio_types
    ├── audio_types/
    │   └── mod.rs                 # constantes compartilhadas
    ├── audio_io/
    │   ├── mod.rs                 # AUDIO_CONFIG
    │   ├── error.rs               # AudioError
    │   ├── ring_buff.rs           # AudioProducer, AudioConsumer, ring_buffer
    │   └── stream.rs              # builders de entrada/saída + fill_underrun
    └── audio_dsp/
        ├── mod.rs                 # registra codec e jitter_buffer
        ├── codec.rs               # AudioEncoder
        └── jitter_buffer.rs       # NetEq, AudioReceiver, JitterEngine
```

## Responsabilidades

| Módulo | Responsabilidade | Não faz |
|---|---|---|
| `audio_types` | Fonte única de taxa, frame e payload type | Não conhece `cpal`, Opus ou rede |
| `audio_io::ring_buff` | Transportar amostras entre threads com tamanho fixo | Não codifica, não decide política de áudio |
| `audio_io::stream` | Criar streams `cpal` e ligar callbacks aos ring buffers | Não codifica, não faz jitter buffering |
| `audio_io::error` | Erros de dispositivo, stream e Opus | Não define política de recuperação |
| `audio_dsp::codec` | Converter `f32` capturado em pacotes Opus | Não lê hardware, não envia pela rede |
| `audio_dsp::jitter_buffer` | Reordenar, absorver jitter, ocultar perda e decodificar | Não conhece `cpal` nem RTP de rede |
| `webrtc_transport` (futuro) | RTP/ICE/DTLS/SRTP e encaminhamento no SFU | Não deve conhecer `cpal` nem ring buffers |

## Caminho de envio

```
[Microfone]
    │
    ▼
callback de entrada do cpal  (thread de tempo real)
    │  producer.write(data)
    ▼
AudioProducer ── ring buffer de captura ── AudioConsumer
                                                │
                                                ▼
                                     AudioEncoder::process()
                                                │
                                  frames de 960 samples
                                                │
                                                ▼
                                     Vec<Vec<u8>> (Opus)
                                                │
                                                ▼
                                  cifragem E2E + RTP (futuro)
```

Passo a passo:

1. `input_stream_builder` obtém o dispositivo de entrada padrão e cria um `cpal::Stream` com `AUDIO_CONFIG`.
2. O callback de entrada recebe `&[f32]` e chama `AudioProducer::write`.
3. `AudioProducer::write` usa `push_slice`: grava o que couber e devolve quantas amostras foram aceitas. Se o ring estiver cheio, as amostras novas são descartadas; as antigas permanecem.
4. Em outra thread, `AudioEncoder::process` drena o `AudioConsumer`, descarta o warmup inicial de 1 segundo e acumula amostras.
5. Cada 960 amostras viram um pacote Opus de 20 ms. O encoder devolve `Vec<Vec<u8>>`, uma entrada por frame completo.
6. O transporte (futuro) cifra o payload, monta o RTP e envia.

## Caminho de recepção

```
Pacote Opus + metadados RTP (futuro)
    │
    ▼
make_audio_packet(...)
    │
    ▼
AudioReceiverHandle::push_packet(id, packet)
    │  canal limitado (64 comandos)
    ▼
thread "audio-receiver"
    │
    ├── drain_commands: insere pacotes no JitterEngine
    │
    ▼
JitterEngine
    │  um NetEq por participante
    ▼
NetEq::get_audio()  (frames de 10 ms)
    │
    ▼
NetEqOpusDecoder  (Opus -> f32)
    │
    ▼
mix_frame: soma participantes + clamp
    │
    ▼
AudioProducer (ring de reprodução)
    │
    ▼
callback de saída do cpal
    │  consumer.read + fill_underrun
    ▼
[Alto-falante]
```

Passo a passo:

1. O transporte recebe RTP, valida/decifra o payload e entrega os bytes Opus com número de sequência, timestamp, SSRC e payload type.
2. `make_audio_packet` monta um `neteq::AudioPacket` com `SAMPLE_RATE`, 1 canal e `FRAME_MS`.
3. `AudioReceiverHandle::push_packet` tenta enviar o comando para a fila. Ele **não bloqueia**: retorna `false` se a fila estiver cheia ou se o receiver já foi derrubado.
4. A thread `audio-receiver` drena a fila e insere cada pacote no `JitterEngine`.
5. O `JitterEngine` mantém um `NetEq` por `ParticipantId`. O primeiro pacote de um participante cria o NetEq e registra o `NetEqOpusDecoder` no payload type 111.
6. O NetEq reordena, absorve jitter, decide perda/estiramento e devolve frames de 10 ms (480 samples).
7. `NetEqOpusDecoder` decodifica o payload Opus para `f32`. O NetEq cuida da ocultação de perda; o adaptador não pede FEC.
8. `mix_frame` soma todos os participantes e aplica `clamp` em `[-1.0, 1.0]`.
9. O frame de 10 ms é escrito no ring de reprodução enquanto houver espaço e até o alvo de 40 ms.
10. O callback de saída lê o ring. Se não houver amostras suficientes, `fill_underrun` preenche o restante com zero.

## Threads e propriedade

| Thread | Dono do quê | Tempo real? | Observações |
|---|---|---|---|
| Callback de entrada do `cpal` | `AudioProducer` de captura | Sim | Só escreve no ring; sem alocação/bloqueio |
| Thread da aplicação | `AudioEncoder` e `AudioConsumer` de captura | Não | `process()` aloca `Vec` e pacotes |
| Thread de rede (futura) | `AudioReceiverHandle` | Não | Chama `push_packet`; pode compartilhar clones do handle |
| Thread `audio-receiver` | `JitterEngine`, `NetEq`s e `AudioProducer` de reprodução | Não | Drena comandos, decodifica e escreve no ring |
| Callback de saída do `cpal` | `AudioConsumer` de reprodução | Sim | Só lê o ring e preenche underrun |

Consequências:

- `AudioProducer` e `AudioConsumer` são as duas metades de um ring SPSC (single-producer, single-consumer). Não clone as metades: cada lado deve pertencer a uma única thread.
- `AudioReceiverHandle` é clonável e pode ser compartilhado com a thread de rede.
- `AudioReceiver` precisa continuar vivo enquanto a chamada existir. O `Drop` dele para a thread e faz `join`.
- `JitterEngine` não é compartilhado entre threads; ele pertence à thread `audio-receiver`.

## Buffers e latência

| Buffer/constante | Valor atual | Efeito |
|---|---|---|
| Ring de captura | definido pelo chamador; a documentação usa 4800 samples (100 ms) | Se encher, descarta áudio novo |
| Warmup do encoder | 48.000 samples (1 s) | O primeiro segundo de captura é descartado |
| Frame de entrada Opus | 960 samples (20 ms) | Unidade de pacote |
| Fila de comandos do receiver | 64 comandos | Pacotes são recusados quando enche |
| Alvo de preenchimento da reprodução | 1920 samples (40 ms) | Amortecimento contra jitter local |
| Frame de saída do NetEq | 480 samples (10 ms) | Unidade de mixagem/reprodução |
| Atraso máximo do NetEq | 250 ms | Limite de buffering do jitter buffer |
| Sleep da thread do receiver | 2 ms | Menos CPU, adiciona latência de polling |
| Ring de reprodução | definido pelo chamador | Se pequeno, ocorre underrun; se grande, aumenta latência |

O caminho de captura e o de reprodução são independentes. O tamanho dos ring buffers é escolha da aplicação; o código não impõe 100 ms, apenas a documentação e os testes usam esse valor como referência.

## Erros e logging

- `AudioError` cobre ausência de dispositivo, erro de stream e erro do Opus.
- O `JitterEngine` usa `log::warn!` para falhas ao criar NetEq, pacotes rejeitados e falhas de `get_audio`.
- O projeto não inicializa um logger. Sem um logger configurado no binário, esses avisos são descartados. Veja [Operação](operacao.md#logging).
- O callback de erro dos streams usa `eprintln!`. Isso não está no callback de dados, mas ainda é uma saída de texto; para produção, o ideal é encaminhar para o sistema de logging.

## Fronteiras e decisões

- O host é um **SFU simples**: recebe o payload de um cliente e encaminha o mesmo payload cifrado aos demais. Ele não decodifica, não mixa e não precisa de `AudioReceiver`.
- A mixagem de vários participantes acontece no cliente, dentro de `JitterEngine::mix_frame`.
- O transporte futuro trabalha com bytes Opus/RTP. Ele não deve importar `cpal`, `ring_buff` ou `audio_dsp`.
- O cliente entrega pacotes ao `AudioReceiverHandle`; a partir daí o áudio é responsabilidade do módulo de áudio.
- A cifragem ponta a ponta ainda é uma decisão em aberto; veja a seção 9 de `HYDRARGYUM_AUDIO_AGENT.md`.

## O que ainda não existe

- `webrtc_transport/`: ICE, DTLS, SRTP, RTP, tracks e SFU.
- `signaling/`: offer/answer e candidatos ICE.
- `call_session/`: orquestração da chamada.
- Cifragem ponta a ponta.
- Seleção de dispositivo de entrada/saída.
- Binário ou interface gráfica para operar o áudio.

Para montar um fluxo local de teste, veja [Operação](operacao.md#montando-um-fluxo-local).
