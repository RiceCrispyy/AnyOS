/* Console fixture: kernel32 imports, a pointer table (forces base relocations),
 * and a hand-built TLS directory with one callback. */
#include "win.h"

static const char line1[] = "hello from anyos fixture\n";
static const char line2[] = "second line\n";
/* Absolute pointers in .data -> base relocations. */
const char *const lines[] = { line1, line2 };
static const DWORD lens[] = { sizeof(line1) - 1, sizeof(line2) - 1 };

volatile int tls_callback_ran;
static void WINAPI tls_cb(void *h, DWORD reason, void *r) { (void)h; (void)r; tls_callback_ran = (int)reason; }

/* TLS directory the linker points the image's TLS data directory at. */
unsigned int _tls_index;
char _tls_start[8] = { 1, 2, 3, 4, 5, 6, 7, 8 };
char _tls_end;
void (WINAPI *const tls_callbacks[2])(void *, DWORD, void *) = { tls_cb, 0 };
struct {
    void *start, *end, *index, *callbacks;
    DWORD zero_fill, characteristics;
} const _tls_used = { _tls_start, &_tls_end, &_tls_index, (void *)tls_callbacks, 0, 0 };

void mainCRTStartup(void) {
    HANDLE out = GetStdHandle(STD_OUTPUT_HANDLE);
    DWORD written;
    for (int i = 0; i < 2; i++) WriteFile(out, lines[i], lens[i], &written, 0);
    ExitProcess(0);
}
