// tests/contract/ cross-language proto round-trip helper (C++ side).
// `roundtrip`: reads an Envelope from stdin, decodes it, re-encodes it to stdout.
#include <iostream>
#include <sstream>
#include <string>

#include "neuroos/v1/envelope.pb.h"

int main(int argc, char** argv) {
    if (argc != 2 || std::string(argv[1]) != "roundtrip") {
        std::cerr << "usage: cpp_codec roundtrip (reads Envelope bytes on stdin)\n";
        return 2;
    }

    std::ostringstream buf;
    buf << std::cin.rdbuf();
    const std::string input = buf.str();

    neuroos::v1::Envelope env;
    if (!env.ParseFromString(input)) {
        std::cerr << "failed to parse Envelope\n";
        return 1;
    }

    std::string out;
    if (!env.SerializeToString(&out)) {
        std::cerr << "failed to serialize Envelope\n";
        return 1;
    }

    std::cout.write(out.data(), static_cast<std::streamsize>(out.size()));
    return 0;
}
