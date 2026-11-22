# Termos técnicos

Este arquivo é dedicado exclusivamente às definições dos termos técnicos usados no código e na documentação. A ordem é aproximadamente a ordem em que eles aparecem no pipeline.

## Categorias

- Áudio e amostras
- Buffers e threads
- Codec
- Rede e RTP
- Jitter buffer
- Topologia
- Hardware e bibliotecas

## Áudio e amostras

**PCM (Pulse Code Modulation)**
Representação digital de áudio como uma sequência de amostras. O projeto usa PCM em `f32`, com valores normalizados entre `-1.0` e `1.0`.

**Amostra (`sample`)**
Um valor de áudio em um instante. Em mono, uma amostra representa o canal único; em estéreo, há uma amostra por canal.

**Taxa de amostragem (`sample rate`)**
Quantas amostras existem por segundo. O projeto usa 48.000 Hz. Um frame de 20 ms tem `48.000 × 0,020 = 960` amostras.

**Mono**
Um único canal de áudio. O projeto assume mono em todo o pipeline.

**Frame**
Um bloco de amostras com duração fixa. No encoder, um frame Opus tem 20 ms (960 amostras). Na saída do NetEq, o frame de mixagem tem 10 ms (480 amostras).

**Warmup**
Período inicial descartado para evitar transientes. O `AudioEncoder` descarta o primeiro segundo de captura (`WARMUP_SAMPLES = 48.000`).

**Underrun**
Situação em que o consumidor precisa de mais amostras do que o buffer tem. Na saída, o restante é preenchido com zero por `fill_underrun`, produzindo silêncio em vez de lixo.

**Overflow**
Situação em que o produtor tenta escrever mais do que o ring buffer suporta. A política do projeto é descartar o dado novo e manter o antigo.

## Buffers e threads

**Ring buffer**
Buffer circular de tamanho fixo. O projeto usa `ringbuf::HeapRb` com capacidade em amostras. É usado para desacoplar a thread de tempo real do `cpal` das threads normais de codificação/recepção.

**SPSC (Single-Producer, Single-Consumer)**
Padrão em que apenas uma thread escreve e apenas uma lê. O ring do projeto é SPSC; não clone as metades.

**Produtor / consumidor**
`AudioProducer` é o lado de escrita; `AudioConsumer` é o lado de leitura. Eles são criados juntos por `ring_buffer(size)`.

**Thread de tempo real**
Thread do callback de áudio do `cpal`. Deve evitar alocação, mutex, I/O e logging.

**Fila de comandos**
Canal `mpsc::sync_channel` com capacidade 64 usado para enviar pacotes e eventos de saída ao `AudioReceiver`. `push_packet` usa `try_send` (não bloqueia); `leave` usa `send` (pode bloquear).

## Codec

**Opus**
Codec de áudio de baixa latência usado para voz. O projeto usa `opus::Encoder` em 48 kHz, mono, `Application::Voip`.

**Bitrate**
Taxa de bits do codec. O projeto não configura bitrate explicitamente; usa o padrão do Opus.

**Pacote Opus**
Bytes produzidos pelo encoder para um frame. O encoder devolve `Vec<u8>` por frame; o payload é enviado pela rede (futuro).

**Application::Voip**
Modo do Opus otimizado para voz. Prioriza inteligibilidade e latência em vez de fidelidade musical.

## Rede e RTP

**RTP (Real-time Transport Protocol)**
Protocolo que carrega mídia em tempo real, com número de sequência, timestamp, SSRC e payload type.

**SRTP**
Versão cifrada do RTP. Protege cada salto (cliente ↔ host) com DTLS.

**DTLS**
Handshake de criptografia usado pelo WebRTC para negociar chaves do SRTP.

**SSRC**
Identificador de uma fonte de stream RTP. No projeto, é um `u32` passado para `make_audio_packet`.

**Sequence number**
Número de sequência RTP; usado para ordenar pacotes e detectar perda.

**Timestamp RTP**
Posição do primeiro sample do pacote no tempo. Para Opus a 48 kHz, avança 960 por pacote de 20 ms.

**Payload type**
Número que identifica o formato do payload. O projeto usa 111 para Opus. Precisa bater com o SDP/transporte.

**Marker**
Bit do RTP que marca o início de um talkspurt. O primeiro pacote de uma fala costuma usar `true`.

## Jitter buffer

**Jitter**
Variação no tempo de chegada dos pacotes. Mesmo que o emissor envie a cada 20 ms, a rede pode entregar em rajadas ou com atrasos diferentes.

**Jitter buffer**
Buffer que absorve jitter, reordena pacotes e espera o momento certo de tocar. O projeto usa NetEq.

**NetEq**
Jitter buffer adaptativo do WebRTC. Reordena, absorve jitter, estira/encolhe o tempo e oculta perda. No projeto, há um NetEq por participante.

**PLC (Packet Loss Concealment)**
Técnica para mascarar pacotes perdidos gerando áudio aproximado. O NetEq faz PLC internamente.

**FEC (Forward Error Correction)**
Informação extra enviada para reconstruir pacotes perdidos. A trait do NetEq usada pelo projeto não expõe FEC; o adaptador chama `decode_float(..., false)`.

**Expansão (`expand`)**
Quando o NetEq não tem um pacote e gera áudio por um curto período para cobrir a lacuna.

**Atraso máximo (`max_delay_ms`)**
Limite de buffering do NetEq. No projeto, 250 ms.

**Alvo de preenchimento (`target fill`)**
Quantidade de áudio que o `AudioReceiver` tenta manter no ring de reprodução. No projeto, 40 ms (1920 samples).

## Topologia

**SFU (Selective Forwarding Unit)**
Servidor que recebe fluxos de mídia e os repassa aos outros participantes sem mixar. No Hydrargyum, o host é um SFU simples: encaminha o payload cifrado, não decodifica.

**Host**
Participante que coordena a chamada e atua como SFU. Não é um servidor de mixagem.

**P2P (peer-to-peer)**
Comunicação direta entre participantes. O Hydrargyum não usa malha P2P completa; usa host/SFU.

**E2E (end-to-end)**
Cifragem ponta a ponta: só os clientes conseguem decifrar o payload; o host vê apenas bytes cifrados.

**AEAD**
Cifra autenticada que garante sigilo e integridade juntos (por exemplo, ChaCha20-Poly1305 ou AES-GCM). Ainda é uma decisão em aberto no projeto.

**ICE / STUN**
ICE descobre caminhos de rede; STUN informa o endereço público atrás de NAT. Ainda não implementados.

**Offer/Answer**
Troca de descrições SDP que inicia uma conexão WebRTC. Ainda não implementado.

## Hardware e bibliotecas

**cpal**
Biblioteca Rust de entrada/saída de áudio. O projeto usa `cpal` 0.18.2, com o host e os dispositivos padrão do sistema.

**Stream**
Objeto do `cpal` que representa captura ou reprodução. Criar não inicia; é preciso chamar `play()`. O stream precisa continuar vivo.

**BufferSize::Default**
Deixa o `cpal` escolher o tamanho interno do bloco de callback. Não é o tamanho do ring buffer do projeto.

**ringbuf**
Crate Rust usada para o ring buffer. O projeto usa `HeapRb<f32>` e as metades com cache.

**thiserror**
Crate usada para definir erros tipados com `#[derive(Error)]`.

**log**
Crate de logging. O projeto emite `log::warn!` no jitter buffer, mas não inicializa logger; sem um logger no binário, os avisos são descartados.

**Tauri**
Framework de desktop planejado para o app. Ainda não faz parte deste repositório.
