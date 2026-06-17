#pragma once
// Tiny append-only file logger for diagnosing in-host (Kindle) failures.
// Writes to %TEMP%\KokoroSapi.log -- note that for packaged (MSIX) hosts,
// %TEMP% is redirected to the package's LocalCache, so look there too.
#include <windows.h>
#include <cstdio>
#include <cstdarg>

inline void KokoroLog(const char* fmt, ...) {
    wchar_t path[MAX_PATH];
    const DWORD n = GetTempPathW(MAX_PATH, path);
    if (n == 0 || n >= MAX_PATH) return;
    wcscat_s(path, L"KokoroSapi.log");

    FILE* f = _wfopen(path, L"ab");
    if (!f) return;

    SYSTEMTIME st;
    GetLocalTime(&st);
    fprintf(f, "[%02u:%02u:%02u.%03u pid=%lu] ", st.wHour, st.wMinute,
            st.wSecond, st.wMilliseconds, GetCurrentProcessId());

    va_list ap;
    va_start(ap, fmt);
    vfprintf(f, fmt, ap);
    va_end(ap);
    fputc('\n', f);
    fclose(f);
}
