// Benchmark harness (phases.md §5.1 item 7, exit criterion §5.4): TTFT,
// prefill ms/token, decode ms/token at 128/512/1024-token contexts, plus a
// thread-count sweep and RSS — against the real model, real llama_decode
// calls, not a synthetic estimate. `neuroos-inference --bench` runs this
// instead of starting the servers, writing JSON to
// reports/bench/inference-<unix-seconds>.json.
#pragma once

#include "config.hpp"

namespace neuroos::inference {

// Returns the process exit code (0 on success).
int run_benchmark(const Config& config);

} // namespace neuroos::inference
