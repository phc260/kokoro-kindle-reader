// Probe the OneCore SAPI stack (sapi_onecore.dll) the way Kindle's
// NarratorService does: CoCreate its SpVoice, report the default voice,
// enumerate the voice tokens it sees, then speak one line with the default.
#include <windows.h>
#include <sapi.h>
#include <sphelper.h>
#include <cstdio>

// OneCore co-classes (registered by sapi_onecore.dll; same interfaces as SAPI5).
static const CLSID CLSID_SpVoiceOneCore =
    {0x9BC773B8, 0x9B6C, 0x400F, {0x8A, 0xF0, 0x0D, 0xFD, 0xD1, 0xC4, 0x32, 0x29}};
static const CLSID CLSID_SpObjectTokenCategoryOneCore =
    {0x461DED9E, 0x81D5, 0x494F, {0xBC, 0x96, 0x64, 0x32, 0xC8, 0x64, 0x57, 0x33}};

static void PrintToken(ISpObjectToken* tok, const wchar_t* label) {
    LPWSTR id = nullptr, desc = nullptr;
    tok->GetId(&id);
    tok->GetStringValue(nullptr, &desc);  // (Default) value = friendly name
    wprintf(L"%s: %s\n    id=%s\n", label, desc ? desc : L"?", id ? id : L"?");
    if (id) CoTaskMemFree(id);
    if (desc) CoTaskMemFree(desc);
}

int wmain() {
    CoInitialize(nullptr);

    // 1) Enumerate what the OneCore category sees.
    ISpObjectTokenCategory* cat = nullptr;
    HRESULT hr = CoCreateInstance(CLSID_SpObjectTokenCategoryOneCore, nullptr,
                                  CLSCTX_INPROC_SERVER, IID_ISpObjectTokenCategory,
                                  reinterpret_cast<void**>(&cat));
    wprintf(L"create OneCore token category: 0x%08lX\n", hr);
    if (SUCCEEDED(hr)) {
        hr = cat->SetId(L"HKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Speech_OneCore\\Voices", FALSE);
        wprintf(L"category SetId: 0x%08lX\n", hr);
        IEnumSpObjectTokens* en = nullptr;
        if (SUCCEEDED(hr) && SUCCEEDED(cat->EnumTokens(nullptr, nullptr, &en))) {
            ULONG count = 0;
            en->GetCount(&count);
            wprintf(L"OneCore voices: %lu\n", count);
            for (ULONG i = 0; i < count; ++i) {
                ISpObjectToken* t = nullptr;
                if (SUCCEEDED(en->Item(i, &t))) {
                    wchar_t label[16];
                    swprintf_s(label, L"  [%lu]", i);
                    PrintToken(t, label);
                    t->Release();
                }
            }
            en->Release();
        }
        // Default token id as the category resolves it.
        LPWSTR defId = nullptr;
        if (SUCCEEDED(cat->GetDefaultTokenId(&defId))) {
            wprintf(L"category default: %s\n", defId);
            CoTaskMemFree(defId);
        } else {
            wprintf(L"category default: <none>\n");
        }
        cat->Release();
    }

    // 2) Create the OneCore SpVoice and see which voice it starts with.
    ISpVoice* v = nullptr;
    hr = CoCreateInstance(CLSID_SpVoiceOneCore, nullptr, CLSCTX_INPROC_SERVER,
                          IID_ISpVoice, reinterpret_cast<void**>(&v));
    wprintf(L"create OneCore SpVoice: 0x%08lX\n", hr);
    if (SUCCEEDED(hr)) {
        ISpObjectToken* tok = nullptr;
        if (SUCCEEDED(v->GetVoice(&tok)) && tok) {
            PrintToken(tok, L"SpVoice default voice");
            tok->Release();
        }
        hr = v->Speak(L"This is the one core default voice.", 0, nullptr);
        wprintf(L"Speak: 0x%08lX\n", hr);
        v->Release();
    }

    CoUninitialize();
    return 0;
}
