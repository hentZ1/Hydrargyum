# Testes

A suíte é dividida entre testes de lógica pura, que rodam no CI sem placa de som, e um teste manual de hardware. O comando `cargo test` executa todos os testes de lógica e marca o de hardware como `ignored`.

## Matriz atual

| Arquivo | Testes | Precisa de hardware? | O que cobre |
|---|---:|---|---|
| `tests/codec_tests.rs` | 3 | Não | Warmup, frames completos e resto entre chamadas do `AudioEncoder` |
| `tests/neteq_tests.rs` | 1 | Não | Decodificação real de Opus + NetEq com senoide de 1 kHz |
| `tests/receiver_tests.rs` | 1 | Não | Criação e parada do `AudioReceiver` com ring real |
| `tests/stream_tests.rs` | 4 | Não | `fill_underrun` em leitura parcial, completa, vazia e `filled` acima do tamanho |
| `tests/hardware_tests.rs` | 1 (`ignored`) | Sim | Captura do microfone, contagem de amostras e resposta à voz |

Total padrão do `cargo test`: **9 testes de lógica** + 1 ignorado.

## Comandos

```bash
# Toda a suíte; hardware fica ignored
cargo test

# Um arquivo específico
cargo test --test codec_tests
cargo test --test neteq_tests
cargo test --test receiver_tests
cargo test --test stream_tests

# Teste manual de hardware (interativo)
cargo test --test hardware_tests -- --ignored --nocapture

# Ver a saída dos testes de NetEq
cargo test --test neteq_tests -- --nocapture
```

## `codec_tests.rs`

### `encoder_discards_warmup_and_emits_one_packet_per_frame`

- Gera uma senoide de 440 Hz com 1 segundo de warmup + 10 frames de 960 samples.
- Escreve tudo no ring, chama `process()`.
- Espera 10 pacotes, todos não vazios e com no máximo 4000 bytes.

Prova que o warmup é descartado e que cada frame completo gera um pacote.

### `encoder_returns_nothing_before_a_full_frame`

- Escreve warmup + 959 amostras.
- `process()` deve devolver vazio.
- Escreve mais 1 amostra.
- `process()` deve devolver 1 pacote.

Prova que o encoder não emite frame incompleto e que o acumulador preserva o resto.

### `encoder_keeps_remainder_between_rounds`

- Escreve warmup + 1 frame + meio frame.
- Primeira chamada devolve 1 pacote.
- Escreve o outro meio frame.
- Segunda chamada devolve mais 1 pacote.

Prova que o resto entre chamadas é mantido.

## `neteq_tests.rs`

### `neteq_decodes_1khz_sine_with_correlation`

Este é o teste mais importante da recepção. Ele prova que o adaptador `NetEqOpusDecoder` decodifica Opus de verdade, e não um stub.

Fluxo:

1. Gera 1 segundo de warmup + 100 frames de 20 ms de uma senoide de **1 kHz**.
2. Codifica com `AudioEncoder`.
3. Guarda a referência: o sinal de entrada depois do warmup.
4. Cria um `JitterEngine`.
5. Para cada pacote, monta um `AudioPacket` com:
   - sequence number `i`;
   - timestamp `i * 960`;
   - SSRC 1;
   - payload type 111;
   - `marker = true` no primeiro;
   - payload Opus.
6. Insere no engine e chama `mix_frame` duas vezes por pacote, acumulando 2 segundos de saída.
7. Ignora o primeiro segundo da saída.
8. Pega uma janela de 0,5 s e procura o atraso (lag) entre 0 e 250 ms que maximiza a correlação normalizada com a referência.
9. Verifica:
   - RMS da saída > 0,01 (não é silêncio);
   - correlação > 0,7 (a saída corresponde ao sinal de 1 kHz).

Por que 1 kHz e não 440 Hz: o teste precisa distinguir o sinal real de um tom de teste ou de silêncio. A senoide de 1 kHz é bem diferente de um tom de 440 Hz e tem correlação baixa com ele. O teste falhou com o stub antigo (que devolvia silêncio) e passa com o decoder real.

O teste não usa hardware e roda no CI. Ele é mais lento que os outros porque codifica/decodifica 2 segundos de áudio.

## `receiver_tests.rs`

### `spawn_starts_and_stops_without_hardware`

- Cria um ring de `FRAME_SIZE * 4` (80 ms).
- Chama `AudioReceiver::spawn(playback_producer)`.
- Envia `leave(1)` pelo handle.
- Dropa o receiver, que sinaliza parada e faz `join`.

Prova que a thread pode ser criada e encerrada sem hardware. **Não** prova que o áudio é reproduzido; isso é coberto pelo teste de NetEq e pelo teste manual de hardware.

## `stream_tests.rs`

Testa `fill_underrun` sem `cpal`:

1. **Leitura parcial:** 3 amostras preenchidas, 5 viram zero.
2. **Leitura completa:** nada muda.
3. **Leitura vazia:** tudo vira zero.
4. **`filled` acima do tamanho:** é limitado ao tamanho; nada muda.

Esses testes garantem que underrun não deixa lixo antigo no buffer de saída.

## `hardware_tests.rs`

### `mic_samples_and_voice_are_received`

Teste manual e `ignored`. Requer microfone e interação humana.

Fases:

1. **Transiente:** 5 janelas de 100 ms.
2. **Silêncio:** pede 1,5 s de silêncio; mede 15 janelas.
3. **Voz:** pede 3 s de fala contínua; mede 30 janelas.

Para cada janela calcula peak e RMS. No final:

- mediana do RMS de silêncio;
- máximo do RMS de voz;
- total de amostras recebidas.

Asserções:

- A contagem de amostras deve ficar entre 90% e 110% do esperado pela duração.
- A voz deve ter RMS > 0,005 e ser pelo menos 5× maior que o ruído de fundo.

Ele usa `println!` fora do callback; o callback de captura só escreve no ring. O teste não usa saída de áudio.

Para rodar:

```bash
cargo test --test hardware_tests -- --ignored --nocapture
```

## Como adicionar um teste

- Lógica pura: crie `tests/<nome>_tests.rs` e use apenas APIs públicas.
- Não dependa de placa de som em testes de lógica.
- Se precisar de hardware, marque `#[ignore]` e documente o comando.
- Para testar o encoder, use um ring real e escreva amostras suficientes para o warmup.
- Para testar o NetEq, monte `AudioPacket`s com timestamp e sequence number coerentes.
- Para testar underrun, use `fill_underrun` diretamente.

## Limitações da suíte

- Não há teste de integração completo captura → encoder → NetEq → reprodução com hardware.
- Não há teste de múltiplos participantes no `JitterEngine`.
- Não há teste de fila cheia (`push_packet` retornando `false`).
- Não há teste de perda/reordenação de pacotes no NetEq.
- Não há teste de RTP/SDP/transporte, porque essa camada ainda não existe.
- O teste de hardware depende do ambiente e não é determinístico.
