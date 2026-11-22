# Hydrargyum — Contexto de Áudio para o Agente de IA

> Objetivo deste documento: deixar o agente pronto para começar a construir os módulos de WebRTC (`webrtc_transport/`) sem reabrir decisões já tomadas.
> Convenção: código e identificadores em inglês; explicações em PT-BR.

---

## 1. O que é o projeto

Hydrargyum é um app de chamada de voz em tempo real, escrito em Rust, com Tauri como framework de desktop. Alvos: Windows, macOS e Linux.

Modelo de rede: inspirado no TeamSpeak. Existe um **host** por chamada e um servidor de diretório. O áudio é host-based (não é malha P2P completa).

Meta atual: a parte de áudio local (captura, buffer, codec e NetEq) está fechada. O próximo bloco é o transporte WebRTC, depois signaling e call_session.

---

## 2. Stack e versões

| Crate | Papel |
|---|---|
| `cpal` | I/O de áudio de hardware |
| `ringbuf` 0.5.x (`HeapRb`) | Buffer entre thread de tempo real e threads normais |
| `webrtc-rs` | Transporte (ICE, DTLS/SRTP, RTP) |
| Opus (binding em `audio_dsp/codec.rs`) | Codec de voz |
| `neteq` | Jitter buffer no receptor |
| `thiserror` | Erros tipados nas camadas internas |
| Tauri | Aplicação desktop |

O agente deve verificar as versões reais no `Cargo.toml` antes de assumir qualquer API. `ringbuf` 0.5.x e `cpal` já foram validados em loopback (PipeWire).

---

## 3. Estrutura de módulos

- `audio_io/` — `ring_buff.rs`, `stream.rs`, `error.rs`, `mod.rs`
- `audio_dsp/` — `codec.rs` (encoder Opus) e `jitter_buffer.rs` (NetEq + `AudioReceiver`)
- `audio_types/` — constantes compartilhadas: taxa de amostragem, frame e payload type
- `webrtc_transport/` — **próxima etapa; ainda não iniciado**
- `signaling/`
- `call_session/`

---

## 4. Decisões já tomadas (não reabrir sem debater com o usuário)

### Formato de áudio
- Amostras são `f32` em todo o pipeline.
- Config única: `AUDIO_CONFIG` em `audio_io/mod.rs` — mono e `BufferSize::Default`; a taxa vem de `audio_types::SAMPLE_RATE` (48 kHz). Usada por entrada e saída. `input_config()`/`output_config()`, `devices.rs` e as variantes `NoSupported*Config` foram removidos no commit `527dbdc`.

### Ring buffer
- Política de overflow: **descartar o dado novo**, nunca sobrescrever o antigo (`push_overwrite` não existe depois do split no `ringbuf` 0.5.x).
- Ring buffer de captura: 100 ms = 4800 samples, derivado da taxa por constante.
- O ring buffer é criado no `audio_io`, não no codec.

### Opus
- `Encoder::new` com 48 kHz, mono, `Application::Voip`.
- Frame de 20 ms = 960 samples, definido por `FRAME_MS` e `FRAME_SIZE` em `audio_types/`.
- `codec.rs` tem só `AudioEncoder`; o construtor recebe o `AudioConsumer` e `process()` devolve os pacotes.
- A recepção usa `NetEqOpusDecoder` em `jitter_buffer.rs`, que envolve `opus::Decoder` e é registrado no payload type 111.

### Jitter buffer
- Usar **neteq** no receptor (em vez do caminho simplificado com reorder buffer + PLC do Opus).

### Topologia de transporte
- O host funciona como **SFU simples**: recebe pacotes Opus de cada cliente e **encaminha** para os outros. Não mixa áudio no host.
- Segurança: o usuário quer **cifragem ponta a ponta por cima do SFU**, e quer isso já no `webrtc_transport/`, antes do `config.rs`.

### Organização do código
- `stream.rs` fica unificado (sem separar capture/playback).
- `devices.rs` foi removido; os builders chamam `default_host()` diretamente.
- Diagnóstico de hardware fica em `tests/hardware_tests.rs` (manual e ignorado no CI); não há exemplo separado.

---

## 5. Status atual

| Item | Status |
|---|---|
| `ring_buff.rs` | Implementado; sem arquivo de teste dedicado, exercitado por `codec_tests` e `receiver_tests` |
| `devices.rs` | Removido no commit `527dbdc`; configuração fixa em `audio_io/mod.rs` |
| `stream.rs` | Implementado, com `fill_underrun` no callback de saída |
| `audio_types/` | Constantes centralizadas: 48 kHz, 20 ms, 960 samples e payload type 111 |
| Encoder Opus (`codec.rs`) | Implementado |
| Receptor com NetEq (`jitter_buffer.rs`) | Implementado: `JitterEngine`, `AudioReceiver` e `NetEqOpusDecoder` |
| Teste de hardware do mic (`tests/hardware_tests.rs`) | Manual, ignorado no CI; verifica 48 kHz mono |
| Testes de lógica pura | `codec_tests`, `neteq_tests`, `receiver_tests` e `stream_tests` |
| Abstração de `cpal::Host` por trait | Proposta, não decidida; não é necessária para o transporte |
| `webrtc_transport/` | Não iniciado (próxima etapa) |

O usuário quer entender a fundo o receptor com NetEq. Ao explicar, detalhe o fluxo em vez de só entregar o código.

---

## 6. Pipeline de ponta a ponta

Envio:

1. Callback do `cpal` (thread de tempo real) escreve `f32` no ring buffer de captura.
2. `AudioEncoder` consome 960 samples por vez e produz um pacote Opus.
3. Camada E2E cifra o payload Opus.
4. `webrtc_transport` empacota em RTP e envia (SRTP sobre DTLS).

Recepção:

1. `webrtc_transport` recebe RTP e entrega o payload Opus.
2. Camada E2E decifra.
3. `AudioReceiverHandle::push_packet` entrega o `AudioPacket` ao `AudioReceiver`; o `JitterEngine` insere no NetEq.
4. NetEq reordena, absorve jitter e decide perda/estiramento; `NetEqOpusDecoder` decodifica para `f32`.
5. `AudioReceiver` escreve frames de 10 ms no ring buffer de reprodução; o callback de saída do `cpal` consome.

Papel do host (SFU): recebe do cliente A e encaminha o mesmo payload cifrado para B, C, etc. O host **não** decifra o conteúdo de áudio.

---

## 7. Regras invioláveis

1. **Callbacks do cpal** não alocam, não travam mutex, não fazem I/O, não usam `println!`. Comunicação só via ring buffer.
2. **Overflow**: descartar dado novo. Não introduzir sobrescrita.
3. Tudo é `f32`, 48 kHz, mono. Qualquer conversão de formato é decisão a debater, não a assumir.
4. Não alterar `audio_io/` sem avisar e justificar.
5. Constantes de tempo (20 ms, 100 ms) sempre derivadas da taxa de amostragem, nunca números soltos.
6. Erros em camadas internas com `thiserror`.
7. Testes que dependem de hardware ficam separados dos testes de lógica pura (CI não tem placa de som). Hardware: `tests/hardware_tests.rs`. Lógica pura: `tests/*_tests.rs`.
8. Prioridade de design: qualquer simplificação que não afete diretamente o desempenho **vence**.

---

## 8. Plano do `webrtc_transport/` (proposta, por arquivo)

Os nomes abaixo são sugestão; só `config.rs` foi citado pelo usuário. Ajuste conforme o que fizer sentido e justifique.

| Arquivo | Responsabilidade |
|---|---|
| `config.rs` | Parâmetros do transporte: servidores ICE (STUN), timeouts e endereços UDP. Payload type e clock rate vêm de `audio_types` |
| `error.rs` | Enum de erros do transporte com `thiserror` |
| `crypto.rs` | Cifragem ponta a ponta do payload Opus (ver seção 9). Deve vir **antes** do `config.rs` ser finalizado, por pedido do usuário |
| `peer.rs` | Encapsula `PeerConnectionBuilder` e `PeerConnection` do `webrtc` 0.21: criação, offer/answer, candidatos ICE, estado da conexão |
| `track.rs` | Track de áudio local (envia pacotes Opus como RTP) e track remota (lê RTP e entrega ao receptor) |
| `sfu.rs` | Lado host: mantém a lista de peers e encaminha pacotes de um para os demais, sem decifrar |
| `mod.rs` | API pública do módulo, usada por `call_session` |

Ordem de trabalho sugerida: `error.rs` → `crypto.rs` → `config.rs` → `peer.rs` → `track.rs` → `sfu.rs`.

Fronteiras:
- `webrtc_transport` **não** conhece `cpal` nem ring buffers de hardware. Ele recebe pacotes Opus (bytes) e entrega pacotes Opus (bytes).
- No host (SFU), o payload cifrado é encaminhado sem decodificação; o host não precisa de `AudioReceiver`.
- No cliente, o transporte entrega o payload para a camada E2E e depois para `AudioReceiverHandle::push_packet`; o `AudioReceiver` é a fronteira com o áudio local.
- Signaling (troca de offer/answer/ICE) é assunto de `signaling/`. O transporte só expõe "gera offer", "aplica answer", "adiciona candidato" e emite eventos.
- `call_session` orquestra tudo; o transporte não decide política de chamada.

### API do `webrtc` 0.21

- Construção: `PeerConnectionBuilder::new().with_configuration(config).with_handler(Arc::new(handler)).with_udp_addrs(vec!["0.0.0.0:0"]).build().await?`.
- `build()` devolve `impl PeerConnection`; a conexão precisa continuar viva enquanto a chamada existir.
- Eventos: implementar `PeerConnectionEventHandler` com `#[async_trait::async_trait]`; callbacks principais: `on_ice_candidate`, `on_track`, `on_connection_state_change` e `on_negotiation_needed`. A implementação exigirá a dependência `async-trait` no `Cargo.toml`.
- SDP/ICE: `create_offer`, `set_local_description`, `set_remote_description` e `add_ice_candidate`.
- O handler recebe `Arc<dyn TrackRemote>` em `on_track`; a track remota entrega os pacotes RTP que serão convertidos em `AudioPacket` para o `AudioReceiver`.

---

## 9. Cifragem ponta a ponta — ponto a decidir

Por que existe: DTLS/SRTP protegem cada salto (cliente ↔ host). Como o host é SFU, ele vê o payload em claro depois do SRTP. Para o host não ouvir o áudio, é preciso uma camada extra aplicada ao payload Opus **antes** do RTP.

Proposta do agente para debater com o usuário (não está decidida):
- Cifra AEAD (ChaCha20-Poly1305 ou AES-GCM) aplicada por frame Opus.
- Cabeçalho RTP em claro (o SFU precisa dele para rotear); só o payload é cifrado.
- Nonce derivado de SSRC/número de sequência/contador, nunca reutilizado com a mesma chave.
- Uma chave de sessão compartilhada por chamada, trocada via signaling, trocada novamente quando alguém entra/sai (simplicidade primeiro).

Questões em aberto que o agente deve levantar antes de implementar: como as chaves são distribuídas (o host não pode ser quem as vê), como autenticar participantes, e o custo de rotação de chave.

---

## 10. Testes

- Lógica pura (CI): codec, NetEq (senoide de 1 kHz), receiver, underrun de stream e, futuramente, serialização/cifragem de payload e roteamento do SFU com peers falsos.
- Hardware (manual): `tests/hardware_tests.rs`; o antigo `examples/test_audio.rs` foi removido no commit `527dbdc`.
- Para testar o transporte sem rede real: usar dois peers no mesmo processo (loopback) e comparar os bytes que entram e saem.
- Pendência: definir os testes do `webrtc_transport` (loopback de dois peers) antes de implementar.

---

## 11. Como trabalhar com o usuário (Zhtyz)

- Trabalha iterativamente, arquivo por arquivo, corrigindo tipos e problemas de API conforme aparecem.
- **Nunca entregar código completo sem autorização explícita.** Primeiro: rationale do design e pseudo-código. Código real só se ele pedir. Em revisão de código: apontar e explicar o problema, sem código corrigido.
- Quando houver várias soluções: recomendar **uma**, com justificativa, não uma lista de trade-offs.
- Se ele rejeitar uma solução: debater, não reformular em silêncio.
- Explicações detalhadas, com exemplos; texto corrido em instruções passo a passo.
- Ao citar um termo técnico ou sigla, explicar o que é.
- Em debug: sintoma preciso → causa raiz → o que o código deveria fazer. Se não tiver certeza da causa, apresentar a hipótese mais provável e debater.
- Ao retomar depois de uma pausa: perguntar onde parou e reconstruir o contexto antes de agir.
- Tom técnico e objetivo em contexto de código.

---

## 12. Termos técnicos

As definições completas ficam em [`doc/termos_tecnicos.md`](doc/termos_tecnicos.md).

---

## 13. Registro de progresso

### 2026-11-22 — Cargo.toml: limpeza de dependências

- Removido `runtime-tokio = "0.0.0"`: não é crate; `runtime-tokio` é feature do `webrtc` 0.21, já ativa nas features default (`default = ["runtime-tokio", "crypto-ring"]`).
- Removido `features = "0.10.0"`: sem uso no código.
- `anyhow` movido para `[dev-dependencies]`; o único uso é `tests/hardware_tests.rs`.
- `cargo build` ok; `cargo test` ok (6 testes de codec; teste de hardware ignorado).
- Próximo passo: mudança 2 — tratar underrun no callback de saída de `audio_io/stream.rs`.

### 2026-11-22 — `audio_io/stream.rs`: underrun de saída

- Adicionada `fill_underrun(data: &mut [f32], filled: usize)`: limita `filled` ao tamanho do buffer e preenche o restante com `0.0`.
- O callback de saída agora usa o retorno de `consumer.read(data)` e chama `fill_underrun`; antes, o restante do buffer podia manter amostras antigas.
- Novo `tests/stream_tests.rs` (lógica pura, sem hardware): leitura parcial, leitura completa, leitura zero e `filled` acima do tamanho.
- `cargo build` ok; `cargo test` ok (6 codec + 4 stream; hardware ignorado).
- Próximo passo: mudança 3 — `AudioReceiver::spawn` com `AudioProducer` concreto e teste que instancia o `spawn`.

### 2026-11-22 — `AudioReceiver::spawn` com `AudioProducer` concreto

- `AudioReceiver::spawn` e `run` deixaram de ser genéricos; recebem `AudioProducer`.
- `run` usa `playback.inner` para `occupied_len`, `vacant_len` e `push_slice`; `audio_io/` não foi alterado.
- Novo `tests/receiver_tests.rs` instancia o `spawn` com um `ring_buffer` real e derruba o receiver; sem hardware.
- `cargo build` ok; `cargo test` ok (6 codec + 4 stream + 1 receiver; hardware ignorado).
- Próximo passo: mudança 4 — adaptador Opus para o NetEq e teste de correlação com senoide de 1 kHz.

### 2026-11-22 — adaptador Opus real para o NetEq

- Correção da verificação anterior: com `default-features = false`, `neteq::codec::OpusDecoder` é o alias de `StubOpusDecoder` (codec.rs), que devolve **silêncio**; não é o `wasm_stub` que devolvia 440 Hz.
- `neteq::codec::AudioDecoder` expõe só `sample_rate`, `channels` e `decode(&[u8])`; não há PLC nem FEC na trait. O NetEq faz a ocultação de perda internamente; o adaptador usa `decode_float(..., fec = false)`.
- Adicionado `NetEqOpusDecoder` em `jitter_buffer.rs`, em volta de `opus::Decoder` 0.4.0, com buffer de até 120 ms derivado de `SAMPLE_RATE`.
- `JitterEngine::new_peer` registra o adaptador no payload type 111; a feature `native` do `neteq` continua desligada.
- Novo `tests/neteq_tests.rs`: senoide de 1 kHz codificada com `AudioEncoder`, passada pelo `JitterEngine`, comparação por correlação normalizada com busca de atraso.
- Prova com o stub: o teste falhou com `rms 0` (saída silenciosa). Com o adaptador: teste passa.
- `cargo build` ok; `cargo test` ok (6 codec + 4 stream + 1 receiver + 1 neteq; hardware ignorado).
- Próximo passo: mudança 5 — confirmar com o usuário antes de remover `codec::AudioDecoder`.

### 2026-11-22 — remoção do `codec::AudioDecoder`

- Removido `AudioDecoder` de `src/modules/audio_dsp/codec.rs`; o arquivo ficou só com `AudioEncoder`, `SAMPLE_RATE` e `FRAME_SIZE`.
- Removidos os três testes que dependiam dele em `tests/codec_tests.rs`: roundtrip, pacote ausente e contagem de amostras descartadas.
- O caminho de recepção passa a ser exclusivamente `NetEq` + `NetEqOpusDecoder`; `AudioProducer` continua usado pelo `AudioReceiver`.
- `cargo build` ok; `cargo test` ok (3 codec + 4 stream + 1 receiver + 1 neteq; hardware ignorado).
- Próximo passo: mudança 6 — centralizar payload type 111, 48 kHz e 20 ms em `audio_types/`.

### 2026-11-22 — constantes de áudio centralizadas em `audio_types/`

- Criado `src/modules/audio_types/mod.rs` com `SAMPLE_RATE = 48_000`, `FRAME_MS = 20`, `FRAME_SIZE` derivado e `OPUS_PAYLOAD_TYPE = 111`.
- `codec.rs` e `jitter_buffer.rs` deixaram de definir esses valores; passaram a importar de `audio_types`.
- `audio_io/mod.rs` usa `SAMPLE_RATE` no lugar de `cpal::SAMPLE_RATE_48K`; foi a única alteração em `audio_io/`.
- Testes de codec, NetEq e receiver atualizados para importar de `audio_types`.
- `cargo fmt` aplicado; `cargo build` ok; `cargo test` ok (3 codec + 4 stream + 1 receiver + 1 neteq; hardware ignorado).
- Próximo passo: mudança 7 — corrigir o pipeline e a fronteira do SFU antes de `webrtc_transport/`.

### 2026-11-22 — documentação alinhada ao pipeline atual

- `devices.rs` e `examples/test_audio.rs` continuam removidos (commit `527dbdc`); a configuração é fixa em `audio_io/mod.rs` e usa `audio_types::SAMPLE_RATE`.
- Estrutura, status e pipeline de recepção corrigidos: `AudioReceiver` → `JitterEngine` → `NetEqOpusDecoder`, sem `AudioDecoder`.
- Documentada a API do `webrtc` 0.21: `PeerConnectionBuilder`, `PeerConnectionEventHandler` e `PeerConnection`.
- Reforçada a fronteira do SFU: o transporte troca bytes Opus/RTP, não conhece `cpal` nem ring buffers; o host encaminha o payload cifrado sem decodificar.
- Testes de hardware apontam apenas para `tests/hardware_tests.rs`.
- Sem alteração de código de produção; `cargo build` ok; `cargo test` ok (3 codec + 4 stream + 1 receiver + 1 neteq; hardware ignorado).

### 2026-11-22 — guia de código e operação em `doc/`

- Criado `doc/README.md` com índice, visão geral, invariantes e comandos rápidos.
- Criados guias específicos: `arquitetura.md`, `audio_types.md`, `audio_io.md`, `codec.md`, `jitter_buffer.md`, `testes.md`, `operacao.md` e `termos_tecnicos.md`.
- Documentados fluxo de envio/recepção, threads, buffers, NetEq, encoder, testes e diagnóstico.
- Nenhum código de produção alterado; `cargo build` e `cargo test` continuam passando.
