/* DLL fixture: named exports, an ordinal-only export, and a DllMain entry. */
#include "win.h"
__declspec(dllexport) int anyos_add(int a, int b) { return a + b; }
__declspec(dllexport) int anyos_mul(int a, int b) { return a * b; }
int anyos_hidden(void) { return 42; }
int counter = 7;
int *counter_ptr = &counter;
BOOL WINAPI _DllMainCRTStartup(HANDLE h, DWORD reason, void *r) { (void)h; (void)reason; (void)r; return 1; }
