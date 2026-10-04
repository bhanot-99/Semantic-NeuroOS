// GBNF grammar-constrained decoding (PRD FR-INF-04, P2-S05): thin wrapper
// over llama.cpp's own grammar sampler (llama-grammar.cpp, part of the core
// `llama` library, not `common/` — no separate GBNF parser needed here).
#pragma once

#include <string>

#include "llama.h"

namespace neuroos::inference {

// Returns nullptr if `grammar_gbnf` fails to parse (matches
// llama_sampler_init_grammar's own contract). The caller adds the returned
// sampler to a chain (llama_sampler_chain_add), which takes ownership.
llama_sampler* build_grammar_sampler(const llama_model* model, const std::string& grammar_gbnf);

} // namespace neuroos::inference
