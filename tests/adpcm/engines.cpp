// Copyright (c) 2026, Alban Gicquel
// SPDX-License-Identifier: BSD-3-Clause
//
// The engine oracle: drives upstream ymfm's ADPCM-A or ADPCM-B engine on its
// own, outside any chip, and writes what it produced, so the translation in
// src/adpcm.rs can be held to it sample for sample where no chip the main
// oracle (oracle/oracle.cpp) drives reaches: ADPCM-A at all until the OPN
// family is ported, and ADPCM-B with a fixed address shift, the right pan,
// the limit register and the register-8 read path, which the Y8950 does not
// expose.
//
// Like the main oracle this is an offline development tool: Cargo never
// builds it and it is not part of the package. tests/adpcm.rs uses it through
// the YMFM_ENGINE_ORACLE environment variable. Build it against an upstream
// checkout at the commit the README names:
//
//     YMFM=/path/to/ymfm
//     c++ -std=c++17 -O2 -I"$YMFM/src" tests/adpcm/engines.cpp \
//         "$YMFM"/src/ymfm_adpcm.cpp -o engines
//
// The log is text, `#` comments, a header then commands:
//
//     engine a|b        which engine
//     shift <n>         the constructor's address shift, in decimal
//     rshift <n>        ADPCM-B only: the output's right shift, in decimal
//     samples <count>   how many samples to render
//     mem <file>        the external memory image, relative to the log
//
//     <sample> w <reg> <data> [<reg> <data> ...]   engine.write(reg, data)
//     <sample> r <reg>                             engine.read(reg), recorded
//     <sample> m <mask>                            ADPCM-A: the channel mask
//                                                  for clock() and output()
//
// Registers, data and masks are hexadecimal. Every sample, the commands for
// it are applied, then the engine is clocked (ADPCM-A with the mask, all
// channels by default) and its output taken into a cleared stereo output.
//
// The output, little-endian, on standard output: per sample, the two outputs
// as int32 and one byte (ADPCM-A: the mask clock() returned; ADPCM-B: the
// status); then a uint32 count of events and, for each, uint32 sample, uint8
// kind ('r' a read's result, 'x' an external write), uint32 address (the
// register, for a read) and uint8 value.

#include "ymfm_adpcm.h"

#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <vector>

namespace {

[[noreturn]] void fail(const char *message, const char *detail = "") {
    fprintf(stderr, "engines: %s%s\n", message, detail);
    exit(2);
}

struct Event {
    uint32_t sample;
    uint8_t kind;
    uint32_t address;
    uint8_t value;
};

struct Host : ymfm::ymfm_interface {
    std::vector<uint8_t> memory;
    std::vector<Event> events;
    uint32_t sample = 0;

    uint8_t ymfm_external_read(ymfm::access_class, uint32_t address) override {
        return address < memory.size() ? memory[address] : 0;
    }
    void ymfm_external_write(ymfm::access_class, uint32_t address, uint8_t data) override {
        if (address < (1u << 24)) {
            if (address >= memory.size()) {
                memory.resize(address + 1, 0);
            }
            memory[address] = data;
        }
        events.push_back({sample, uint8_t('x'), address, data});
    }
};

struct Command {
    uint32_t sample;
    char kind;
    std::vector<uint32_t> values;
};

void put(const void *data, size_t size) { fwrite(data, 1, size, stdout); }

void put32(uint32_t value) {
    uint8_t bytes[4] = {uint8_t(value), uint8_t(value >> 8), uint8_t(value >> 16), uint8_t(value >> 24)};
    put(bytes, 4);
}

}  // namespace

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: engines <log>\n");
        return 2;
    }
    FILE *in = fopen(argv[1], "r");
    if (in == nullptr) {
        fail("cannot open ", argv[1]);
    }
    std::string directory(argv[1]);
    size_t slash = directory.rfind('/');
    directory = slash == std::string::npos ? std::string() : directory.substr(0, slash + 1);

    Host host;
    char engine = 0;
    uint32_t shift = 0, rshift = 0, samples = 0;
    std::vector<Command> commands;
    char line[4096];
    while (fgets(line, sizeof(line), in) != nullptr) {
        char *hash = strchr(line, '#');
        if (hash != nullptr) {
            *hash = '\0';
        }
        std::vector<char *> words;
        for (char *word = strtok(line, " \t\r\n"); word != nullptr; word = strtok(nullptr, " \t\r\n")) {
            words.push_back(word);
        }
        if (words.empty()) {
            continue;
        }
        if (strcmp(words[0], "engine") == 0 && words.size() == 2) {
            engine = words[1][0];
        } else if (strcmp(words[0], "shift") == 0 && words.size() == 2) {
            shift = uint32_t(strtoul(words[1], nullptr, 10));
        } else if (strcmp(words[0], "rshift") == 0 && words.size() == 2) {
            rshift = uint32_t(strtoul(words[1], nullptr, 10));
        } else if (strcmp(words[0], "samples") == 0 && words.size() == 2) {
            samples = uint32_t(strtoul(words[1], nullptr, 10));
        } else if (strcmp(words[0], "mem") == 0 && words.size() == 2) {
            std::string path = words[1][0] == '/' ? std::string(words[1]) : directory + words[1];
            FILE *image = fopen(path.c_str(), "rb");
            if (image == nullptr) {
                fail("cannot open the memory image ", path.c_str());
            }
            uint8_t buffer[65536];
            size_t got;
            while ((got = fread(buffer, 1, sizeof(buffer), image)) > 0) {
                host.memory.insert(host.memory.end(), buffer, buffer + got);
            }
            fclose(image);
        } else if (words[0][0] >= '0' && words[0][0] <= '9' && words.size() >= 3) {
            Command command;
            command.sample = uint32_t(strtoul(words[0], nullptr, 10));
            command.kind = words[1][0];
            for (size_t index = 2; index < words.size(); ++index) {
                command.values.push_back(uint32_t(strtoul(words[index], nullptr, 16)));
            }
            commands.push_back(command);
        } else {
            fail("cannot read the line starting ", words[0]);
        }
    }
    fclose(in);
    if (engine != 'a' && engine != 'b') {
        fail("the log needs an engine, a or b");
    }

    ymfm::adpcm_a_engine a(host, shift);
    ymfm::adpcm_b_engine b(host, shift);
    a.reset();
    b.reset();
    uint32_t mask = 0x3f;
    size_t next = 0;
    for (uint32_t sample = 0; sample < samples; ++sample) {
        host.sample = sample;
        while (next < commands.size() && commands[next].sample <= sample) {
            const Command &command = commands[next++];
            if (command.kind == 'w') {
                for (size_t index = 0; index + 1 < command.values.size(); index += 2) {
                    if (engine == 'a') {
                        a.write(command.values[index], uint8_t(command.values[index + 1]));
                    } else {
                        b.write(command.values[index], uint8_t(command.values[index + 1]));
                    }
                }
            } else if (command.kind == 'r' && engine == 'b') {
                uint8_t value = uint8_t(b.read(command.values[0]));
                host.events.push_back({sample, uint8_t('r'), command.values[0], value});
            } else if (command.kind == 'm' && engine == 'a') {
                mask = command.values[0];
            } else {
                fail("cannot apply a command at sample ", std::to_string(sample).c_str());
            }
        }
        ymfm::ymfm_output<2> output;
        output.clear();
        uint8_t flags;
        if (engine == 'a') {
            flags = uint8_t(a.clock(mask));
            a.output(output, mask);
        } else {
            b.clock();
            b.output(output, rshift);
            flags = b.status();
        }
        put32(uint32_t(output.data[0]));
        put32(uint32_t(output.data[1]));
        put(&flags, 1);
    }
    put32(uint32_t(host.events.size()));
    for (const Event &event : host.events) {
        put32(event.sample);
        put(&event.kind, 1);
        put32(event.address);
        put(&event.value, 1);
    }
    return 0;
}
