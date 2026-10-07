// Minimal `Expected<T, E>` (rules.md: "C++ errors: tl::expected<T, Error>
// returns; exceptions across module/ABI boundaries" is banned). The real
// tl::expected (github.com/TartanLlama/expected) isn't vendored — this dev
// environment has no outbound network access to fetch it — so this is a
// small hand-written equivalent covering exactly what this codebase needs:
// construct from a value or an error, check which, and read it back. It is
// not a drop-in replacement for the full library (no monadic `and_then` /
// `map`, no `void` specialization), but the call sites in this codebase
// don't need those.
#pragma once

#include <optional>
#include <utility>
#include <variant>

namespace neuroos {

template <typename E> class Unexpected {
  public:
    explicit Unexpected(E error) : error_(std::move(error)) {}
    const E& error() const& {
        return error_;
    }
    E&& error() && {
        return std::move(error_);
    }

  private:
    E error_;
};

template <typename E> Unexpected<E> make_unexpected(E error) {
    return Unexpected<E>(std::move(error));
}

template <typename T, typename E> class Expected {
  public:
    Expected(T value)
        : storage_(std::in_place_index<0>, std::move(value)) {} // NOLINT(*-explicit-constructor)
    Expected(Unexpected<E> err)                                 // NOLINT(*-explicit-constructor)
        : storage_(std::in_place_index<1>, std::move(err).error()) {}

    bool has_value() const {
        return storage_.index() == 0;
    }
    explicit operator bool() const {
        return has_value();
    }

    const T& value() const& {
        return std::get<0>(storage_);
    }
    T& value() & {
        return std::get<0>(storage_);
    }
    T&& value() && {
        return std::move(std::get<0>(storage_));
    }

    const E& error() const& {
        return std::get<1>(storage_);
    }
    E&& error() && {
        return std::move(std::get<1>(storage_));
    }

    const T& operator*() const& {
        return value();
    }
    T& operator*() & {
        return value();
    }

  private:
    std::variant<T, E> storage_;
};

// `Expected<void, E>` specialization: success carries no value, only the
// possibility of `E`.
template <typename E> class Expected<void, E> {
  public:
    Expected() : error_(std::nullopt) {}
    Expected(Unexpected<E> err)
        : error_(std::move(err).error()) {} // NOLINT(*-explicit-constructor)

    bool has_value() const {
        return !error_.has_value();
    }
    explicit operator bool() const {
        return has_value();
    }

    // Precondition: `!has_value()`. Mirrors std::expected::error(), which is
    // likewise undefined on a value-carrying expected.
    const E& error() const& {
        return *error_; // NOLINT(bugprone-unchecked-optional-access)
    }
    E&& error() && {
        return std::move(*error_); // NOLINT(bugprone-unchecked-optional-access)
    }

  private:
    std::optional<E> error_;
};

} // namespace neuroos
