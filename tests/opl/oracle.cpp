// Copyright (c) 2026, Alban Gicquel
// SPDX-License-Identifier: BSD-3-Clause
//
// Renders a register log through ymfm, the emulator this crate translates, so
// the translation can be held to it sample for sample.
//
// This is a development tool and never ships: Cargo does not build it, and
// nothing in the crate links ymfm. It regenerates the golden renders in this
// directory, which are what the tests compare against.
//
// A log is one write per line, `sample register value`, with the sample in
// decimal and the register and value in hexadecimal; `#` starts a comment.
// Each write lands before the sample it names is generated, in file order,
// which is the order the Rust side applies a schedule in. The output is raw
// little-endian int16 at the chip's own rate: one channel for the OPL2, and
// the OPL3's outputs A and B interleaved.
//
// Build it against a ymfm checkout (BSD-3-Clause, by Aaron Giles) at the
// commit the crate names (81aec25), then render every log:
//
//     YMFM=/path/to/ymfm
//     c++ -std=c++17 -O2 -I"$YMFM/src" oracle.cpp "$YMFM/src/ymfm_opl.cpp" \
//         "$YMFM/src/ymfm_adpcm.cpp" "$YMFM/src/ymfm_pcm.cpp" -o oracle
//     for log in *.log; do ./oracle "$log" > "${log%.log}.golden"; done
//
// The chip and the length come from the log's first line, `# chip samples`.

#include "ymfm_opl.h"

#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>

namespace {

struct Interface : ymfm::ymfm_interface {};

struct Write {
    unsigned long sample;
    unsigned reg;
    unsigned value;
};

// Reads the next write, skipping blank lines and comments.
bool next(FILE *in, Write &write) {
    char line[256];
    while (fgets(line, sizeof(line), in) != nullptr) {
        char *text = line;
        while (*text == ' ' || *text == '\t') {
            ++text;
        }
        if (*text == '#' || *text == '\n' || *text == '\0') {
            continue;
        }
        if (sscanf(text, "%lu %x %x", &write.sample, &write.reg, &write.value) == 3) {
            return true;
        }
        fprintf(stderr, "oracle: cannot read the line: %s", line);
        exit(2);
    }
    return false;
}

void emit(int32_t value) {
    int16_t sample = int16_t(value);
    uint8_t bytes[2] = {uint8_t(sample & 0xff), uint8_t((sample >> 8) & 0xff)};
    fwrite(bytes, 1, 2, stdout);
}

template <typename Chip>
void render(FILE *in, unsigned long total, bool stereo) {
    Interface intf;
    Chip chip(intf);
    chip.reset();
    typename Chip::output_data out;
    Write write;
    bool have = next(in, write);
    for (unsigned long now = 0; now < total; ++now) {
        while (have && write.sample <= now) {
            if (write.reg & 0x100) {
                chip.write(2, uint8_t(write.reg));
                chip.write(3, uint8_t(write.value));
            } else {
                chip.write(0, uint8_t(write.reg));
                chip.write(1, uint8_t(write.value));
            }
            have = next(in, write);
        }
        chip.generate(&out, 1);
        emit(out.data[0]);
        if (stereo) {
            emit(out.data[1 % Chip::OUTPUTS]);
        }
    }
}

}  // namespace

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: oracle <log>\n");
        return 2;
    }
    FILE *in = fopen(argv[1], "r");
    if (in == nullptr) {
        fprintf(stderr, "oracle: cannot open %s\n", argv[1]);
        return 2;
    }
    char chip[16] = {0};
    unsigned long total = 0;
    if (fscanf(in, "# %15s %lu", chip, &total) != 2) {
        fprintf(stderr, "oracle: %s does not start with '# chip samples'\n", argv[1]);
        return 2;
    }
    if (strcmp(chip, "opl2") == 0) {
        render<ymfm::ym3812>(in, total, false);
    } else if (strcmp(chip, "opl3") == 0) {
        render<ymfm::ymf262>(in, total, true);
    } else {
        fprintf(stderr, "oracle: unknown chip %s\n", chip);
        return 2;
    }
    fclose(in);
    return 0;
}
