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
