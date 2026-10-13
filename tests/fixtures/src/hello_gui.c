/* GUI-subsystem fixture: kernel32 import plus a delay-loaded user32 import. */
#include "win.h"

/* Stand-in for the MSVC delay-load helper so no CRT is needed. Never runs. */
void *WINAPI __delayLoadHelper2(const void *descriptor, void **iat_slot) {
    (void)descriptor;
    return *iat_slot;
}

void WinMainCRTStartup(void) {
    MessageBoxA(0, "hello from anyos fixture", "AnyOS", 0);
    ExitProcess(0);
}
