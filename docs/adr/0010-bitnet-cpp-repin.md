# ADR-0010: Repin bitnet.cpp to 01eb415 (pre-July-2026 llama.cpp fork)

- Status: Accepted (owner chose this option on 2026-10-03, among: repin / debug the new fork / switch to upstream llama.cpp TQ2_0)
- Date: 2026-10-04
- Supersedes: the bitnet.cpp pin recorded in ADR-0006 (P0-S08 spike)

## Context

KPI-1 (BUG-007) showed C4 answering with repeated-token loops and run-on
text even after the chat template, sampling and stale-logits fixes. Isolating
C4 from the rest of the stack showed that the inference itself computes wrong
results.

C4 was built against bitnet.cpp `0b341e5` (2026-07-27). Its nested llama.cpp is
`isHuangXin/llama.cpp`, branch `release-bitnet-embedding-0.6b-270m`
(`390c307`): a July 2026 re-port of BitNet onto upstream llama.cpp b9918.

Measured on this machine, same GGUF (`microsoft/BitNet-b1.58-2B-4T-gguf`,
`ggml-model-i2_s.gguf`), same Wikipedia paragraph:

| Build | Perplexity |
| :--- | :--- |
| HF `microsoft/bitnet-b1.58-2B-4T-bf16`, transformers (reference) | 3.74 |
| bitnet.cpp `01eb415` (2026-03-10, llama.cpp b3639 fork), `llama-perplexity -c 128` | 9.5 |
| bitnet.cpp `0b341e5` (pinned), `llama-perplexity -c 128` | 58.2 |

The `0b341e5` result was identical across ubatch 1/4/32, 1 or 8 threads, and
GCC vs the official clang-18 build: a deterministic computation error, not a
race or a build-configuration problem. GGUF hyperparameters match the HF config.

On `01eb415` the same model answers correctly and ends its turn with
`<|eot_id|>`, matching the HF reference output. ADR-0006's spike and
ADR-0008's benchmark were both measured on the broken fork.

## Decision

Pin `cpp/third_party/bitnet.cpp` to `01eb415`, the last commit before the
re-port, with its own nested llama.cpp (`Eddie-Wang1120/llama.cpp`,
`merge-dev`, `1f86f05`).

- **C4 ported to that llama.cpp's API:**
  - model-based tokenize/detokenize/grammar calls instead of `llama_vocab`;
  - `llama_kv_cache_seq_rm`;
  - explicit positions in `llama_batch_get_one`;
  - the 9-argument `llama_sampler_init_penalties`.
- **No separate `llama_sampler_accept`:** in this API, `llama_sampler_sample`
  already accepts the token. Calling accept again fed the grammar every token
  twice and tripped its `GGML_ASSERT`.
- **Pre-tokenizer override at model load:** `tokenizer.ggml.pre = llama-bpe`.
  The GGUF lacks it, so llama.cpp fell back to the GPT-2 pre-tokenizer and
  logged "GENERATION QUALITY WILL BE DEGRADED".
- **Build integration** (`cpp/neuroos-inference/CMakeLists.txt`):
  - This fork's ggml lists `include/bitnet-lut-kernels.h`, which BitNet's
    `setup_env.py` normally generates. Its whole body is under
    `GGML_BITNET_X86_TL2`, so CMake writes an empty placeholder (exact for i2_s).
    `.gitmodules` sets `ignore = untracked` for that generated file.
  - `-fpermissive` for ggml under GCC: the same flag bitnet.cpp's own top-level
    CMake adds, for one const-dropping pointer init in `ggml-bitnet-mad.cpp`.
- **Prompt** (C5): BitNet's trained format from the HF `tokenizer_config.json`,
  `System: …<|eot_id|>User: …<|eot_id|>Assistant: `. The GGUF's
  `Human:/BITNETAssistant:` template is a placeholder hardcoded by
  `convert-ms-to-gguf-bitnet.py`, and under it the model never stops.

## Consequences

- C4 output is correct. `tests/contract/inference_smoke.sh` passes in full,
  including GBNF-constrained output and preemption.
- Decode speed: the 2026-10-04 spot check with `llama-cli` measured 44.7 ms/token
  (FR-INF-07 target 45 ms/token). ADR-0008's 56–72 ms/token figure is
  superseded; the full benchmark re-run is recorded below.
- The pinned fork is older (llama.cpp b3639 era). Model formats and features
  newer than that are unavailable until a correct newer BitNet port exists.
  **Re-verify perplexity before any future repin**: one perplexity number would
  have caught this regression.
- **Phase 6 build note:** whisper.cpp v1.9.4 brings its own, newer ggml. Both
  ggml copies cannot be `add_subdirectory`'d into one CMake project (duplicate
  `ggml` targets), nor linked into one process (duplicate C symbols). C2 and C4
  are separate executables, so build whisper.cpp for C2 as its own CMake
  project (ExternalProject or a separate build tree) and link it only into
  `neuroos-voice`.

## Benchmark (2026-10-04, `neuroos-inference --bench`, after repin)

| n_ctx | threads | prompt tokens | TTFT | prefill ms/tok | decode ms/tok | RSS |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| 128 | 8 | 71 | 368 ms | 5.19 | 42.85 | 1,218 MiB |
| 512 | 8 | 451 | 2,953 ms | 6.55 | 47.06 | 1,289 MiB |
| 1,024 | 8 | 961 | 7,277 ms | 7.57 | 50.27 | 1,351 MiB |
| 128 | 4 | 71 | 634 ms | 8.92 | 36.88 | 1,218 MiB |
| 512 | 4 | 451 | 5,014 ms | 11.12 | 40.90 | 1,289 MiB |
| 1,024 | 4 | 961 | 12,204 ms | 12.70 | 44.11 | 1,351 MiB |

Versus ADR-0008, on the broken fork:
- **Decode** improves from 56–72 to 43–50 ms/token at 8 threads, and 37–44 ms/token at 4 threads, meeting FR-INF-07's 45 ms target.
- **Prefill** improves from 8.3–8.9 to 5.2–7.6 ms/token, still above its 2.5 ms target.

From the bench log; the JSON wasn't written because of BUG-008, a 1-thread segfault in the bench's thread sweep.
