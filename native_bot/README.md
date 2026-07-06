# native_bot

A **built-in, libriichi-free** mahjong bot for Akagi. It runs a small
behavior-cloned convolutional net entirely in Rust (via [candle]) — no Python,
no libriichi, no subprocess. Two variants ship: 4-player (yonma) and 3-player
(sanma).

The bot is intentionally only "default strength": it imitates human play from
Tenhou logs with a compact model. It exists so Akagi has a sane, zero-install
default bot, not to be state of the art.

## How it fits together

```
              (training, offline)                       (runtime, in Akagi)
  dataset/*.json.gz ─► extract (Rust) ─► shards ─► train.py ─► *.safetensors
                          │                                        │
                          └──────── obs.rs / action_codec ─────────┘  (shared code = parity)
                                                                     │
   mjai events ─► Engine::feed ─► get_observation ─► obs.rs ─► candle Model ─► legal Action
```

The **same** Rust observation encoder ([`obs`]) and action codec
([`action_codec`]) are used to build training samples and to run inference, so
the model always sees identical features — parity is guaranteed by
construction, not by re-implementing the encoding twice.

## Modules

| Module | Role |
|---|---|
| `tiles` | tile id ↔ tile34 / sanma compact-27, red-five + dora helpers |
| `obs` | our own observation feature encoder (`EncInput` → `[C, T]` planes) |
| `adapt` | build an `EncInput` from a riichienv-core observation (shared by extractor + inference) |
| `action_codec` | action indexing, legal masking, logit → legal-action decoding (reuses riichienv-core action ids) |
| `replay` (`extract` feature) | drive an mjai game log through the engine, emit `(obs, action, mask)` samples incl. synthesized `Pass` |
| `model` (`infer` feature) | candle CNN: load `.safetensors`, forward → logits |
| `engine` (`infer` feature) | live inference: feed events → pick a legal action (handles riichi two-step) |
| `defaults` (`infer` feature) | embedded default weights + `engine(num_players, seat)` |
| `bin/extract` (`extract` feature) | the offline training-data extractor |

The engine is deliberately decoupled from Akagi: it consumes
`riichienv_core::replay::MjaiEvent` and returns a schema-agnostic
`engine::BotAction`. Akagi's `src/bot/native.rs` maps that to Akagi's own
`MjaiEvent`.

## Geometry

| | 4-player | 3-player |
|---|---|---|
| tile axis `T` | 34 | 27 (2m–8m removed) |
| obs channels `C` | 39 | 37 (adds per-player kita) |
| action space `A` | 82 | 60 |

Feature planes are channel-major (`buf[ch*T + tile]`) and fully self-relative
(index 0 = the deciding seat). See the doc comment in `src/obs.rs` for the
exact channel layout.

## Retraining / extending

See [`train/README.md`](train/README.md) for the end-to-end extract → train →
export flow.

**To change the observation features**: edit `src/obs.rs` (`channels()` and
`EncInput::encode`, keeping the `debug_assert_eq!(ch, c)` cursor honest) and the
adapters in `src/adapt.rs`. Because the encoder is shared, re-extract and
re-train after any change — old weights will not match a new layout.

**To change the model size/shape**: keep `CONV`/`BLOCKS`/`FC` in `src/model.rs`
in lock-step with the same constants in `train/train.py`. The safetensors key
names (`conv_in`, `res.{i}.conv{1,2}`, `fc`, `head`) are the contract between
the two.

## Building

```sh
# core codec + inference (default features = infer)
cargo test

# the offline extractor (no candle needed)
cargo run --release --no-default-features --features extract --bin extract -- \
    <dataset_dir> <num_players> <out_dir> [max_games] [max_samples]
```

`riichienv-core` (Apache-2.0) provides the pure-Rust game engine, legal-action
enumeration, and canonical action ids. This crate is part of Akagi and inherits
its licence.

[candle]: https://github.com/huggingface/candle
