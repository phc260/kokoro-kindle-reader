#pragma once
#include <guiddef.h>

// CLSID for the Kokoro SAPI5 TTS engine COM object.
// {0898F9AB-42C8-4DA5-A54F-520C9DD13C49}
// In Guids.cpp (which defines INITGUID) this becomes the GUID's storage definition;
// in every other translation unit it is just an extern declaration.
DEFINE_GUID(CLSID_KokoroTTSEngine,
    0x0898f9ab, 0x42c8, 0x4da5, 0xa5, 0x4f, 0x52, 0x0c, 0x9d, 0xd1, 0x3c, 0x49);
