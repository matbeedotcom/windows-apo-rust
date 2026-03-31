#pragma once

#include <audioenginebaseapo.h>
#include <audioengineextensionapo.h>
#include <BaseAudioProcessingObject.h>
#include <mmdeviceapi.h>
#include <Unknwn.h>

// ── Callback vtable: C++ skeleton calls Rust via these function pointers ──

struct ApoCallbacks {
    bool (*on_initialize)(void* user_data, float sample_rate);
    bool (*on_lock)(void* user_data, unsigned int in_channels, unsigned int out_channels, float sample_rate);
    void (*on_unlock)(void* user_data);
    void (*process)(void* user_data, float* buffer, unsigned int n_frames, unsigned int channels, bool is_silent);
    void (*destroy)(void* user_data);
    void (*on_effect_state)(void* user_data, const unsigned char* effect_id, int state);
    long long (*get_latency)(void* user_data);
};

// ── Static registration info provided by Rust ──

struct ApoRegistration {
    unsigned char clsid[16];       // Raw GUID bytes
    const wchar_t* name;           // Null-terminated wide string
    const wchar_t* copyright;      // Null-terminated wide string
    unsigned int apo_flags;        // APO_FLAG bitmask
    const unsigned char (*effect_guids)[16]; // Array of 16-byte GUIDs
    unsigned int num_effects;      // Number of effect GUIDs
};

// ── Rust FFI functions (implemented by consumer crate via apo_entry! macro) ──

extern "C" {
    ApoCallbacks* apo_get_callbacks();
    ApoRegistration* apo_get_registration();
    void* apo_create_processor();
}

// ── Helper: convert ApoRegistration CLSID bytes to a Windows GUID ──

inline GUID ApoClsidToGuid(const unsigned char* bytes) {
    GUID g;
    // Windows GUID layout: Data1 (LE u32), Data2 (LE u16), Data3 (LE u16), Data4[8]
    g.Data1 = (unsigned long)bytes[0] | ((unsigned long)bytes[1] << 8) |
              ((unsigned long)bytes[2] << 16) | ((unsigned long)bytes[3] << 24);
    g.Data2 = (unsigned short)bytes[4] | ((unsigned short)bytes[5] << 8);
    g.Data3 = (unsigned short)bytes[6] | ((unsigned short)bytes[7] << 8);
    memcpy(g.Data4, &bytes[8], 8);
    return g;
}

inline GUID ApoEffectGuid(const ApoRegistration* reg, unsigned int index) {
    return ApoClsidToGuid(reg->effect_guids[index]);
}

// ── INonDelegatingUnknown interface ──

class INonDelegatingUnknown
{
    virtual HRESULT __stdcall NonDelegatingQueryInterface(const IID& iid, void** ppv) = 0;
    virtual ULONG __stdcall NonDelegatingAddRef() = 0;
    virtual ULONG __stdcall NonDelegatingRelease() = 0;
};

// ── GenericApo: the generic APO class ──

class GenericApo : public CBaseAudioProcessingObject,
                   public IAudioSystemEffects3,
                   public IAudioProcessingObjectNotifications,
                   public INonDelegatingUnknown
{
public:
    GenericApo(IUnknown* pUnkOuter);
    virtual ~GenericApo();

    // IUnknown (delegates to pUnkOuter)
    virtual HRESULT __stdcall QueryInterface(const IID& iid, void** ppv);
    virtual ULONG __stdcall AddRef();
    virtual ULONG __stdcall Release();

    // IAudioProcessingObject
    virtual HRESULT __stdcall GetLatency(HNSTIME* pTime);
    virtual HRESULT __stdcall Initialize(UINT32 cbDataSize, BYTE* pbyData);
    virtual HRESULT __stdcall IsInputFormatSupported(IAudioMediaType* pOutputFormat,
        IAudioMediaType* pRequestedInputFormat, IAudioMediaType** ppSupportedInputFormat);

    // IAudioSystemEffects2
    virtual HRESULT __stdcall GetEffectsList(LPGUID* ppEffectsIds, UINT* pcEffects, HANDLE Event);

    // IAudioSystemEffects3
    virtual HRESULT __stdcall GetControllableSystemEffectsList(
        AUDIO_SYSTEMEFFECT** effects, UINT* numEffects, HANDLE event);
    virtual HRESULT __stdcall SetAudioSystemEffectState(
        GUID effectId, AUDIO_SYSTEMEFFECT_STATE state);

    // IAudioProcessingObjectNotifications
    virtual HRESULT __stdcall GetApoNotificationRegistrationInfo(
        APO_NOTIFICATION_DESCRIPTOR** apoNotifications, DWORD* count);
    virtual void __stdcall HandleNotification(APO_NOTIFICATION* apoNotification);

    // IAudioProcessingObjectConfiguration
    virtual HRESULT __stdcall LockForProcess(UINT32 u32NumInputConnections,
        APO_CONNECTION_DESCRIPTOR** ppInputConnections, UINT32 u32NumOutputConnections,
        APO_CONNECTION_DESCRIPTOR** ppOutputConnections);
    virtual HRESULT __stdcall UnlockForProcess(void);

    // IAudioProcessingObjectRT
    virtual void __stdcall APOProcess(UINT32 u32NumInputConnections,
        APO_CONNECTION_PROPERTY** ppInputConnections,
        UINT32 u32NumOutputConnections,
        APO_CONNECTION_PROPERTY** ppOutputConnections);

    // INonDelegatingUnknown
    virtual HRESULT __stdcall NonDelegatingQueryInterface(const IID& iid, void** ppv);
    virtual ULONG __stdcall NonDelegatingAddRef();
    virtual ULONG __stdcall NonDelegatingRelease();

    static long instCount;
    static const CRegAPOProperties<1> regProperties;

private:
    long refCount;
    IUnknown* pUnkOuter;

    UINT32 inputChannels;
    UINT32 outputChannels;
    float sampleRate;
    bool firstProcess;

    // Rust processor state (opaque, managed via callbacks)
    void* userData;
    ApoCallbacks* callbacks;

    // IAudioSystemEffects3 / CAPX state
    HANDLE effectsChangedEvent;
    bool discoveryOnly;
    GUID audioProcessingMode;
    IMMDevice* audioEndpoint;
};
