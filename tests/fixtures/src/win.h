/* Minimal Win32 declarations so fixtures build without a Windows SDK. */
#pragma once
typedef void *HANDLE;
typedef unsigned long DWORD;
typedef int BOOL;
typedef unsigned int UINT;
#define WINAPI __stdcall
#define STD_OUTPUT_HANDLE ((DWORD)-11)
__declspec(dllimport) HANDLE WINAPI GetStdHandle(DWORD);
__declspec(dllimport) BOOL WINAPI WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);
__declspec(dllimport) void WINAPI ExitProcess(UINT);
__declspec(dllimport) int WINAPI MessageBoxA(void *, const char *, const char *, UINT);
__declspec(dllimport) DWORD WINAPI GetLastError(void);
__declspec(dllimport) HANDLE WINAPI GetModuleHandleA(const char *);
__declspec(dllimport) DWORD WINAPI GetModuleFileNameA(HANDLE, char *, DWORD);
__declspec(dllimport) HANDLE WINAPI LoadLibraryA(const char *);
__declspec(dllimport) BOOL WINAPI FreeLibrary(HANDLE);
__declspec(dllimport) void *WINAPI GetProcAddress(HANDLE, const char *);

/* ---- used by apitest.c ---- */
typedef unsigned short WORD;
typedef unsigned short WCHAR;
typedef unsigned char BYTE;
typedef long LONG;
typedef __SIZE_TYPE__ SIZE_T;
typedef __UINTPTR_TYPE__ ULONG_PTR;
typedef ULONG_PTR DWORD_PTR;
typedef void *HKEY;
typedef long LSTATUS;
typedef DWORD(WINAPI *THREAD_START)(void *);
typedef union {
    struct {
        DWORD LowPart;
        LONG HighPart;
    } u;
    long long QuadPart;
} LARGE_INTEGER;
typedef struct {
    DWORD dwLowDateTime, dwHighDateTime;
} FILETIME;
typedef struct {
    WORD wYear, wMonth, wDayOfWeek, wDay, wHour, wMinute, wSecond, wMilliseconds;
} SYSTEMTIME;
typedef struct {
    void *DebugInfo;
    LONG LockCount, RecursionCount;
    HANDLE OwningThread, LockSemaphore;
    ULONG_PTR SpinCount;
} CRITICAL_SECTION;
typedef struct {
    void *BaseAddress, *AllocationBase;
    DWORD AllocationProtect;
    SIZE_T RegionSize;
    DWORD State, Protect, Type;
} MEMORY_BASIC_INFORMATION;
typedef struct {
    DWORD dwFileAttributes;
    FILETIME ftCreationTime, ftLastAccessTime, ftLastWriteTime;
    DWORD nFileSizeHigh, nFileSizeLow, dwReserved0, dwReserved1;
    char cFileName[260];
    char cAlternateFileName[14];
} WIN32_FIND_DATAA;
typedef struct {
    WORD wProcessorArchitecture, wReserved;
    DWORD dwPageSize;
    void *lpMinimumApplicationAddress, *lpMaximumApplicationAddress;
    DWORD_PTR dwActiveProcessorMask;
    DWORD dwNumberOfProcessors, dwProcessorType, dwAllocationGranularity;
    WORD wProcessorLevel, wProcessorRevision;
} SYSTEM_INFO;
typedef struct {
    DWORD dwOSVersionInfoSize, dwMajorVersion, dwMinorVersion, dwBuildNumber, dwPlatformId;
    char szCSDVersion[128];
} OSVERSIONINFOA;

#define TRUE 1
#define FALSE 0
#define INFINITE 0xFFFFFFFF
#define INVALID_HANDLE_VALUE ((HANDLE)(ULONG_PTR)-1)
#define INVALID_FILE_ATTRIBUTES ((DWORD)-1)
#define TLS_OUT_OF_INDEXES ((DWORD)0xFFFFFFFF)
#define WAIT_OBJECT_0 0
#define WAIT_TIMEOUT 0x102
#define HEAP_ZERO_MEMORY 0x8
#define MEM_COMMIT 0x1000
#define MEM_RESERVE 0x2000
#define MEM_RELEASE 0x8000
#define MEM_FREE 0x10000
#define PAGE_READONLY 0x02
#define PAGE_READWRITE 0x04
#define GENERIC_READ 0x80000000
#define GENERIC_WRITE 0x40000000
#define CREATE_NEW 1
#define FILE_ATTRIBUTE_ARCHIVE 0x20
#define FILE_ATTRIBUTE_NORMAL 0x80
#define FILE_BEGIN 0
#define CP_UTF8 65001
#define ERROR_SUCCESS 0
#define ERROR_FILE_NOT_FOUND 2
#define ERROR_ACCESS_DENIED 5
#define ERROR_NO_MORE_FILES 18
#define ERROR_FILE_EXISTS 80
#define ERROR_DIR_NOT_EMPTY 145
#define ERROR_ALREADY_EXISTS 183
#define ERROR_ENVVAR_NOT_FOUND 203
#define ERROR_MORE_DATA 234
#define HKEY_CURRENT_USER ((HKEY)(ULONG_PTR)(LONG)0x80000001)
#define HKEY_LOCAL_MACHINE ((HKEY)(ULONG_PTR)(LONG)0x80000002)
#define KEY_READ 0x20019
#define KEY_ALL_ACCESS 0xF003F
#define REG_SZ 1
#define REG_DWORD 4
#define REG_CREATED_NEW_KEY 1
#define FORMAT_MESSAGE_IGNORE_INSERTS 0x200
#define FORMAT_MESSAGE_FROM_STRING 0x400
#define FORMAT_MESSAGE_FROM_SYSTEM 0x1000
#define FORMAT_MESSAGE_ARGUMENT_ARRAY 0x2000

#define K32 __declspec(dllimport)
K32 HANDLE WINAPI GetProcessHeap(void);
K32 void *WINAPI HeapAlloc(HANDLE, DWORD, SIZE_T);
K32 void *WINAPI HeapReAlloc(HANDLE, DWORD, void *, SIZE_T);
K32 SIZE_T WINAPI HeapSize(HANDLE, DWORD, const void *);
K32 BOOL WINAPI HeapFree(HANDLE, DWORD, void *);
K32 HANDLE WINAPI HeapCreate(DWORD, SIZE_T, SIZE_T);
K32 BOOL WINAPI HeapDestroy(HANDLE);
K32 void *WINAPI VirtualAlloc(void *, SIZE_T, DWORD, DWORD);
K32 BOOL WINAPI VirtualFree(void *, SIZE_T, DWORD);
K32 BOOL WINAPI VirtualProtect(void *, SIZE_T, DWORD, DWORD *);
K32 SIZE_T WINAPI VirtualQuery(const void *, MEMORY_BASIC_INFORMATION *, SIZE_T);
K32 DWORD WINAPI TlsAlloc(void);
K32 BOOL WINAPI TlsFree(DWORD);
K32 void *WINAPI TlsGetValue(DWORD);
K32 BOOL WINAPI TlsSetValue(DWORD, void *);
K32 HANDLE WINAPI CreateThread(void *, SIZE_T, THREAD_START, void *, DWORD, DWORD *);
K32 BOOL WINAPI GetExitCodeThread(HANDLE, DWORD *);
K32 DWORD WINAPI GetCurrentThreadId(void);
K32 BOOL WINAPI CloseHandle(HANDLE);
K32 void WINAPI InitializeCriticalSection(CRITICAL_SECTION *);
K32 void WINAPI EnterCriticalSection(CRITICAL_SECTION *);
K32 void WINAPI LeaveCriticalSection(CRITICAL_SECTION *);
K32 void WINAPI DeleteCriticalSection(CRITICAL_SECTION *);
K32 HANDLE WINAPI CreateEventA(void *, BOOL, BOOL, const char *);
K32 BOOL WINAPI SetEvent(HANDLE);
K32 HANDLE WINAPI CreateSemaphoreA(void *, LONG, LONG, const char *);
K32 BOOL WINAPI ReleaseSemaphore(HANDLE, LONG, LONG *);
K32 HANDLE WINAPI CreateMutexA(void *, BOOL, const char *);
K32 BOOL WINAPI ReleaseMutex(HANDLE);
K32 DWORD WINAPI WaitForSingleObject(HANDLE, DWORD);
K32 void WINAPI Sleep(DWORD);
K32 DWORD WINAPI GetTempPathA(DWORD, char *);
K32 UINT WINAPI GetTempFileNameA(const char *, const char *, UINT, char *);
K32 BOOL WINAPI DeleteFileA(const char *);
K32 BOOL WINAPI CreateDirectoryA(const char *, void *);
K32 BOOL WINAPI RemoveDirectoryA(const char *);
K32 HANDLE WINAPI CreateFileA(const char *, DWORD, DWORD, void *, DWORD, DWORD, HANDLE);
K32 BOOL WINAPI ReadFile(HANDLE, void *, DWORD, DWORD *, void *);
K32 DWORD WINAPI GetFileSize(HANDLE, DWORD *);
K32 DWORD WINAPI SetFilePointer(HANDLE, LONG, LONG *, DWORD);
K32 DWORD WINAPI GetFileAttributesA(const char *);
K32 BOOL WINAPI CopyFileA(const char *, const char *, BOOL);
K32 BOOL WINAPI MoveFileA(const char *, const char *);
K32 HANDLE WINAPI FindFirstFileA(const char *, WIN32_FIND_DATAA *);
K32 BOOL WINAPI FindNextFileA(HANDLE, WIN32_FIND_DATAA *);
K32 BOOL WINAPI FindClose(HANDLE);
K32 DWORD WINAPI GetFullPathNameA(const char *, DWORD, char *, char **);
K32 BOOL WINAPI SetCurrentDirectoryA(const char *);
K32 DWORD WINAPI GetCurrentDirectoryA(DWORD, char *);
K32 int WINAPI lstrlenA(const char *);
K32 int WINAPI lstrcmpA(const char *, const char *);
K32 int WINAPI lstrcmpiA(const char *, const char *);
K32 char *WINAPI lstrcpynA(char *, const char *, int);
K32 int WINAPI MultiByteToWideChar(UINT, DWORD, const char *, int, WCHAR *, int);
K32 int WINAPI WideCharToMultiByte(UINT, DWORD, const WCHAR *, int, char *, int, const char *, BOOL *);
K32 void WINAPI GetSystemTime(SYSTEMTIME *);
K32 BOOL WINAPI SystemTimeToFileTime(const SYSTEMTIME *, FILETIME *);
K32 BOOL WINAPI FileTimeToSystemTime(const FILETIME *, SYSTEMTIME *);
K32 DWORD WINAPI GetTickCount(void);
K32 BOOL WINAPI QueryPerformanceCounter(LARGE_INTEGER *);
K32 BOOL WINAPI QueryPerformanceFrequency(LARGE_INTEGER *);
K32 BOOL WINAPI SetEnvironmentVariableA(const char *, const char *);
K32 DWORD WINAPI GetEnvironmentVariableA(const char *, char *, DWORD);
K32 DWORD WINAPI ExpandEnvironmentStringsA(const char *, char *, DWORD);
K32 void WINAPI GetSystemInfo(SYSTEM_INFO *);
K32 BOOL WINAPI GetVersionExA(OSVERSIONINFOA *);
K32 BOOL WINAPI GetComputerNameA(char *, DWORD *);
K32 UINT WINAPI GetWindowsDirectoryA(char *, UINT);
K32 UINT WINAPI GetSystemDirectoryA(char *, UINT);
K32 DWORD WINAPI FormatMessageA(DWORD, const void *, DWORD, DWORD, char *, DWORD, void *);
K32 int __cdecl wsprintfA(char *, const char *, ...);
K32 int __cdecl wsprintfW(WCHAR *, const WCHAR *, ...);
K32 BOOL WINAPI GetUserNameA(char *, DWORD *);
K32 LSTATUS WINAPI RegCreateKeyExA(HKEY, const char *, DWORD, char *, DWORD, DWORD, void *, HKEY *, DWORD *);
K32 LSTATUS WINAPI RegOpenKeyExA(HKEY, const char *, DWORD, DWORD, HKEY *);
K32 LSTATUS WINAPI RegCloseKey(HKEY);
K32 LSTATUS WINAPI RegSetValueExA(HKEY, const char *, DWORD, DWORD, const BYTE *, DWORD);
K32 LSTATUS WINAPI RegQueryValueExA(HKEY, const char *, DWORD *, DWORD *, BYTE *, DWORD *);
K32 LSTATUS WINAPI RegEnumValueA(HKEY, DWORD, char *, DWORD *, DWORD *, DWORD *, BYTE *, DWORD *);
K32 LSTATUS WINAPI RegEnumKeyExA(HKEY, DWORD, char *, DWORD *, DWORD *, char *, DWORD *, FILETIME *);
K32 LSTATUS WINAPI RegDeleteValueA(HKEY, const char *);
K32 LSTATUS WINAPI RegDeleteKeyA(HKEY, const char *);
