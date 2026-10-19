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

void mainCRTStartup(void) {
    out = GetStdHandle(STD_OUTPUT_HANDLE);
    heaps();
    virtual_memory();
    threads();
    files();
    strings();
    registry();
    time_env_system();
    if (!failures) say("ok\n");
    ExitProcess(failures);
}
