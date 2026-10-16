/* A tour of the kernel32, user32 and advapi32 APIs with behaviour worth
 * pinning down: heaps, virtual memory, TLS, threads with events and critical
 * sections, semaphores, named mutexes, files and directory searches,
 * wsprintf, code page conversion, the registry, time, the environment, system
 * information and FormatMessage. Each failed check prints "FAIL <name>"; the
 * program prints "ok" and exits with 0 when everything passes, otherwise it
 * exits with the number of failures. */
#include "win.h"

static HANDLE out;
static int failures;

void *memset(void *d, int c, SIZE_T n) {
    unsigned char *p = d;
    while (n--) *p++ = (unsigned char)c;
    return d;
}

void *memcpy(void *d, const void *s, SIZE_T n) {
    unsigned char *p = d;
    const unsigned char *q = s;
    while (n--) *p++ = *q++;
    return d;
}

static void say(const char *s) {
    DWORD n;
    WriteFile(out, s, lstrlenA(s), &n, 0);
}

static void check(int ok, const char *name) {
    if (ok) return;
    failures++;
    say("FAIL ");
    say(name);
    say("\n");
}

static int starts_with(const char *s, const char *head) {
    while (*head)
        if (*s++ != *head++) return 0;
    return 1;
}

static int ends_with(const char *s, const char *tail) {
    int n = lstrlenA(s), m = lstrlenA(tail);
    return m <= n && lstrcmpA(s + n - m, tail) == 0;
}

/* ---- heaps and virtual memory ---------------------------------------- */

static void heaps(void) {
    HANDLE heap = GetProcessHeap();
    unsigned char *p = HeapAlloc(heap, HEAP_ZERO_MEMORY, 100);
    int zero = p != 0;
    for (int i = 0; p && i < 100; i++) zero &= p[i] == 0;
    check(zero, "HeapAlloc zeroes");
    if (!p) return;
    for (int i = 0; i < 100; i++) p[i] = (unsigned char)i;
    p = HeapReAlloc(heap, 0, p, 5000);
    int kept = p != 0;
    for (int i = 0; p && i < 100; i++) kept &= p[i] == i;
    check(kept, "HeapReAlloc keeps contents");
    check(p && HeapSize(heap, 0, p) == 5000, "HeapSize");
    check(p && HeapFree(heap, 0, p), "HeapFree");

    HANDLE own = HeapCreate(0, 0, 0);
    void *q = own ? HeapAlloc(own, 0, 1 << 20) : 0; /* a large block */
    check(q != 0, "HeapCreate + large HeapAlloc");
    check(own && HeapDestroy(own), "HeapDestroy");
}

static void virtual_memory(void) {
    unsigned char *p = VirtualAlloc(0, 0x10000, MEM_RESERVE, PAGE_READWRITE);
    check(p != 0, "VirtualAlloc reserve");
    if (!p) return;
    check(VirtualAlloc(p, 0x1000, MEM_COMMIT, PAGE_READWRITE) == p, "VirtualAlloc commit");
    p[0] = 1;
    p[0xFFF] = 2;
    MEMORY_BASIC_INFORMATION mbi;
    check(VirtualQuery(p, &mbi, sizeof mbi) == sizeof mbi && mbi.State == MEM_COMMIT &&
              mbi.RegionSize == 0x1000 && mbi.AllocationBase == p,
          "VirtualQuery committed");
    check(VirtualQuery(p + 0x1000, &mbi, sizeof mbi) && mbi.State == MEM_RESERVE,
          "VirtualQuery reserved");
    DWORD old = 0;
    check(VirtualProtect(p, 0x1000, PAGE_READONLY, &old) && old == PAGE_READWRITE,
          "VirtualProtect");
    check(p[0xFFF] == 2, "committed memory readable");
    check(VirtualFree(p, 0, MEM_RELEASE), "VirtualFree release");
    check(VirtualQuery(p, &mbi, sizeof mbi) && mbi.State == MEM_FREE, "released is free");
}

/* ---- threads and synchronisation ------------------------------------- */

static CRITICAL_SECTION cs;
static volatile long counter;
static HANDLE done;
static DWORD tls;

static DWORD WINAPI worker(void *param) {
    /* A new thread starts with empty TLS slots. */
    if (TlsGetValue(tls) != 0) return 99;
    TlsSetValue(tls, param);
    for (int i = 0; i < 1000; i++) {
        EnterCriticalSection(&cs);
        long v = counter;
        if ((i & 63) == 0) Sleep(0); /* let the other thread try to get in */
        counter = v + 1;
        LeaveCriticalSection(&cs);
    }
    SetEvent(done);
    return TlsGetValue(tls) == param ? 7 : 98;
}

static void threads(void) {
    tls = TlsAlloc();
    check(tls != TLS_OUT_OF_INDEXES, "TlsAlloc");
    check(TlsSetValue(tls, (void *)0x1234) && TlsGetValue(tls) == (void *)0x1234, "TlsGet/SetValue");
    InitializeCriticalSection(&cs);
    done = CreateEventA(0, FALSE, FALSE, 0);
    DWORD tid = 0;
    HANDLE th = CreateThread(0, 0, worker, (void *)0x55, 0, &tid);
    check(th != 0 && tid != 0 && tid != GetCurrentThreadId(), "CreateThread");
    for (int i = 0; i < 1000; i++) {
        EnterCriticalSection(&cs);
        long v = counter;
        if ((i & 63) == 0) Sleep(0);
        counter = v + 1;
        LeaveCriticalSection(&cs);
    }
    check(WaitForSingleObject(done, INFINITE) == WAIT_OBJECT_0, "wait for event");
    check(WaitForSingleObject(th, 5000) == WAIT_OBJECT_0, "wait for thread");
    DWORD code = 0;
    check(GetExitCodeThread(th, &code) && code == 7, "thread exit code");
    check(counter == 2000, "critical section counts");
    check(TlsGetValue(tls) == (void *)0x1234, "TLS is per thread");
    check(WaitForSingleObject(done, 0) == WAIT_TIMEOUT, "auto-reset event resets");
    CloseHandle(th);
    CloseHandle(done);
    DeleteCriticalSection(&cs);
    check(TlsFree(tls), "TlsFree");

    HANDLE sem = CreateSemaphoreA(0, 1, 2, 0);
    check(WaitForSingleObject(sem, 0) == WAIT_OBJECT_0, "semaphore take");
    check(WaitForSingleObject(sem, 0) == WAIT_TIMEOUT, "semaphore empty");
    long prev = -1;
    check(ReleaseSemaphore(sem, 2, &prev) && prev == 0, "ReleaseSemaphore");
    check(!ReleaseSemaphore(sem, 1, 0), "semaphore maximum");
    CloseHandle(sem);

    HANDLE m1 = CreateMutexA(0, TRUE, "anyos-apitest-mutex");
    HANDLE m2 = CreateMutexA(0, FALSE, "anyos-apitest-mutex");
    check(m1 && m2 && GetLastError() == ERROR_ALREADY_EXISTS, "named mutex exists");
    check(WaitForSingleObject(m2, 0) == WAIT_OBJECT_0, "mutex is recursive for its owner");
    check(ReleaseMutex(m1) && ReleaseMutex(m2) && !ReleaseMutex(m1), "ReleaseMutex counts");
    CloseHandle(m1);
    CloseHandle(m2);
}

/* ---- files ------------------------------------------------------------ */

static void files(void) {
    char tmp[260], dir[260], path[260], other[260], buf[64];
    DWORD n = GetTempPathA(sizeof tmp, tmp);
    check(n > 0 && tmp[n - 1] == '\\', "GetTempPathA");
    /* A unique name, made into a directory. */
    check(GetTempFileNameA(tmp, "any", 0, dir) != 0, "GetTempFileNameA");
    check(DeleteFileA(dir), "DeleteFileA temp name");
    check(CreateDirectoryA(dir, 0), "CreateDirectoryA");
    check(!CreateDirectoryA(dir, 0) && GetLastError() == ERROR_ALREADY_EXISTS, "CreateDirectoryA again");

    wsprintfA(path, "%s\\One.txt", dir);
    HANDLE f = CreateFileA(path, GENERIC_READ | GENERIC_WRITE, 0, 0, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, 0);
    check(f != INVALID_HANDLE_VALUE, "CreateFileA");
    check(WriteFile(f, "hello world", 11, &n, 0) && n == 11, "WriteFile");
    check(GetFileSize(f, 0) == 11, "GetFileSize");
    check(SetFilePointer(f, 6, 0, FILE_BEGIN) == 6, "SetFilePointer");
    check(ReadFile(f, buf, sizeof buf, &n, 0) && n == 5 && buf[0] == 'w', "ReadFile");
    check(ReadFile(f, buf, sizeof buf, &n, 0) && n == 0, "ReadFile at end");
    CloseHandle(f);
    check(CreateFileA(path, GENERIC_READ, 0, 0, CREATE_NEW, 0, 0) == INVALID_HANDLE_VALUE &&
              GetLastError() == ERROR_FILE_EXISTS,
          "CREATE_NEW on existing");

    /* Paths ignore case, like Windows. */
    wsprintfA(other, "%s\\ONE.TXT", dir);
    check(GetFileAttributesA(other) == FILE_ATTRIBUTE_ARCHIVE, "case-insensitive attributes");
    wsprintfA(other, "%s\\two.txt", dir);
    check(CopyFileA(path, other, TRUE), "CopyFileA");
    check(!CopyFileA(path, other, TRUE), "CopyFileA fail if exists");

    wsprintfA(buf, "%s\\*.TXT", dir);
    WIN32_FIND_DATAA fd;
    HANDLE find = FindFirstFileA(buf, &fd);
    int found = 0;
    if (find != INVALID_HANDLE_VALUE) {
        do {
            if (!lstrcmpA(fd.cFileName, "One.txt") || !lstrcmpA(fd.cFileName, "two.txt")) found++;
        } while (FindNextFileA(find, &fd));
        check(GetLastError() == ERROR_NO_MORE_FILES, "FindNextFileA ends");
        FindClose(find);
    }
    check(found == 2, "FindFirstFileA wildcard");

    wsprintfA(buf, "%s\\three.txt", dir);
    check(MoveFileA(other, buf), "MoveFileA");
    check(GetFileAttributesA(other) == INVALID_FILE_ATTRIBUTES, "moved away");
    check(!RemoveDirectoryA(dir) && GetLastError() == ERROR_DIR_NOT_EMPTY, "RemoveDirectoryA non-empty");
    check(DeleteFileA(buf) && DeleteFileA(path), "DeleteFileA");
    check(RemoveDirectoryA(dir), "RemoveDirectoryA");

    char *file_part = 0;
    n = GetFullPathNameA("a\\b\\..\\c.txt", sizeof path, path, &file_part);
    check(n > 0 && ends_with(path, "\\a\\c.txt") && file_part && !lstrcmpA(file_part, "c.txt"),
          "GetFullPathNameA");
    /* The current directory is reported without the trailing backslash. */
    check(SetCurrentDirectoryA(tmp) && GetCurrentDirectoryA(sizeof path, path) == (DWORD)lstrlenA(tmp) - 1 &&
              starts_with(tmp, path),
          "Set/GetCurrentDirectoryA");
}

/* ---- strings ---------------------------------------------------------- */

static void strings(void) {
    char buf[128];
    const char *want = "-42|   ab|ff  |BEEF|Z|4000000000|end";
    int n = wsprintfA(buf, "%d|%5s|%-4x|%04X|%c|%lu|%s", -42, "ab", 0xff, 0xbeef, 'Z',
                      4000000000UL, "end");
    check(n == lstrlenA(want) && !lstrcmpA(buf, want), "wsprintfA");
    WCHAR w[64];
    n = wsprintfW(w, L"%s=%d", L"x", 5);
    check(n == 3 && w[0] == 'x' && w[1] == '=' && w[2] == '5' && w[3] == 0, "wsprintfW");

    const char *utf8 = "h\xc3\xa9!";
    n = MultiByteToWideChar(CP_UTF8, 0, utf8, -1, 0, 0);
    check(n == 4, "MultiByteToWideChar size");
    n = MultiByteToWideChar(CP_UTF8, 0, utf8, -1, w, 64);
    check(n == 4 && w[0] == 'h' && w[1] == 0xE9 && w[2] == '!' && w[3] == 0, "MultiByteToWideChar");
    n = WideCharToMultiByte(CP_UTF8, 0, w, -1, buf, sizeof buf, 0, 0);
    check(n == 5 && !lstrcmpA(buf, utf8), "WideCharToMultiByte");
    BOOL used = FALSE;
    n = WideCharToMultiByte(1252, 0, w, 3, buf, sizeof buf, 0, &used);
    check(n == 3 && (unsigned char)buf[1] == 0xE9 && !used, "WideCharToMultiByte 1252");

    check(lstrcmpiA("Hello", "hELLO") == 0 && lstrcmpA("a", "B") < 0, "lstrcmp order");
    lstrcpynA(buf, "truncate me", 6);
    check(!lstrcmpA(buf, "trunc"), "lstrcpynA");
}

/* ---- registry --------------------------------------------------------- */

static void registry(void) {
    HKEY key = 0;
    DWORD disp = 0;
    check(RegCreateKeyExA(HKEY_CURRENT_USER, "Software\\AnyOS Test\\Sub", 0, 0, 0, KEY_ALL_ACCESS, 0,
                          &key, &disp) == ERROR_SUCCESS &&
              disp == REG_CREATED_NEW_KEY,
          "RegCreateKeyExA");
    DWORD v = 1234;
    check(RegSetValueExA(key, "Number", 0, REG_DWORD, (const BYTE *)&v, 4) == 0, "RegSetValueExA dword");
    check(RegSetValueExA(key, "Text", 0, REG_SZ, (const BYTE *)"value", 6) == 0, "RegSetValueExA sz");
    RegCloseKey(key);

    check(RegOpenKeyExA(HKEY_CURRENT_USER, "software\\anyos test\\SUB", 0, KEY_READ, &key) == 0,
          "RegOpenKeyExA ignores case");
    DWORD type = 0, size = sizeof v;
    v = 0;
    check(RegQueryValueExA(key, "number", 0, &type, (BYTE *)&v, &size) == 0 && type == REG_DWORD &&
              v == 1234 && size == 4,
          "RegQueryValueExA dword");
    char text[16];
    size = 2;
    check(RegQueryValueExA(key, "Text", 0, &type, (BYTE *)text, &size) == ERROR_MORE_DATA && size == 6,
          "RegQueryValueExA too small");
    size = sizeof text;
    check(RegQueryValueExA(key, "Text", 0, &type, (BYTE *)text, &size) == 0 && type == REG_SZ &&
              !lstrcmpA(text, "value"),
          "RegQueryValueExA sz");
    char name[32];
    DWORD count = 0, len;
    for (DWORD i = 0;; i++) {
        len = sizeof name;
        if (RegEnumValueA(key, i, name, &len, 0, 0, 0, 0) != 0) break;
        count++;
    }
    check(count == 2, "RegEnumValueA");
    check(RegDeleteValueA(key, "Text") == 0 && RegDeleteValueA(key, "Text") == ERROR_FILE_NOT_FOUND,
          "RegDeleteValueA");
    RegCloseKey(key);

    check(RegOpenKeyExA(HKEY_CURRENT_USER, "Software\\AnyOS Test", 0, KEY_READ, &key) == 0, "open parent");
    len = sizeof name;
    check(RegEnumKeyExA(key, 0, name, &len, 0, 0, 0, 0) == 0 && !lstrcmpA(name, "Sub") && len == 3,
          "RegEnumKeyExA");
    RegCloseKey(key);
    check(RegDeleteKeyA(HKEY_CURRENT_USER, "Software\\AnyOS Test") == ERROR_ACCESS_DENIED,
          "RegDeleteKeyA refuses keys with subkeys");
    check(RegDeleteKeyA(HKEY_CURRENT_USER, "Software\\AnyOS Test\\Sub") == 0 &&
              RegDeleteKeyA(HKEY_CURRENT_USER, "Software\\AnyOS Test") == 0,
          "RegDeleteKeyA");
    check(RegOpenKeyExA(HKEY_CURRENT_USER, "Software\\AnyOS Test", 0, KEY_READ, &key) == ERROR_FILE_NOT_FOUND,
          "deleted key is gone");

    check(RegOpenKeyExA(HKEY_LOCAL_MACHINE, "Software\\Microsoft\\Windows NT\\CurrentVersion", 0,
                        KEY_READ, &key) == 0,
          "open CurrentVersion");
    size = sizeof name;
    check(RegQueryValueExA(key, "ProductName", 0, 0, (BYTE *)name, &size) == 0 && starts_with(name, "Windows"),
          "ProductName");
    RegCloseKey(key);
}

/* ---- time, environment, system information ---------------------------- */

static void time_env_system(void) {
    SYSTEMTIME st, back;
    FILETIME ft;
    GetSystemTime(&st);
    check(st.wYear >= 2024 && st.wMonth >= 1 && st.wMonth <= 12, "GetSystemTime");
    check(SystemTimeToFileTime(&st, &ft) && FileTimeToSystemTime(&ft, &back) && back.wYear == st.wYear &&
              back.wDay == st.wDay && back.wSecond == st.wSecond && back.wMilliseconds == st.wMilliseconds,
          "SYSTEMTIME round trip");
    SYSTEMTIME epoch = {1601, 1, 1, 1, 0, 0, 0, 0};
    check(SystemTimeToFileTime(&epoch, &ft) && ft.dwLowDateTime == 0 && ft.dwHighDateTime == 0,
          "FILETIME epoch");
    DWORD t0 = GetTickCount();
    Sleep(20);
    check(GetTickCount() - t0 >= 15, "Sleep and GetTickCount");
    LARGE_INTEGER freq, c1, c2;
    check(QueryPerformanceFrequency(&freq) && QueryPerformanceCounter(&c1) && QueryPerformanceCounter(&c2) &&
              c2.QuadPart >= c1.QuadPart && freq.QuadPart > 0,
          "QueryPerformanceCounter");

    char buf[260];
    check(SetEnvironmentVariableA("ANYOS_TEST", "42"), "SetEnvironmentVariableA");
    check(GetEnvironmentVariableA("anyos_test", buf, sizeof buf) == 2 && !lstrcmpA(buf, "42"),
          "GetEnvironmentVariableA");
    check(ExpandEnvironmentStringsA("%ANYOS_TEST%-%NOT_SET%", buf, sizeof buf) == 13 &&
              !lstrcmpA(buf, "42-%NOT_SET%"),
          "ExpandEnvironmentStringsA");
    check(SetEnvironmentVariableA("ANYOS_TEST", 0) && !GetEnvironmentVariableA("ANYOS_TEST", buf, sizeof buf) &&
              GetLastError() == ERROR_ENVVAR_NOT_FOUND,
          "environment variable removed");

    SYSTEM_INFO si;
    GetSystemInfo(&si);
    check(si.dwPageSize == 4096 && si.dwAllocationGranularity == 0x10000 && si.dwNumberOfProcessors >= 1,
          "GetSystemInfo");
    OSVERSIONINFOA vi;
    vi.dwOSVersionInfoSize = sizeof vi;
    check(GetVersionExA(&vi) && vi.dwMajorVersion == 10 && vi.dwPlatformId == 2, "GetVersionExA");
    DWORD n = sizeof buf;
    check(GetComputerNameA(buf, &n) && n > 0 && n == (DWORD)lstrlenA(buf), "GetComputerNameA");
    n = sizeof buf;
    check(GetUserNameA(buf, &n) && n == (DWORD)lstrlenA(buf) + 1, "GetUserNameA");
    check(GetWindowsDirectoryA(buf, sizeof buf) == 10 && !lstrcmpA(buf, "C:\\Windows"), "GetWindowsDirectoryA");
    check(GetSystemDirectoryA(buf, 5) == 20, "GetSystemDirectoryA size");

    n = FormatMessageA(FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS, 0, ERROR_FILE_NOT_FOUND, 0,
                       buf, sizeof buf, 0);
    check(n > 0 && starts_with(buf, "The system cannot find the file"), "FormatMessageA system");
    DWORD_PTR args[2] = {(DWORD_PTR) "disk", 7};
    n = FormatMessageA(FORMAT_MESSAGE_FROM_STRING | FORMAT_MESSAGE_ARGUMENT_ARRAY, "%1 has %2!d! files%n", 0, 0,
                       buf, sizeof buf, (void *)args);
    check(n == 18 && !lstrcmpA(buf, "disk has 7 files\r\n"), "FormatMessageA inserts");
}

/* ---- structured exception handling (SEH) --------------------------------
 * Verify that RaiseException walks the x86 SEH chain (FS:[0]).
 *
 * We install a handler that:
 *   1. Records it was called.
 *   2. Modifies the CONTEXT's EIP to skip past the RaiseException call.
 *   3. Returns ExceptionContinueExecution (0).
 *
 * Exception code 0x0EEDFADE is the code Delphi / Inno Setup uses.
 *
 * A second case below covers the other half of the same contract: a filter
 * that claims the exception with ExceptionContinueExecution but leaves the
 * CONTEXT alone. On Windows the thread then carries on just past the raise, so
 * RaiseException returns to its caller -- a call gate must not leave the
 * thread running into its own trap padding.
 *
 * Only compiled for x86 (i386) because the chain is FS:[0]-based; x64 uses
 * table-driven unwind which AnyOS handles separately.
 */

#if defined(__i386__)

static volatile int seh_handler_called;
static volatile DWORD seh_resume_eip; /* set before the raise */

/* Minimal x86 CONTEXT overlay (only the fields we need). */
typedef struct {
    DWORD flags;           /* +0x00 */
    DWORD dr[6];           /* +0x04 */
    BYTE  float_save[112]; /* +0x1C */
    DWORD seg_gs;          /* +0x8C */
    DWORD seg_fs;          /* +0x90 */
    DWORD seg_es;          /* +0x94 */
    DWORD seg_ds;          /* +0x98 */
    DWORD edi;             /* +0x9C */
    DWORD esi;             /* +0xA0 */
    DWORD ebx;             /* +0xA4 */
    DWORD edx;             /* +0xA8 */
    DWORD ecx;             /* +0xAC */
    DWORD eax;             /* +0xB0 */
    DWORD ebp;             /* +0xB4 */
    DWORD eip;             /* +0xB8 */
    DWORD seg_cs;          /* +0xBC */
    DWORD eflags;          /* +0xC0 */
    DWORD esp;             /* +0xC4 */
    DWORD seg_ss;          /* +0xC8 */
} SEH_CONTEXT;

/* SEH registration record: { next, handler }. */
typedef struct {
    DWORD next;
    DWORD handler;
} SEH_FRAME;

/* The handler itself: called with the 4-arg Windows SEH calling convention.
 * We use __attribute__((regparm(0))) / plain stdcall push-args model. */
static DWORD WINAPI seh_handler(
    void        *rec,
    SEH_FRAME   *frame,
    SEH_CONTEXT *ctx,
    void        *disp)
{
    (void)rec; (void)frame; (void)disp;
    seh_handler_called = 1;
    /* Redirect to just after the RaiseException call. */
    ctx->eip = seh_resume_eip;
    return 0; /* ExceptionContinueExecution */
}

static void seh(void) {
    SEH_FRAME frame;
    seh_handler_called = 0;

    /* Save FS:[0] and install our frame. */
    DWORD old_head;
    __asm__ volatile("mov %%fs:0, %0" : "=r"(old_head));
    frame.next    = old_head;
    frame.handler = (DWORD)(ULONG_PTR)&seh_handler;
    __asm__ volatile("mov %0, %%fs:0" :: "r"((DWORD)(ULONG_PTR)&frame) : "memory");

    /* Capture the resume address (right after RaiseException) using a
     * call/pop pair placed immediately after the RaiseException call in the
     * same asm block. */
    __asm__ volatile(
        /* Save EAX/ECX/EDX (cdecl caller-saved) so we can use them. */
        "push %%eax\n\t"
        "push %%ecx\n\t"
        "push %%edx\n\t"
        /* Push RaiseException args: info=0, nparams=0, flags=0, code=0x0EEDFADE */
        "push $0\n\t"          /* lpArguments   */
        "push $0\n\t"          /* nNumberOfArguments */
        "push $0\n\t"          /* dwExceptionFlags */
        "push $0x0EEDFADE\n\t" /* dwExceptionCode */
        /* Load the address of the label just after the call into seh_resume_eip. */
        "lea 1f, %%eax\n\t"
        "mov %%eax, %0\n\t"
        /* Call RaiseException via the import thunk. */
        "call *%1\n\t"
        /* Handler resumes here (ExceptionContinueExecution). */
        "1:\n\t"
        /* Restore. */
        "pop %%edx\n\t"
        "pop %%ecx\n\t"
        "pop %%eax\n\t"
        : "=m"(seh_resume_eip)
        : "r"((DWORD)(ULONG_PTR)RaiseException) /* call-gate address in a register */
        : "memory", "eax", "ecx", "edx", "cc"
    );

    /* Restore original SEH head. */
    __asm__ volatile("mov %0, %%fs:0" :: "r"(old_head) : "memory");

    check(seh_handler_called, "SEH ExceptionContinueExecution (Delphi 0x0EEDFADE)");
}

/* The Delphi / Inno Setup shape of the same thing: the filter returns
 * ExceptionContinueExecution without touching the CONTEXT at all. Execution
 * resumes where the runtime says the exception happened -- just past the
 * raise -- so RaiseException returns to this call site. */
static volatile int seh_continue_called;
static volatile int seh_continue_returned;

static DWORD WINAPI seh_continue_handler(
    void        *rec,
    SEH_FRAME   *frame,
    SEH_CONTEXT *ctx,
    void        *disp)
{
    (void)rec; (void)frame; (void)ctx; (void)disp;
    seh_continue_called = 1;
    return 0; /* ExceptionContinueExecution, CONTEXT left as it was */
}

static void seh_continue(void) {
    SEH_FRAME frame;
    seh_continue_called = 0;
    seh_continue_returned = 0;

    DWORD old_head;
    __asm__ volatile("mov %%fs:0, %0" : "=r"(old_head));
    frame.next    = old_head;
    frame.handler = (DWORD)(ULONG_PTR)&seh_continue_handler;
    __asm__ volatile("mov %0, %%fs:0" :: "r"((DWORD)(ULONG_PTR)&frame) : "memory");

    __asm__ volatile(
        "push %%eax\n\t"
        "push %%ecx\n\t"
        "push %%edx\n\t"
        "push $0\n\t"          /* lpArguments   */
        "push $0\n\t"          /* nNumberOfArguments */
        "push $0\n\t"          /* dwExceptionFlags */
        "push $0x0EEDFADE\n\t" /* dwExceptionCode */
        "call *%1\n\t"
        /* Reached only if RaiseException returned to us. */
        "movl $1, %0\n\t"
        "pop %%edx\n\t"
        "pop %%ecx\n\t"
        "pop %%eax\n\t"
        : "=m"(seh_continue_returned)
        : "r"((DWORD)(ULONG_PTR)RaiseException)
        : "memory", "eax", "ecx", "edx", "cc"
    );

    __asm__ volatile("mov %0, %%fs:0" :: "r"(old_head) : "memory");

    check(seh_continue_called, "SEH filter ran (untouched CONTEXT)");
    check(seh_continue_returned, "SEH ContinueExecution returns from RaiseException");
}

/* ---- RtlUnwind -----------------------------------------------------------
 * Delphi / Inno Setup run the `finally` code of the frames they unwind and pop
 * them before entering an `except` block, then carry on where the raise left
 * off with the raise's return value in EAX. RtlUnwind calls the handlers of the
 * frames *inside* the target frame with EXCEPTION_UNWINDING set, does *not*
 * call the target frame's own handler, and returns. */

static volatile int   rtl_inner_calls;
static volatile DWORD rtl_inner_flags;    /* ExceptionFlags of the last call */
static volatile int   rtl_target_calls;
static volatile int   rtl_target_unwound; /* the target handler called by the unwind? */
static volatile DWORD rtl_value;          /* EAX after RtlUnwind returned */
static volatile DWORD rtl_resume_eip;     /* just after the raise */
static SEH_FRAME rtl_inner_frame;
static SEH_FRAME rtl_target_frame;

static DWORD WINAPI rtl_inner_handler(
    void        *rec,
    SEH_FRAME   *frame,
    SEH_CONTEXT *ctx,
    void        *disp)
{
    (void)frame; (void)ctx; (void)disp;
    rtl_inner_calls++;
    rtl_inner_flags = ((DWORD *)rec)[1]; /* ExceptionFlags */
    return 1; /* ExceptionContinueSearch: this frame only has cleanup */
}

static DWORD WINAPI rtl_target_handler(
    void        *rec,
    SEH_FRAME   *frame,
    SEH_CONTEXT *ctx,
    void        *disp)
{
    (void)frame; (void)disp;
    rtl_target_calls++;
    if (((DWORD *)rec)[1] & 2) { /* EXCEPTION_UNWINDING */
        rtl_target_unwound = 1;
        return 1;
    }
    /* The search reached the frame that handles the exception: unwind the
     * frames inside it first, the way the RTL does. */
    rtl_value = (DWORD)(ULONG_PTR)RtlUnwind(&rtl_target_frame, 0, rec,
                                           (void *)(ULONG_PTR)0x1234);
    /* Back from the unwind: enter the "except block". */
    ctx->eip = rtl_resume_eip;
    return 0; /* ExceptionContinueExecution */
}

static void rtl_unwind(void) {
    DWORD old_head, head_after;
    rtl_inner_calls = rtl_target_calls = rtl_target_unwound = 0;
    rtl_inner_flags = rtl_value = 0;

    __asm__ volatile("mov %%fs:0, %0" : "=r"(old_head));
    rtl_target_frame.next    = old_head;
    rtl_target_frame.handler = (DWORD)(ULONG_PTR)&rtl_target_handler;
    rtl_inner_frame.next     = (DWORD)(ULONG_PTR)&rtl_target_frame;
    rtl_inner_frame.handler  = (DWORD)(ULONG_PTR)&rtl_inner_handler;
    __asm__ volatile("mov %0, %%fs:0" :: "r"((DWORD)(ULONG_PTR)&rtl_inner_frame) : "memory");

    __asm__ volatile(
        "push %%eax\n\t"
        "push %%ecx\n\t"
        "push %%edx\n\t"
        "push $0\n\t"          /* lpArguments   */
        "push $0\n\t"          /* nNumberOfArguments */
        "push $0\n\t"          /* dwExceptionFlags */
        "push $0x0EEDFADE\n\t" /* dwExceptionCode */
        "lea 1f, %%eax\n\t"
        "mov %%eax, %0\n\t"
        "call *%1\n\t"
        "1:\n\t"
        "pop %%edx\n\t"
        "pop %%ecx\n\t"
        "pop %%eax\n\t"
        : "=m"(rtl_resume_eip)
        : "r"((DWORD)(ULONG_PTR)RaiseException)
        : "memory", "eax", "ecx", "edx", "cc");

    __asm__ volatile("mov %%fs:0, %0" : "=r"(head_after));
    __asm__ volatile("mov %0, %%fs:0" :: "r"(old_head) : "memory");

    check(rtl_inner_calls == 2, "RtlUnwind runs the inner frame's handler");
    check((rtl_inner_flags & 2) != 0, "RtlUnwind sets EXCEPTION_UNWINDING");
    check(rtl_target_calls == 1, "RtlUnwind leaves the target frame alone");
    check(!rtl_target_unwound, "the target frame is not called during the unwind");
    check(rtl_value == 0x1234, "RtlUnwind leaves ReturnValue in EAX");
    check(head_after == (DWORD)(ULONG_PTR)&rtl_target_frame, "RtlUnwind pops the unwound frames");
}

/* The other half of RtlUnwind, and the one an `except` block relies on: a frame
 * handler reached by the unwind answers ExceptionContinueExecution with a
 * CONTEXT of its own, and the thread resumes there -- the call to RtlUnwind
 * never returns. Delphi's frames set both EIP and ESP, so the fixture does too. */
static volatile int   rtl_handler_inner_unwound;
static volatile int   rtl_handler_resumed;  /* resumed inside the handler */
static volatile int   rtl_handler_returned; /* or came back from RtlUnwind */
static volatile DWORD rtl_handler_eip;
static volatile DWORD rtl_handler_esp;
static volatile int   rtl_handler_target_calls;
static SEH_FRAME rtl_handler_inner_frame;
static SEH_FRAME rtl_handler_target_frame;

static DWORD WINAPI rtl_handler_inner(
    void        *rec,
    SEH_FRAME   *frame,
    SEH_CONTEXT *ctx,
    void        *disp)
{
    (void)frame; (void)disp;
    if (((DWORD *)rec)[1] & 2) { /* EXCEPTION_UNWINDING */
        rtl_handler_inner_unwound = 1;
        /* The capture was taken with the three saved registers already pushed,
         * which is the stack the resume point expects. */
        ctx->eip = rtl_handler_eip;
        ctx->esp = rtl_handler_esp;
        return 0; /* ExceptionContinueExecution */
    }
    return 1; /* ExceptionContinueSearch */
}

static DWORD WINAPI rtl_handler_target(
    void        *rec,
    SEH_FRAME   *frame,
    SEH_CONTEXT *ctx,
    void        *disp)
{
    (void)frame; (void)ctx; (void)disp;
    rtl_handler_target_calls++;
    if (((DWORD *)rec)[1] & 2) {
        return 1;
    }
    RtlUnwind(&rtl_handler_target_frame, 0, rec, 0);
    /* Only reached if the unwind came back instead of continuing in a frame. */
    rtl_handler_returned = 1;
    return 1;
}

static void rtl_unwind_into_handler(void) {
    DWORD old_head;
    rtl_handler_inner_unwound = rtl_handler_resumed = rtl_handler_returned = 0;
    rtl_handler_target_calls = 0;

    __asm__ volatile("mov %%fs:0, %0" : "=r"(old_head));
    rtl_handler_target_frame.next    = old_head;
    rtl_handler_target_frame.handler = (DWORD)(ULONG_PTR)&rtl_handler_target;
    rtl_handler_inner_frame.next     = (DWORD)(ULONG_PTR)&rtl_handler_target_frame;
    rtl_handler_inner_frame.handler  = (DWORD)(ULONG_PTR)&rtl_handler_inner;
    __asm__ volatile("mov %0, %%fs:0" :: "r"((DWORD)(ULONG_PTR)&rtl_handler_inner_frame) : "memory");

    __asm__ volatile(
        "push %%eax\n\t"
        "push %%ecx\n\t"
        "push %%edx\n\t"
        "mov %%esp, %0\n\t"    /* the stack the resume point expects */
        "push $0\n\t"          /* lpArguments   */
        "push $0\n\t"          /* nNumberOfArguments */
        "push $0\n\t"          /* dwExceptionFlags */
        "push $0x0EEDFADE\n\t" /* dwExceptionCode */
        "lea 2f, %%eax\n\t"
        "mov %%eax, %1\n\t"
        "call *%4\n\t"
        "movl $1, %2\n\t"      /* RtlUnwind returned to its caller */
        "jmp 3f\n\t"
        "2:\n\t"                 /* the unwind resumed here */
        "movl $1, %3\n\t"
        "3:\n\t"
        "pop %%edx\n\t"
        "pop %%ecx\n\t"
        "pop %%eax\n\t"
        : "=m"(rtl_handler_esp),
          "=m"(rtl_handler_eip),
          "=m"(rtl_handler_returned),
          "=m"(rtl_handler_resumed)
        : "r"((DWORD)(ULONG_PTR)RaiseException)
        : "memory", "eax", "ecx", "edx", "cc");

    __asm__ volatile("mov %0, %%fs:0" :: "r"(old_head) : "memory");

    check(rtl_handler_inner_unwound, "RtlUnwind runs the inner frame's cleanup");
    check(rtl_handler_resumed, "the unwind continues inside the frame's handler");
    check(!rtl_handler_returned, "RtlUnwind does not return when a handler takes over");
}

#else /* !__i386__ */
static void seh(void) {} /* no-op on other architectures */
static void seh_continue(void) {}
static void rtl_unwind(void) {}
static void rtl_unwind_into_handler(void) {}
#endif

void mainCRTStartup(void) {
    out = GetStdHandle(STD_OUTPUT_HANDLE);
    heaps();
    virtual_memory();
    threads();
    files();
    strings();
    registry();
    time_env_system();
    seh();
    seh_continue();
    rtl_unwind();
    rtl_unwind_into_handler();
    if (!failures) say("ok\n");
    ExitProcess(failures);
}
