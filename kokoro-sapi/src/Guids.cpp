// Single translation unit that *defines* every COM GUID we reference.
// Including <initguid.h> before the SAPI headers makes their DEFINE_GUID
// declarations emit actual storage here, so we don't need to link sapi.lib.
#include <windows.h>
#include <initguid.h>
#include <sapi.h>
#include <sapiddk.h>
#include "Guids.h"
