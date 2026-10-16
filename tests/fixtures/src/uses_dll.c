/* Imports from exports_dll by name (anyos_add) and by ordinal (anyos_hidden = @7),
 * so the loader can be tested binding one guest module against another. */
#include "win.h"
__declspec(dllimport) int anyos_add(int, int);
__declspec(dllimport) int anyos_hidden(void);
volatile int result;
void mainCRTStartup(void) {
    result = anyos_add(2, 3) + anyos_hidden();
    ExitProcess((UINT)result);
}
