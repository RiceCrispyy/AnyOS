/* Run-time linking: GetModuleHandleA, LoadLibraryA, GetProcAddress (by name and
 * by ordinal), GetModuleFileNameA and FreeLibrary. exports_dll.dll is loaded
 * with LoadLibraryA only, never imported. Each check that passes sets one bit
 * of the exit code, which is passed to an ExitProcess found with GetProcAddress
 * (all checks passing gives 0x7FF). */
#include "win.h"
typedef int (*binop)(int, int);
typedef int (*nullary)(void);
typedef void(WINAPI *exit_fn)(UINT);

static int ends_with(const char *s, const char *tail) {
    int n = 0, m = 0;
    while (s[n]) n++;
    while (tail[m]) m++;
    if (m > n) return 0;
    for (int i = 0; i < m; i++)
        if (s[n - m + i] != tail[i]) return 0;
    return 1;
}

void mainCRTStartup(void) {
    UINT score = 0;
    char path[260];
    HANDLE k = GetModuleHandleA("KERNEL32");
    if (k) score |= 1 << 0;
    if (!GetModuleHandleA("user32.dll") && GetLastError() == 126) score |= 1 << 1;
    if (!GetProcAddress(k, "NoSuchApiAnywhere") && GetLastError() == 127) score |= 1 << 2;

    HANDLE d = LoadLibraryA("exports_dll"); /* ".dll" is implied */
    if (d) score |= 1 << 3;
    binop add = (binop)GetProcAddress(d, "anyos_add");
    if (add && add(40, 2) == 42) score |= 1 << 4;
    nullary hidden = (nullary)GetProcAddress(d, (const char *)7); /* NONAME ordinal */
    if (hidden && hidden() == 42) score |= 1 << 5;
    if (!GetProcAddress(d, "anyos_missing")) score |= 1 << 6;
    if (LoadLibraryA("EXPORTS_DLL.DLL") == d && GetModuleHandleA("exports_dll.dll") == d)
        score |= 1 << 7;
    if (!LoadLibraryA("not_here.dll") && GetLastError() == 126) score |= 1 << 8;
    DWORD n = GetModuleFileNameA(d, path, sizeof path);
    if (n > 0 && ends_with(path, "\\exports_dll.dll")) score |= 1 << 9;
    if (FreeLibrary(d)) score |= 1 << 10;

    exit_fn exit_process = (exit_fn)GetProcAddress(k, "ExitProcess");
    if (!exit_process) ExitProcess(0xBAD);
    exit_process(score);
}
