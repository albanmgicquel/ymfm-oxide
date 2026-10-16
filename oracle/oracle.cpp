// Copyright (c) 2026, Alban Gicquel
// SPDX-License-Identifier: BSD-3-Clause
//
// The oracle: replays a chip-tagged register log through upstream ymfm (the
// BSD-3-Clause C++ library this crate translates) and writes what the chip
// produced, so the Rust translation can be held to it sample for sample.
//
// This is an offline development tool. Cargo never builds it, nothing in the
// crate links ymfm, and it is not part of the package. It renders the golden
// files under tests/ and serves the randomized differential tests, which
// find it through the YMFM_ORACLE environment variable.
//
// Building
// --------
// Against a checkout of ymfm at the commit the crate names in its README:
//
//     YMFM=/path/to/ymfm
//     c++ -std=c++17 -O2 -I"$YMFM/src" oracle/oracle.cpp \
//         "$YMFM"/src/ymfm_adpcm.cpp "$YMFM"/src/ymfm_misc.cpp \
//         "$YMFM"/src/ymfm_opl.cpp "$YMFM"/src/ymfm_opm.cpp \
//         "$YMFM"/src/ymfm_opn.cpp "$YMFM"/src/ymfm_opq.cpp \
//         "$YMFM"/src/ymfm_opz.cpp "$YMFM"/src/ymfm_pcm.cpp \
//         "$YMFM"/src/ymfm_ssg.cpp -o oracle/oracle
//
// and render a log with `oracle/oracle <log> > <golden>`.
//
// The log
// -------
// Text, one item per line; `#` starts a comment, and blank lines are skipped.
// A header comes first:
//
//     chip <name>          the ymfm class, e.g. ym2151, ym3812, ymf262
//     clock <hz>           the input clock, in decimal
//     samples <count>      how many output samples to render
//     mem <class> <file>   optional, repeatable: loads a raw file as the
//                          external memory of an access class (io, adpcm-a,
//                          adpcm-b or pcm); the path is relative to the log
//
// then commands, each led by the sample it happens before, in decimal, in
// nondecreasing order:
//
//     <sample> w <offset> <data> [<offset> <data> ...]
//         calls write(offset, data) for each pair, in order; the offset is
//         the chip's port, exactly as its write() takes it, so a register
//         write is usually an address-port and a data-port pair
//     <sample> r <offset>
//         calls read(offset) and records the byte it returns
//
// Offsets and data are hexadecimal.
//
// Time
// ----
// Time is counted in input clocks from zero. Each output sample lasts
// round(clock / sample_rate(clock)) clocks, recomputed every sample since a
// prescaler write can change it. Before each sample the oracle fires every
// timer whose deadline has passed, earliest first and timer 0 first on a
// tie, with the time set to the deadline while it runs, so a timer reloads
// without drift. It then applies the commands for that sample, at the
// sample's start, and generates the sample. A timer set for d clocks expires
// d clocks after the moment it is set; the chip is busy from a write until
// the number of clocks it names has passed. External reads return the loaded
// image's byte, or 0 past its end; external writes store into the image and
// are recorded.
//
// The output
// ----------
// Little-endian binary, on standard output, in version 2 of the format:
//
//     4 bytes          the magic "YMFR"
//     uint32           the format version, 2
//     uint32           OUTPUTS, the number of outputs the chip has
//     uint32           the number of samples
//     for each sample: OUTPUTS x int32, every output the chip has, in its
//                      order, exactly as ymfm's output_data holds it; then
//                      one byte, 1 if the IRQ line is asserted after the
//                      sample and 0 if not
//     uint32           the number of events
//     for each event:  uint32 sample, uint8 kind ('r' for a read, 'x' for an
//                      external write), uint8 access class (0xff for a
//                      read), uint32 address (the offset, for a read),
//                      uint8 value
//
// ymfm's outputs are 32-bit, and some chips go past 16 bits: the YM2612 with
// every channel clipped reaches +32768, the YM3438 -32896. Version 1, the
// first, had no header and wrote each output as an int16, refusing any that
// did not fit.

#include "ymfm.h"
#include "ymfm_misc.h"
#include "ymfm_opl.h"
#include "ymfm_opm.h"
#include "ymfm_opn.h"
#include "ymfm_opq.h"
#include "ymfm_opz.h"

#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <vector>

namespace {

[[noreturn]] void fail(const char *message, const char *detail = "") {
    fprintf(stderr, "oracle: %s%s\n", message, detail);
    exit(2);
}

constexpr uint64_t NEVER = UINT64_MAX;
constexpr uint32_t MEMORY_LIMIT = 1u << 24;
constexpr uint32_t FORMAT_VERSION = 2;

struct Event {
    uint32_t sample;
    uint8_t kind;
    uint8_t access;
    uint32_t address;
    uint8_t value;
};

// The host side of ymfm_interface, with the time model described above.
struct Host : ymfm::ymfm_interface {
    uint64_t now = 0;
    uint64_t deadline[2] = {NEVER, NEVER};
    uint64_t busy_end = 0;
    bool irq = false;
    uint32_t sample = 0;
    std::vector<uint8_t> memory[ymfm::ACCESS_CLASSES];
    std::vector<Event> events;

    void ymfm_set_timer(uint32_t tnum, int32_t duration_in_clocks) override {
        deadline[tnum] = duration_in_clocks < 0 ? NEVER : now + uint64_t(duration_in_clocks);
    }
    void ymfm_set_busy_end(uint32_t clocks) override { busy_end = now + clocks; }
    bool ymfm_is_busy() override { return now < busy_end; }
    void ymfm_update_irq(bool asserted) override { irq = asserted; }
    uint8_t ymfm_external_read(ymfm::access_class type, uint32_t address) override {
        std::vector<uint8_t> &image = memory[type];
        return address < image.size() ? image[address] : 0;
    }
    void ymfm_external_write(ymfm::access_class type, uint32_t address, uint8_t data) override {
        std::vector<uint8_t> &image = memory[type];
        if (address < MEMORY_LIMIT) {
            if (address >= image.size()) {
                image.resize(address + 1, 0);
            }
            image[address] = data;
        }
        events.push_back({sample, uint8_t('x'), uint8_t(type), address, data});
    }

    // Fires every timer that is due, as described above.
    void fire_timers() {
        for (;;) {
            int which = deadline[1] < deadline[0] ? 1 : 0;
            if (deadline[which] == NEVER || deadline[which] > now) {
                return;
            }
            uint64_t saved = now;
            now = deadline[which];
            deadline[which] = NEVER;
            m_engine->engine_timer_expired(which);
            now = saved;
        }
    }
};

struct Command {
    uint32_t sample;
    bool read;
    std::vector<uint32_t> values;
};

struct Log {
    std::string chip;
    uint32_t clock = 0;
    uint32_t samples = 0;
    std::vector<Command> commands;
};

int access_class(const char *name) {
    if (strcmp(name, "io") == 0) return ymfm::ACCESS_IO;
    if (strcmp(name, "adpcm-a") == 0) return ymfm::ACCESS_ADPCM_A;
    if (strcmp(name, "adpcm-b") == 0) return ymfm::ACCESS_ADPCM_B;
    if (strcmp(name, "pcm") == 0) return ymfm::ACCESS_PCM;
    fail("unknown access class ", name);
}

void load(Host &host, const std::string &directory, const char *klass, const char *file) {
    std::string path = file[0] == '/' ? std::string(file) : directory + file;
    FILE *in = fopen(path.c_str(), "rb");
    if (in == nullptr) {
        fail("cannot open the memory image ", path.c_str());
    }
    std::vector<uint8_t> &image = host.memory[access_class(klass)];
    image.clear();
    uint8_t buffer[65536];
    size_t got;
    while ((got = fread(buffer, 1, sizeof(buffer), in)) > 0) {
        image.insert(image.end(), buffer, buffer + got);
    }
    fclose(in);
}

Log parse(const char *path, Host &host) {
    FILE *in = fopen(path, "r");
    if (in == nullptr) {
        fail("cannot open ", path);
    }
    std::string directory(path);
    size_t slash = directory.rfind('/');
    directory = slash == std::string::npos ? std::string() : directory.substr(0, slash + 1);

    Log log;
    char line[4096];
    uint32_t last = 0;
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
        if (strcmp(words[0], "chip") == 0 && words.size() == 2) {
            log.chip = words[1];
        } else if (strcmp(words[0], "clock") == 0 && words.size() == 2) {
            log.clock = uint32_t(strtoul(words[1], nullptr, 10));
        } else if (strcmp(words[0], "samples") == 0 && words.size() == 2) {
            log.samples = uint32_t(strtoul(words[1], nullptr, 10));
        } else if (strcmp(words[0], "mem") == 0 && words.size() == 3) {
            load(host, directory, words[1], words[2]);
        } else if (words[0][0] >= '0' && words[0][0] <= '9' && words.size() >= 3) {
            Command command;
            command.sample = uint32_t(strtoul(words[0], nullptr, 10));
            if (command.sample < last) {
                fail("commands go back in time at sample ", words[0]);
            }
            last = command.sample;
            if (strcmp(words[1], "w") == 0 && words.size() % 2 == 0) {
                command.read = false;
            } else if (strcmp(words[1], "r") == 0 && words.size() == 3) {
                command.read = true;
            } else {
                fail("cannot read the command at sample ", words[0]);
            }
            for (size_t index = 2; index < words.size(); ++index) {
                command.values.push_back(uint32_t(strtoul(words[index], nullptr, 16)));
            }
            log.commands.push_back(command);
        } else {
            fail("cannot read the line starting ", words[0]);
        }
    }
    fclose(in);
    if (log.chip.empty() || log.clock == 0) {
        fail("the log needs a chip and a clock");
    }
    return log;
}

void put(const void *data, size_t size) { fwrite(data, 1, size, stdout); }

void put32(uint32_t value) {
    uint8_t bytes[4] = {uint8_t(value), uint8_t(value >> 8), uint8_t(value >> 16), uint8_t(value >> 24)};
    put(bytes, 4);
}

template <typename Chip>
void render(const Log &log, Host &host) {
    Chip chip(host);
    chip.reset();
    typename Chip::output_data output;
    put("YMFR", 4);
    put32(FORMAT_VERSION);
    put32(Chip::OUTPUTS);
    put32(log.samples);
    size_t next = 0;
    for (uint32_t sample = 0; sample < log.samples; ++sample) {
        host.sample = sample;
        host.fire_timers();
        while (next < log.commands.size() && log.commands[next].sample <= sample) {
            const Command &command = log.commands[next++];
            if (command.read) {
                uint8_t value = chip.read(command.values[0]);
                host.events.push_back({sample, uint8_t('r'), 0xff, command.values[0], value});
            } else {
                for (size_t index = 0; index + 1 < command.values.size(); index += 2) {
                    chip.write(command.values[index], uint8_t(command.values[index + 1]));
                }
            }
        }
        chip.generate(&output, 1);
        for (uint32_t index = 0; index < Chip::OUTPUTS; ++index) {
            put32(uint32_t(output.data[index]));
        }
        uint8_t irq = host.irq ? 1 : 0;
        put(&irq, 1);
        uint32_t rate = chip.sample_rate(log.clock);
        if (rate == 0) {
            fail("the chip reports a sample rate of zero");
        }
        host.now += (uint64_t(log.clock) + rate / 2) / rate;
    }
    put32(uint32_t(host.events.size()));
    for (const Event &event : host.events) {
        put32(event.sample);
        put(&event.kind, 1);
        put(&event.access, 1);
        put32(event.address);
        put(&event.value, 1);
    }
}

}  // namespace

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: oracle <log>\n");
        return 2;
    }
    Host host;
    Log log = parse(argv[1], host);
    const std::string &chip = log.chip;
    if (chip == "ym3526") render<ymfm::ym3526>(log, host);
    else if (chip == "y8950") render<ymfm::y8950>(log, host);
    else if (chip == "ym3812") render<ymfm::ym3812>(log, host);
    else if (chip == "ymf262") render<ymfm::ymf262>(log, host);
    else if (chip == "ymf289b") render<ymfm::ymf289b>(log, host);
    else if (chip == "ymf278b") render<ymfm::ymf278b>(log, host);
    else if (chip == "ym2413") render<ymfm::ym2413>(log, host);
    else if (chip == "ym2423") render<ymfm::ym2423>(log, host);
    else if (chip == "ymf281") render<ymfm::ymf281>(log, host);
    else if (chip == "ds1001") render<ymfm::ds1001>(log, host);
    else if (chip == "ym2151") render<ymfm::ym2151>(log, host);
    else if (chip == "ym2164") render<ymfm::ym2164>(log, host);
    else if (chip == "ym2203") render<ymfm::ym2203>(log, host);
    else if (chip == "ym2608") render<ymfm::ym2608>(log, host);
    else if (chip == "ymf288") render<ymfm::ymf288>(log, host);
    else if (chip == "ym2610") render<ymfm::ym2610>(log, host);
    else if (chip == "ym2610b") render<ymfm::ym2610b>(log, host);
    else if (chip == "ym2612") render<ymfm::ym2612>(log, host);
    else if (chip == "ym3438") render<ymfm::ym3438>(log, host);
    else if (chip == "ymf276") render<ymfm::ymf276>(log, host);
    else if (chip == "ym2414") render<ymfm::ym2414>(log, host);
    else if (chip == "ym3806") render<ymfm::ym3806>(log, host);
    else if (chip == "ym3533") render<ymfm::ym3533>(log, host);
    else if (chip == "ym2149") render<ymfm::ym2149>(log, host);
    else fail("unknown chip ", chip.c_str());
    return 0;
}
