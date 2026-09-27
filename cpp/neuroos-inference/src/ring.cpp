#include "ring.hpp"

namespace neuroos::inference {

neuroos::shm::RingWriter RingRegistry::get_or_create(const std::string& name,
                                                     std::uint32_t capacity_slots,
                                                     std::uint32_t slot_size) {
    std::lock_guard<std::mutex> lock(mutex_);
    auto it = rings_.find(name);
    if (it != rings_.end()) {
        return it->second->writer();
    }
    auto ring = std::make_shared<neuroos::shm::Ring>(neuroos::shm::Ring::create(
        name, capacity_slots == 0 ? kDefaultRingCapacitySlots : capacity_slots,
        slot_size == 0 ? kDefaultRingSlotSize : slot_size));
    auto writer = ring->writer();
    rings_.emplace(name, std::move(ring));
    return writer;
}

int RingRegistry::fd_for(const std::string& name) const {
    std::lock_guard<std::mutex> lock(mutex_);
    auto it = rings_.find(name);
    return it == rings_.end() ? -1 : it->second->fd();
}

bool RingRegistry::exists(const std::string& name) const {
    std::lock_guard<std::mutex> lock(mutex_);
    return rings_.count(name) > 0;
}

} // namespace neuroos::inference
