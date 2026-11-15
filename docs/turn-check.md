# Timing real Chat turns

`one_turn` (in `crates/agents/examples`) times real Chat turns against a running engine. It needs the bundled services and one installed model, so it is not part of the test suite.

It prints the model load on its own first (a 1-token stream, cold), then for each turn the time to the first text, the whole turn, the tokens and tokens per second (warm). Tokens are the server's streamed chunks, about one token each.

## One command

Set `REPOS` to the directory that holds `isonomic-capsule`, `isonomic-sdk`, `isonomic-ui` and `isonomic-inference` side by side, and `CAPSULE` to a scratch Capsule directory (never your own). The model is Qwen3.5 0.8B (0.5 GB); change `MODEL` for another id from the model library.

The engine and the example are two builds: `-p agents --example one_turn` alone does not build the engine.

```sh
REPOS=~/Code/isonomic/repos CAPSULE=/tmp/turn-check MODEL=qwen3.5-0.8b-q4-k-m TURNS=3 TRIPLE=x86_64-apple-darwin; \
cd "$REPOS/isonomic-capsule" && git checkout -q main && git pull -q \
&& for r in sdk:SDK_REF ui:UI_REF inference:INFERENCE_REF; do git -C "../isonomic-${r%%:*}" fetch -q && git -C "../isonomic-${r%%:*}" checkout -q "$(cat ${r##*:})" || exit 1; done \
&& cargo xtask fetch-sidecars --target="$TRIPLE" \
&& (cd ../isonomic-inference && cargo xtask fetch "$MODEL" --dir "$CAPSULE/models") \
&& cargo build --release -q --bin isonomic-capsule \
&& cargo build --release -q -p agents --example one_turn \
&& (ISONOMIC_CAPSULE_DIR="$CAPSULE" ISONOMIC_BUNDLE_DIR="$PWD/sidecars" ./target/release/isonomic-capsule > "$CAPSULE-engine.log" 2>&1 &) \
&& for i in $(seq 1 120); do [ -S "$CAPSULE/run/engine.sock" ] && break; sleep 1; done \
&& ISONOMIC_CAPSULE_DIR="$CAPSULE" ./target/release/examples/one_turn --turns "$TURNS" "Plan one day in Lisbon in three short lines."; \
pkill -TERM -f "target/release/isonomic-capsule"; echo "--- engine log"; grep -E "hardware profile|llama-server|cold start|locale|error|failed" "$CAPSULE-engine.log" | tail -20
```

On an Apple-silicon Mac, set `TRIPLE=aarch64-apple-darwin`.

## What to read

- `model load (and one token)`: the cold start of the model's worker.
- `turn N: first text …, total …, N tokens, … tok/s`: warm turns. Turn 1 runs on the model the load step started.
- The engine log lines: the profile, where the model server was found, the locale, and any error.

## Results so far

| Machine | Model | Load | First text (warm) | Speed | Notes |
| --- | --- | --- | --- | --- | --- |
| 2020 Intel MacBook Pro (i7-8850H, 16 GB) | Qwen3.5 0.8B | — | 11.0 s (cold, one turn) | — | First run, before the load was timed apart; 11.7 s for the turn. |
| 2020 Intel MacBook Pro (i7-8850H, 16 GB), lightly loaded | Qwen3.5 2B | 5.3 s | 1.4–2.0 s | 7.7–16.1 tok/s | Three turns; real places in every reply. Engine cold start 199 ms. |
| Linux CI-class Xeon, 4 cores | Qwen3.5 0.8B | 5.1 s | 0.3–0.5 s | 23–28 tok/s | Real engine binary, real llama.cpp `b12387`, real model. |
