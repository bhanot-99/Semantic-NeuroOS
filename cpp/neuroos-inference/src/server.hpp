// inference.sock server (Architecture.md §5.2): Generate/Cancel/Distill/
// AttachRing/GetInfo request dispatch.
#pragma once

#include <cstdint>
#include <memory>
#include <string>
#include <vector>

#include "engine.hpp"
#include "lanes.hpp"
#include "ring.hpp"

namespace neuroos::inference {

// Blocks the calling thread forever (or until a fatal accept() error),
// spawning one thread per connection. Call from its own std::jthread.
// `max_context_tokens` matches every lane's Context (main.cpp creates them
// all with the same value) — used to reject an oversized prompt
// synchronously, before it's ever queued (phases.md §5.3 FI: "oversized
// prompt rejected").
void serve(const std::string& socket_path, std::vector<std::uint32_t> allowed_uids,
           std::shared_ptr<Model> model, LaneScheduler& lanes, RingRegistry& rings,
           std::uint32_t max_context_tokens);

} // namespace neuroos::inference
