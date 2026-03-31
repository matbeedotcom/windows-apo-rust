#include "GenericApo.h"
#include <cstdio>
#include <cstring>
#include <cmath>

// ── Debug logging ──────────────────────────────────────────────────────

static void DebugLog(const char* msg)
{
    FILE* f = nullptr;
    fopen_s(&f, "C:\\ProgramData\\HrtfApo\\debug.log", "a");
    if (f) {
        SYSTEMTIME st;
        GetLocalTime(&st);
        fprintf(f, "[%02d:%02d:%02d.%03d] [GenericApo] %s\r\n",
                st.wHour, st.wMinute, st.wSecond, st.wMilliseconds, msg);
        fclose(f);
    }
}

static void DebugLogf(const char* fmt, ...)
{
    char buf[512];
    va_list args;
    va_start(args, fmt);
    vsnprintf(buf, sizeof(buf), fmt, args);
    va_end(args);
    DebugLog(buf);
}

// ── Static registration ────────────────────────────────────────────────

// Build CRegAPOProperties from the Rust-provided registration at DLL load time.
// We need the CLSID, name, copyright, and flags.

static GUID GetRegisteredClsid() {
    ApoRegistration* reg = apo_get_registration();
    return ApoClsidToGuid(reg->clsid);
}

long GenericApo::instCount = 0;

// This static initializer calls into Rust to get the CLSID and flags.
// CRegAPOProperties<1> takes: CLSID, name, copyright, major, minor, iid, flags
const CRegAPOProperties<1> GenericApo::regProperties(
    GetRegisteredClsid(),
    apo_get_registration()->name,
    apo_get_registration()->copyright,
    1, 0,
    __uuidof(IAudioProcessingObject),
    (APO_FLAG)apo_get_registration()->apo_flags
);

// ── Constructor / Destructor ───────────────────────────────────────────

GenericApo::GenericApo(IUnknown* pUnkOuter)
    : CBaseAudioProcessingObject(regProperties)
{
    refCount = 1;
    if (pUnkOuter != NULL)
        this->pUnkOuter = pUnkOuter;
    else
        this->pUnkOuter = reinterpret_cast<IUnknown*>(static_cast<INonDelegatingUnknown*>(this));

    inputChannels = 0;
    outputChannels = 0;
    sampleRate = 48000.0f;
    firstProcess = true;

    callbacks = apo_get_callbacks();
    userData = apo_create_processor();

    effectsChangedEvent = NULL;
    discoveryOnly = false;
    audioProcessingMode = AUDIO_SIGNALPROCESSINGMODE_DEFAULT;
    audioEndpoint = nullptr;

    InterlockedIncrement(&instCount);
    DebugLogf("GenericApo constructed (instance %ld, userData=%p)", instCount, userData);
}

GenericApo::~GenericApo()
{
    if (userData && callbacks && callbacks->destroy) {
        callbacks->destroy(userData);
        userData = nullptr;
    }
    if (effectsChangedEvent) { CloseHandle(effectsChangedEvent); effectsChangedEvent = NULL; }
    if (audioEndpoint) { audioEndpoint->Release(); audioEndpoint = nullptr; }
    InterlockedDecrement(&instCount);
    DebugLog("GenericApo destroyed");
}

// ── IUnknown (delegating) ──────────────────────────────────────────────

HRESULT GenericApo::QueryInterface(const IID& iid, void** ppv)
{
    return pUnkOuter->QueryInterface(iid, ppv);
}

ULONG GenericApo::AddRef()
{
    return pUnkOuter->AddRef();
}

ULONG GenericApo::Release()
{
    return pUnkOuter->Release();
}

// ── INonDelegatingUnknown ──────────────────────────────────────────────

HRESULT GenericApo::NonDelegatingQueryInterface(const IID& iid, void** ppv)
{
    if (iid == __uuidof(IUnknown))
        *ppv = static_cast<INonDelegatingUnknown*>(this);
    else if (iid == __uuidof(IAudioProcessingObject))
        *ppv = static_cast<IAudioProcessingObject*>(this);
    else if (iid == __uuidof(IAudioProcessingObjectRT))
        *ppv = static_cast<IAudioProcessingObjectRT*>(this);
    else if (iid == __uuidof(IAudioProcessingObjectConfiguration))
        *ppv = static_cast<IAudioProcessingObjectConfiguration*>(this);
    else if (iid == __uuidof(IAudioSystemEffects))
        *ppv = static_cast<IAudioSystemEffects3*>(this);
    else if (iid == __uuidof(IAudioSystemEffects2))
        *ppv = static_cast<IAudioSystemEffects3*>(this);
    else if (iid == __uuidof(IAudioSystemEffects3))
        *ppv = static_cast<IAudioSystemEffects3*>(this);
    else if (iid == __uuidof(IAudioProcessingObjectNotifications))
        *ppv = static_cast<IAudioProcessingObjectNotifications*>(this);
    else
    {
        *ppv = NULL;
        return E_NOINTERFACE;
    }

    reinterpret_cast<IUnknown*>(*ppv)->AddRef();
    return S_OK;
}

ULONG GenericApo::NonDelegatingAddRef()
{
    return InterlockedIncrement(&refCount);
}

ULONG GenericApo::NonDelegatingRelease()
{
    if (InterlockedDecrement(&refCount) == 0)
    {
        delete this;
        return 0;
    }
    return refCount;
}

// ── IAudioProcessingObject ─────────────────────────────────────────────

HRESULT GenericApo::Initialize(UINT32 cbDataSize, BYTE* pbyData)
{
    DebugLogf("Initialize called, dataSize=%u", cbDataSize);

    if (NULL == pbyData && 0 != cbDataSize) return E_INVALIDARG;
    if (NULL != pbyData && 0 == cbDataSize) return E_INVALIDARG;

    // Handle all three init struct versions (matches SYSVAD sample pattern)
    discoveryOnly = false;
    if (cbDataSize == sizeof(APOInitSystemEffects3))
    {
        APOInitSystemEffects3* init3 = (APOInitSystemEffects3*)pbyData;
        discoveryOnly = init3->InitializeForDiscoveryOnly;
        audioProcessingMode = init3->AudioProcessingMode;

        if (init3->pDeviceCollection) {
            UINT deviceCount = 0;
            init3->pDeviceCollection->GetCount(&deviceCount);
            if (deviceCount > 0) {
                if (audioEndpoint) { audioEndpoint->Release(); audioEndpoint = nullptr; }
                init3->pDeviceCollection->Item(deviceCount - 1, &audioEndpoint);
            }
        }

        DebugLogf("Initialize v3, discovery=%d, mode=%08lX, endpoint=%p",
                 discoveryOnly ? 1 : 0, init3->AudioProcessingMode.Data1, audioEndpoint);
    }
    else if (cbDataSize == sizeof(APOInitSystemEffects2))
    {
        APOInitSystemEffects2* init2 = (APOInitSystemEffects2*)pbyData;
        DebugLogf("Initialize v2, CLSID=%08lX", init2->APOInit.clsid.Data1);
    }
    else if (cbDataSize == sizeof(APOInitSystemEffects))
    {
        APOInitSystemEffects* init1 = (APOInitSystemEffects*)pbyData;
        DebugLogf("Initialize v1, CLSID=%08lX", init1->APOInit.clsid.Data1);
    }
    else
    {
        DebugLogf("Initialize FAILED: unexpected size %u", cbDataSize);
        return E_INVALIDARG;
    }

    // Call base class Initialize
    HRESULT hr = CBaseAudioProcessingObject::Initialize(cbDataSize, pbyData);
    DebugLogf("Base Initialize: 0x%08lX", hr);
    if (FAILED(hr)) return hr;

    // Skip heavy initialization for discovery-only mode
    if (discoveryOnly) {
        DebugLog("Discovery-only init — skipping processor initialization");
        return S_OK;
    }

    // Call Rust processor's initialize callback
    if (callbacks && callbacks->on_initialize && userData) {
        if (!callbacks->on_initialize(userData, sampleRate)) {
            DebugLog("Rust on_initialize returned false");
            return E_FAIL;
        }
    }

    return S_OK;
}

HRESULT GenericApo::GetLatency(HNSTIME* pTime)
{
    if (!pTime) return E_POINTER;
    if (!m_bIsLocked) return APOERR_ALREADY_UNLOCKED;

    if (callbacks && callbacks->get_latency && userData) {
        *pTime = callbacks->get_latency(userData);
    } else {
        *pTime = 0;
    }
    return S_OK;
}

// ── Effects lists ──────────────────────────────────────────────────────

HRESULT GenericApo::GetEffectsList(LPGUID* ppEffectsIds, UINT* pcEffects, HANDLE Event)
{
    if (!ppEffectsIds || !pcEffects) return E_POINTER;

    ApoRegistration* reg = apo_get_registration();
    UINT numEffects = reg->num_effects;

    GUID* pIds = (GUID*)CoTaskMemAlloc(numEffects * sizeof(GUID));
    if (!pIds) return E_OUTOFMEMORY;

    for (UINT i = 0; i < numEffects; i++)
        pIds[i] = ApoEffectGuid(reg, i);

    *ppEffectsIds = pIds;
    *pcEffects = numEffects;
    return S_OK;
}

HRESULT GenericApo::GetControllableSystemEffectsList(
    AUDIO_SYSTEMEFFECT** effects, UINT* numEffects, HANDLE event)
{
    if (!effects || !numEffects) return E_POINTER;

    *effects = nullptr;
    *numEffects = 0;

    if (effectsChangedEvent != NULL) {
        CloseHandle(effectsChangedEvent);
        effectsChangedEvent = NULL;
    }

    if (event != NULL) {
        if (!DuplicateHandle(GetCurrentProcess(), event, GetCurrentProcess(),
                             &effectsChangedEvent, EVENT_MODIFY_STATE, FALSE, 0)) {
            return HRESULT_FROM_WIN32(GetLastError());
        }
    }

    // Don't expose effects in RAW mode
    if (IsEqualGUID(audioProcessingMode, AUDIO_SIGNALPROCESSINGMODE_RAW)) {
        return S_OK;
    }

    ApoRegistration* reg = apo_get_registration();
    UINT count = reg->num_effects;

    AUDIO_SYSTEMEFFECT* pEffects = (AUDIO_SYSTEMEFFECT*)CoTaskMemAlloc(
        count * sizeof(AUDIO_SYSTEMEFFECT));
    if (!pEffects) return E_OUTOFMEMORY;

    for (UINT i = 0; i < count; i++) {
        pEffects[i].id = ApoEffectGuid(reg, i);
        pEffects[i].canSetState = TRUE;
        pEffects[i].state = AUDIO_SYSTEMEFFECT_STATE_ON;
    }

    *effects = pEffects;
    *numEffects = count;
    return S_OK;
}

HRESULT GenericApo::SetAudioSystemEffectState(GUID effectId, AUDIO_SYSTEMEFFECT_STATE state)
{
    bool on = (state == AUDIO_SYSTEMEFFECT_STATE_ON);

    if (callbacks && callbacks->on_effect_state && userData) {
        callbacks->on_effect_state(userData, (const unsigned char*)&effectId, on ? 1 : 0);
    }

    DebugLogf("SetAudioSystemEffectState: effect %08lX → %s", effectId.Data1, on ? "ON" : "OFF");

    if (effectsChangedEvent)
        SetEvent(effectsChangedEvent);

    return S_OK;
}

// ── Notifications ──────────────────────────────────────────────────────

HRESULT GenericApo::GetApoNotificationRegistrationInfo(
    APO_NOTIFICATION_DESCRIPTOR** apoNotifications, DWORD* count)
{
    if (!apoNotifications || !count) return E_POINTER;

    if (!audioEndpoint) {
        *apoNotifications = nullptr;
        *count = 0;
        return S_OK;
    }

    DWORD numDescriptors = 1;
    APO_NOTIFICATION_DESCRIPTOR* descriptors = (APO_NOTIFICATION_DESCRIPTOR*)CoTaskMemAlloc(
        numDescriptors * sizeof(APO_NOTIFICATION_DESCRIPTOR));
    if (!descriptors) return E_OUTOFMEMORY;

    memset(descriptors, 0, numDescriptors * sizeof(APO_NOTIFICATION_DESCRIPTOR));
    descriptors[0].type = APO_NOTIFICATION_TYPE_ENDPOINT_PROPERTY_CHANGE;
    descriptors[0].audioEndpointPropertyChange.device = audioEndpoint;
    audioEndpoint->AddRef();

    *apoNotifications = descriptors;
    *count = numDescriptors;
    return S_OK;
}

void GenericApo::HandleNotification(APO_NOTIFICATION* apoNotification)
{
    if (apoNotification) {
        DebugLogf("HandleNotification: type=%d", apoNotification->type);
    }
}

// ── Format support ─────────────────────────────────────────────────────

HRESULT GenericApo::IsInputFormatSupported(
    IAudioMediaType* pOutputFormat,
    IAudioMediaType* pRequestedInputFormat,
    IAudioMediaType** ppSupportedInputFormat)
{
    __try {
        if (!pRequestedInputFormat || !ppSupportedInputFormat) return E_POINTER;

        UNCOMPRESSEDAUDIOFORMAT inFormat = {};
        HRESULT hr = pRequestedInputFormat->GetUncompressedAudioFormat(&inFormat);
        if (FAILED(hr)) return hr;

        DebugLogf("IsInputFormatSupported: ch=%u, rate=%.0f, bps=%u",
                  inFormat.dwSamplesPerFrame, inFormat.fFramesPerSecond,
                  inFormat.dwValidBitsPerSample);

        hr = CBaseAudioProcessingObject::IsInputFormatSupported(
            pOutputFormat, pRequestedInputFormat, ppSupportedInputFormat);

        DebugLogf("IsInputFormatSupported result: 0x%08lX", hr);
        return hr;
    } __except(EXCEPTION_EXECUTE_HANDLER) {
        DebugLogf("IsInputFormatSupported CRASHED: exception 0x%08lX", GetExceptionCode());
        return E_FAIL;
    }
}

// ── Lock / Unlock ──────────────────────────────────────────────────────

HRESULT GenericApo::LockForProcess(
    UINT32 u32NumInputConnections,
    APO_CONNECTION_DESCRIPTOR** ppInputConnections,
    UINT32 u32NumOutputConnections,
    APO_CONNECTION_DESCRIPTOR** ppOutputConnections)
{
    __try {
        DebugLogf("LockForProcess: %u inputs, %u outputs", u32NumInputConnections, u32NumOutputConnections);

        UNCOMPRESSEDAUDIOFORMAT inFormat = {};
        if (ppInputConnections[0]->pFormat) {
            ppInputConnections[0]->pFormat->GetUncompressedAudioFormat(&inFormat);
            inputChannels = inFormat.dwSamplesPerFrame;
        }

        UNCOMPRESSEDAUDIOFORMAT outFormat = {};
        if (ppOutputConnections[0]->pFormat) {
            ppOutputConnections[0]->pFormat->GetUncompressedAudioFormat(&outFormat);
            outputChannels = outFormat.dwSamplesPerFrame;
        }

        sampleRate = inFormat.fFramesPerSecond;
        if (sampleRate < 1.0f) sampleRate = 48000.0f;
        DebugLogf("Channels: in=%u, out=%u, rate=%.0f", inputChannels, outputChannels, sampleRate);

        HRESULT hr = CBaseAudioProcessingObject::LockForProcess(
            u32NumInputConnections, ppInputConnections,
            u32NumOutputConnections, ppOutputConnections);
        DebugLogf("Base LockForProcess: 0x%08lX", hr);

        firstProcess = true;

        // Notify Rust processor
        if (callbacks && callbacks->on_lock && userData) {
            callbacks->on_lock(userData, inputChannels, outputChannels, sampleRate);
        }

        return hr;
    } __except(EXCEPTION_EXECUTE_HANDLER) {
        DebugLogf("LockForProcess CRASHED: exception 0x%08lX", GetExceptionCode());
        return E_FAIL;
    }
}

HRESULT GenericApo::UnlockForProcess()
{
    DebugLog("UnlockForProcess");

    if (callbacks && callbacks->on_unlock && userData) {
        callbacks->on_unlock(userData);
    }

    return CBaseAudioProcessingObject::UnlockForProcess();
}

// ── Real-time audio processing ─────────────────────────────────────────

#pragma AVRT_CODE_BEGIN
void GenericApo::APOProcess(
    UINT32 u32NumInputConnections,
    APO_CONNECTION_PROPERTY** ppInputConnections,
    UINT32 u32NumOutputConnections,
    APO_CONNECTION_PROPERTY** ppOutputConnections)
{
    __try {
    if (u32NumInputConnections == 0 || u32NumOutputConnections == 0) return;

    float* buffer = reinterpret_cast<float*>(ppInputConnections[0]->pBuffer);
    UINT32 nFrames = ppInputConnections[0]->u32ValidFrameCount;

    if (firstProcess) {
        firstProcess = false;
        DebugLogf("APOProcess first: frames=%u, in=%u, out=%u, inplace=%d",
                  nFrames, inputChannels, outputChannels,
                  (buffer == reinterpret_cast<float*>(ppOutputConnections[0]->pBuffer)) ? 1 : 0);
    }

    switch (ppInputConnections[0]->u32BufferFlags)
    {
    case BUFFER_VALID:
    case BUFFER_SILENT:
    {
        bool isSilent = (ppInputConnections[0]->u32BufferFlags == BUFFER_SILENT);
        if (isSilent)
            memset(buffer, 0, nFrames * inputChannels * sizeof(float));

        // Dispatch to Rust processor
        if (callbacks && callbacks->process && userData) {
            callbacks->process(userData, buffer, nFrames, inputChannels, isSilent);
        }

        ppOutputConnections[0]->u32ValidFrameCount = nFrames;
        ppOutputConnections[0]->u32BufferFlags = BUFFER_VALID;
        break;
    }
    }
    } __except(EXCEPTION_EXECUTE_HANDLER) {
        DebugLogf("APOProcess CRASHED: exception 0x%08lX", GetExceptionCode());
        if (u32NumOutputConnections > 0) {
            ppOutputConnections[0]->u32BufferFlags = BUFFER_SILENT;
        }
    }
}
#pragma AVRT_CODE_END
