#include <windows.h>
#include <aclapi.h>
#include <sddl.h>
#include <cstdio>
#include <new>
#include <audioenginebaseapo.h>
#include "ClassFactory.h"
#include "GenericApo.h"

static HINSTANCE hModule;

static void DebugLog(const char* msg)
{
    FILE* f = nullptr;
    fopen_s(&f, "C:\\ProgramData\\HrtfApo\\debug.log", "a");
    if (f) {
        SYSTEMTIME st;
        GetLocalTime(&st);
        fprintf(f, "[%02d:%02d:%02d.%03d] [APO DLL] %s\r\n",
                st.wHour, st.wMinute, st.wSecond, st.wMilliseconds, msg);
        fclose(f);
    }
}

// Note: DllMain is provided by the Rust runtime. We use a helper to capture hModule.
static void CaptureModule()
{
    HMODULE hMod = NULL;
    GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                       (LPCWSTR)CaptureModule, &hMod);
    hModule = hMod;
}

// ── Registry helpers (generic, reusable) ─────────────────────────────────

static bool EnablePrivilege(LPCWSTR privilegeName)
{
    HANDLE tokenHandle;
    if (!OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &tokenHandle))
        return false;

    LUID luid;
    if (!LookupPrivilegeValueW(NULL, privilegeName, &luid)) {
        CloseHandle(tokenHandle);
        return false;
    }

    TOKEN_PRIVILEGES tp;
    tp.PrivilegeCount = 1;
    tp.Privileges[0].Luid = luid;
    tp.Privileges[0].Attributes = SE_PRIVILEGE_ENABLED;

    BOOL ok = AdjustTokenPrivileges(tokenHandle, FALSE, &tp, sizeof(TOKEN_PRIVILEGES), NULL, NULL);
    CloseHandle(tokenHandle);
    return ok && GetLastError() == ERROR_SUCCESS;
}

static LSTATUS TakeOwnership(HKEY root, LPCWSTR subkey)
{
    HKEY hKey;
    LSTATUS st = RegOpenKeyExW(root, subkey, 0, WRITE_OWNER | KEY_WOW64_64KEY, &hKey);
    if (st != ERROR_SUCCESS) return st;

    PSID adminSid = NULL;
    SID_IDENTIFIER_AUTHORITY authority = SECURITY_NT_AUTHORITY;
    AllocateAndInitializeSid(&authority, 2, SECURITY_BUILTIN_DOMAIN_RID, DOMAIN_ALIAS_RID_ADMINS,
                             0, 0, 0, 0, 0, 0, &adminSid);

    PSECURITY_DESCRIPTOR sd = (PSECURITY_DESCRIPTOR)LocalAlloc(LPTR, SECURITY_DESCRIPTOR_MIN_LENGTH);
    InitializeSecurityDescriptor(sd, SECURITY_DESCRIPTOR_REVISION);
    SetSecurityDescriptorOwner(sd, adminSid, FALSE);

    st = RegSetKeySecurity(hKey, OWNER_SECURITY_INFORMATION, sd);

    FreeSid(adminSid);
    LocalFree(sd);
    RegCloseKey(hKey);
    return st;
}

static LSTATUS MakeWritable(HKEY root, LPCWSTR subkey)
{
    HKEY hKey;
    LSTATUS st = RegOpenKeyExW(root, subkey, 0, READ_CONTROL | WRITE_DAC | KEY_WOW64_64KEY, &hKey);
    if (st != ERROR_SUCCESS) return st;

    DWORD descriptorSize = 0;
    RegGetKeySecurity(hKey, DACL_SECURITY_INFORMATION, NULL, &descriptorSize);
    PSECURITY_DESCRIPTOR oldSd = (PSECURITY_DESCRIPTOR)HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, descriptorSize);
    st = RegGetKeySecurity(hKey, DACL_SECURITY_INFORMATION, oldSd, &descriptorSize);
    if (st != ERROR_SUCCESS) { HeapFree(GetProcessHeap(), 0, oldSd); RegCloseKey(hKey); return st; }

    BOOL aclPresent, aclDefaulted;
    PACL oldAcl = NULL;
    GetSecurityDescriptorDacl(oldSd, &aclPresent, &oldAcl, &aclDefaulted);

    PSID adminSid = NULL;
    SID_IDENTIFIER_AUTHORITY authority = SECURITY_NT_AUTHORITY;
    AllocateAndInitializeSid(&authority, 2, SECURITY_BUILTIN_DOMAIN_RID, DOMAIN_ALIAS_RID_ADMINS,
                             0, 0, 0, 0, 0, 0, &adminSid);

    EXPLICIT_ACCESSW ea = {};
    ea.grfAccessPermissions = KEY_ALL_ACCESS;
    ea.grfAccessMode = SET_ACCESS;
    ea.grfInheritance = SUB_CONTAINERS_AND_OBJECTS_INHERIT;
    ea.Trustee.TrusteeForm = TRUSTEE_IS_SID;
    ea.Trustee.TrusteeType = TRUSTEE_IS_GROUP;
    ea.Trustee.ptstrName = (LPWSTR)adminSid;

    PACL newAcl = NULL;
    SetEntriesInAclW(1, &ea, oldAcl, &newAcl);

    PSECURITY_DESCRIPTOR newSd = (PSECURITY_DESCRIPTOR)LocalAlloc(LPTR, SECURITY_DESCRIPTOR_MIN_LENGTH);
    InitializeSecurityDescriptor(newSd, SECURITY_DESCRIPTOR_REVISION);
    SetSecurityDescriptorDacl(newSd, TRUE, newAcl, FALSE);

    st = RegSetKeySecurity(hKey, DACL_SECURITY_INFORMATION, newSd);

    FreeSid(adminSid);
    LocalFree(newAcl);
    HeapFree(GetProcessHeap(), 0, oldSd);
    LocalFree(newSd);
    RegCloseKey(hKey);
    return st;
}

// ── DLL Entry Points ────────────────────────────────────────────────────

extern "C" HRESULT __stdcall DllCanUnloadNow()
{
    if (GenericApo::instCount == 0 && ClassFactory::lockCount == 0)
        return S_OK;
    return S_FALSE;
}

extern "C" HRESULT __stdcall DllGetClassObject(REFCLSID rclsid, REFIID riid, void** ppv)
{
    ApoRegistration* reg = apo_get_registration();
    GUID registeredClsid = ApoClsidToGuid(reg->clsid);

    char buf[256];
    snprintf(buf, sizeof(buf), "DllGetClassObject: CLSID=%08lX-%04X-%04X",
             rclsid.Data1, rclsid.Data2, rclsid.Data3);
    DebugLog(buf);

    if (rclsid != registeredClsid)
        return CLASS_E_CLASSNOTAVAILABLE;

    ClassFactory* factory = new (std::nothrow) ClassFactory();
    if (!factory) return E_OUTOFMEMORY;

    HRESULT hr = factory->QueryInterface(riid, ppv);
    factory->Release();

    snprintf(buf, sizeof(buf), "DllGetClassObject returning: 0x%08lX", hr);
    DebugLog(buf);
    return hr;
}

extern "C" HRESULT __stdcall DllRegisterServer()
{
    DebugLog("DllRegisterServer called");

    if (!hModule) CaptureModule();
    wchar_t filename[1024];
    GetModuleFileNameW(hModule, filename, sizeof(filename) / sizeof(wchar_t));

    ApoRegistration* reg = apo_get_registration();
    GUID clsid = ApoClsidToGuid(reg->clsid);

    // 1. Unregister first to clear stale data, then re-register
    UnregisterAPO(clsid);
    HRESULT hr = RegisterAPO(&GenericApo::regProperties.m_Properties);
    if (FAILED(hr)) {
        char buf[128]; snprintf(buf, sizeof(buf), "RegisterAPO FAILED: 0x%08lX", hr);
        DebugLog(buf);
        UnregisterAPO(clsid);
        return hr;
    }

    // 1b. Force-update Flags to match our regProperties
    {
        wchar_t apoKey[256];
        wchar_t clsidStr[64];
        StringFromGUID2(clsid, clsidStr, 64);
        _snwprintf_s(apoKey, 256,
            L"SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Audio\\AudioProcessingObjects\\%s", clsidStr);
        HKEY hApo;
        if (RegOpenKeyExW(HKEY_LOCAL_MACHINE, apoKey, 0, KEY_SET_VALUE, &hApo) == ERROR_SUCCESS) {
            DWORD flags = reg->apo_flags;
            RegSetValueExW(hApo, L"Flags", 0, REG_DWORD, (BYTE*)&flags, sizeof(flags));
            RegCloseKey(hApo);
        }
    }

    // 2. Write COM InProcServer32 with actual DLL path
    {
        wchar_t clsidStr[64];
        StringFromGUID2(clsid, clsidStr, 64);
        wchar_t keyPath[512];
        _snwprintf_s(keyPath, 512, L"SOFTWARE\\Classes\\CLSID\\%s\\InprocServer32", clsidStr);
        HKEY hKey;
        if (RegCreateKeyExW(HKEY_LOCAL_MACHINE, keyPath, 0, NULL, 0, KEY_SET_VALUE | KEY_WOW64_64KEY, NULL, &hKey, NULL) == ERROR_SUCCESS)
        {
            RegSetValueExW(hKey, NULL, 0, REG_SZ, (BYTE*)filename, (DWORD)(wcslen(filename) + 1) * sizeof(wchar_t));
            const wchar_t* threading = L"Both";
            RegSetValueExW(hKey, L"ThreadingModel", 0, REG_SZ, (BYTE*)threading, (DWORD)(wcslen(threading) + 1) * sizeof(wchar_t));
            RegCloseKey(hKey);
        }
    }

    char buf[256];
    snprintf(buf, sizeof(buf), "RegisterAPO + COM OK, DLL: %ls", filename);
    DebugLog(buf);
    return S_OK;
}

extern "C" HRESULT __stdcall DllUnregisterServer()
{
    DebugLog("DllUnregisterServer called");

    ApoRegistration* reg = apo_get_registration();
    GUID clsid = ApoClsidToGuid(reg->clsid);

    UnregisterAPO(clsid);

    wchar_t clsidStr[64];
    StringFromGUID2(clsid, clsidStr, 64);
    wchar_t keyPath[512];
    _snwprintf_s(keyPath, 512, L"SOFTWARE\\Classes\\CLSID\\%s\\InprocServer32", clsidStr);
    RegDeleteKeyW(HKEY_LOCAL_MACHINE, keyPath);
    _snwprintf_s(keyPath, 512, L"SOFTWARE\\Classes\\CLSID\\%s", clsidStr);
    RegDeleteKeyW(HKEY_LOCAL_MACHINE, keyPath);

    return S_OK;
}
