/* Windows build of the interpreter self-test (see selftest.h). */
#include "win.h"
#include "selftest.h"

static HANDLE stdout_handle;
static void out(const char *s, unsigned n) { DWORD w; WriteFile(stdout_handle, s, n, &w, 0); }

void mainCRTStartup(void) {
    stdout_handle = GetStdHandle(STD_OUTPUT_HANDLE);
    selftest();
    ExitProcess(0);
}
