#include "grammar.hpp"

namespace neuroos::inference {

llama_sampler* build_grammar_sampler(const llama_model* model, const std::string& grammar_gbnf) {
    // "root" is the conventional GBNF start symbol name (llama.cpp's own
    // grammar examples and skill-call JSON schemas both use it); C6's skill
    // manifests (Phase 7) will need to follow the same convention.
    return llama_sampler_init_grammar(model, grammar_gbnf.c_str(), "root");
}

} // namespace neuroos::inference
