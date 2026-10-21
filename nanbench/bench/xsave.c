/* Times the extended-state save and restore of the kernel runtime's seams and thread switches,
 * xsave64 and xrstor64 over every state component, in picoseconds. A guest under KVM runs both
 * instructions natively, so their native time is their time under the emulator. */

#include <stdint.h>
#include <stdio.h>
#include <time.h>

#define ROUNDS 10000000

static uint8_t area[4096] __attribute__((aligned(64)));

static uint64_t nanoseconds(void) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return (uint64_t)now.tv_sec * 1000000000 + (uint64_t)now.tv_nsec;
}

int main(void) {
    __asm__ volatile("xsave64 %0" : "=m"(area) : "a"(-1), "d"(-1) : "memory");
    uint64_t start = nanoseconds();
    for (int round = 0; round < ROUNDS; round++) {
        __asm__ volatile("xsave64 %0" : "=m"(area) : "a"(-1), "d"(-1) : "memory");
    }
    uint64_t saved = nanoseconds();
    for (int round = 0; round < ROUNDS; round++) {
        __asm__ volatile("xrstor64 %0"
                         :
                         : "m"(area), "a"(-1), "d"(-1)
                         : "memory", "xmm0", "xmm1", "xmm2", "xmm3", "xmm4", "xmm5", "xmm6",
                           "xmm7", "xmm8", "xmm9", "xmm10", "xmm11", "xmm12", "xmm13", "xmm14",
                           "xmm15", "st", "st(1)", "st(2)", "st(3)", "st(4)", "st(5)", "st(6)",
                           "st(7)");
    }
    uint64_t restored = nanoseconds();
    printf("xsave %llu\nxrstor %llu\n", (unsigned long long)((saved - start) * 1000 / ROUNDS),
           (unsigned long long)((restored - saved) * 1000 / ROUNDS));
    return 0;
}
