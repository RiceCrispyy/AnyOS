/* Host (Linux x86 / x86-64) build of the interpreter self-test, used to
 * produce the expected output. Freestanding, so a 32-bit build needs no
 * 32-bit libc: build-selftest.sh compiles it with -nostdlib -static. */
#include "selftest.h"

static void out(const char *s, unsigned n) {
#ifdef __x86_64__
    long r;
    __asm__ volatile("syscall" : "=a"(r) : "a"(1L), "D"(1L), "S"(s), "d"((long)n) : "rcx", "r11", "memory");
#else
    long r;
    __asm__ volatile("int $0x80" : "=a"(r) : "a"(4L), "b"(1L), "c"(s), "d"((long)n) : "memory");
#endif
    (void)r;
}

__attribute__((force_align_arg_pointer)) void _start(void) {
    selftest();
#ifdef __x86_64__
    __asm__ volatile("syscall" : : "a"(60L), "D"(0L));
#else
    __asm__ volatile("int $0x80" : : "a"(1L), "b"(0L));
#endif
    for (;;) {}
}
