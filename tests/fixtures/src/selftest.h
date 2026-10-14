/* Interpreter self-test: integer, bit, string and control-flow work whose
 * results are printed as hex. The same code is built for Windows (run under
 * AnyOS) and natively for the build host; both outputs must match exactly.
 * Inputs come through volatile variables so nothing is folded at compile time.
 * The including file defines `out(const char *s, unsigned n)`. */
#pragma once
typedef unsigned char u8;
typedef unsigned short u16;
typedef unsigned int u32;
typedef unsigned long long u64;
typedef signed char i8;
typedef short i16;
typedef int i32;
typedef long long i64;

static void out(const char *s, unsigned n);

static unsigned slen(const char *s) { unsigned n = 0; while (s[n]) n++; return n; }

static void put(const char *name, u64 v) {
    char buf[64];
    unsigned n = slen(name);
    for (unsigned i = 0; i < n; i++) buf[i] = name[i];
    buf[n++] = ' ';
    for (int sh = 60; sh >= 0; sh -= 4) buf[n++] = "0123456789abcdef"[(v >> sh) & 15];
    buf[n++] = '\n';
    out(buf, n);
}

static volatile u32 seed = 0x12345678u;
static volatile i32 neg = -1234567;
static volatile u32 shifts[] = { 0, 1, 7, 31, 32, 33, 63 };
static volatile i32 divisors[] = { 1, -1, 3, -7, 10, 1000, -65536 };

static u32 lcg(u32 *s) { *s = *s * 1103515245u + 12345u; return *s >> 8; }

static u32 crc32(const u8 *p, unsigned n) {
    u32 c = 0xFFFFFFFFu;
    for (unsigned i = 0; i < n; i++) {
        c ^= p[i];
        for (int k = 0; k < 8; k++) c = (c >> 1) ^ (0xEDB88320u & (0u - (c & 1)));
    }
    return ~c;
}

static u32 fib(u32 n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }

static u32 classify(u32 x) {
    switch (x % 9) {
    case 0: return x * 3;
    case 1: return x ^ 0x55;
    case 2: return x + 77;
    case 3: return x >> 2;
    case 4: return ~x;
    case 5: return x * x;
    case 6: return x - 1000;
    case 7: return x | 0xF0F0;
    default: return 0;
    }
}

static void selftest(void) {
    u32 s = seed;
    u32 a[64];
    for (int i = 0; i < 64; i++) a[i] = lcg(&s);

    /* arithmetic and flags */
    u32 sum = 0, x = 0; u64 prod = 1;
    for (int i = 0; i < 64; i++) { sum += a[i]; x ^= a[i] << (i & 7); prod = prod * (a[i] | 1) + i; }
    put("sum", sum); put("xor", x); put("prod64", prod);

    /* signed / unsigned division and remainder */
    u64 dacc = 0;
    for (int i = 0; i < 7; i++) {
        i32 d = divisors[i];
        dacc = dacc * 31 + (u32)(neg / d) + (u32)(neg % d) * 7u + (a[i] / (u32)(d | 1)) + a[i] % (u32)(d | 1);
    }
    put("div", dacc);
#ifdef __x86_64__
    u64 q = 0;
    for (int i = 0; i < 8; i++) q += ((u64)a[i] << 32 | a[i + 1]) / (a[i + 2] | 1) + (i64)((i64)neg * a[i]) % 977;
    put("div64", q);
#endif

    /* shifts and rotates, by variable counts */
    u64 sh = 0;
    for (int i = 0; i < 7; i++) {
        u32 c = shifts[i];
        u64 w = (u64)a[i] << 32 | a[i + 7];
        sh += (w << c) ^ (w >> c) ^ (u64)((i64)w >> c) ^ ((u32)neg >> (c & 31)) ^ (u32)(neg >> (c & 31));
        sh += (a[i] << (c & 31)) | (a[i] >> ((32 - c) & 31));
    }
    put("shift", sh);

    /* bit operations */
    u32 bits = 0;
    for (int i = 0; i < 64; i++) {
        u32 v = a[i] | 1u << (i & 31);
        bits += (u32)__builtin_ctz(v) + (u32)__builtin_clz(v) * 3 + __builtin_bswap32(v);
        bits ^= (u32)__builtin_popcount(v);
    }
    put("bits", bits);

    /* narrow types: sign and zero extension, 8/16-bit arithmetic */
    i16 h = 0; i8 c8 = 0; u16 uh = 0;
    for (int i = 0; i < 64; i++) { h += (i8)a[i]; c8 ^= (i8)(a[i] >> 3); uh = (u16)(uh * 3 + (u16)a[i]); }
    put("narrow", (u64)(i64)h ^ ((u64)(i64)c8 << 20) ^ ((u64)uh << 40));

    /* min / max / abs (cmov, setcc) */
    i32 mn = 0x7FFFFFFF, mx = -0x7FFFFFFF - 1; u32 cnt = 0;
    for (int i = 0; i < 64; i++) {
        i32 v = (i32)a[i] - 0x400000;
        mn = v < mn ? v : mn; mx = v > mx ? v : mx;
        cnt += (v < 0) + (a[i] > 0x800000u) * 2u + ((v < 0 ? -v : v) & 15);
    }
    put("minmax", (u64)(u32)mn << 32 | (u32)mx); put("count", cnt);

    /* insertion sort */
    u32 b[64];
    for (int i = 0; i < 64; i++) b[i] = a[i];
    for (int i = 1; i < 64; i++) { u32 k = b[i]; int j = i - 1; while (j >= 0 && b[j] > k) { b[j + 1] = b[j]; j--; } b[j + 1] = k; }
    put("sorted", (u64)b[0] << 32 | b[63]); put("median", b[31]);

    /* CRC, recursion, jump table */
    put("crc", crc32((const u8 *)a, sizeof a));
    put("fib", fib(18));
    u32 sw = 0;
    for (int i = 0; i < 64; i++) sw = sw * 7 + classify(a[i]);
    put("switch", sw);

    /* string instructions, cmpxchg, xadd, bt via inline assembly */
    char src[40], dst[40];
    for (int i = 0; i < 40; i++) { src[i] = (char)('a' + i % 26); dst[i] = 0; }
    {
        void *d = dst, *sp = src; __SIZE_TYPE__ n = 33;
        __asm__ volatile("rep movsb" : "+D"(d), "+S"(sp), "+c"(n) : : "memory");
        d = dst + 33; n = 5;
        __asm__ volatile("rep stosb" : "+D"(d), "+c"(n) : "a"('#') : "memory");
    }
    put("movs", crc32((const u8 *)dst, 40));
    {
        u32 mem = a[3], expect = a[3], newv = 99;
        u8 ok;
        __asm__ volatile("lock cmpxchgl %3, %1; sete %b0" : "=q"(ok), "+m"(mem), "+a"(expect) : "r"(newv) : "memory", "cc");
        u32 add = 5;
        __asm__ volatile("lock xaddl %0, %1" : "+r"(add), "+m"(mem) : : "memory", "cc");
        u8 bt;
        __asm__ volatile("btl %2, %1; setc %b0" : "=q"(bt) : "r"(a[5]), "r"(a[6] & 31) : "cc");
        put("atomic", (u64)ok << 48 | (u64)add << 16 | mem << 1 | bt);
    }
}
