// ur_driver: Core Audio server plug-in (AudioServerPlugIn) for the Steinberg UR44 in Yamaha mode
// (CC MODE off). Port of qc_driver from QuadCaptureMac (MIT, Louis Tissot and contributors).
//
// The plug-in publishes one device ("Steinberg UR44") while the UR44 is connected in Yamaha mode:
//   input stream:  Input 1-6
//   output stream: Mix 1 L/R, Mix 2 L/R, FX Bus L/R (FX Bus feeds the REV-X reverb of the DSP)
//   32-bit float, 44.1, 48, 88.2, 96, 176.4 or 192 kHz;
//   an output volume control and an output mute control.
// In CC mode the UR44 is a class-compliant device of Apple's driver; this plug-in ignores it.
// A change of the nominal sample rate is a device configuration change: the engine stops, sets
// the device clock, and starts again. The plug-in keeps the rate in the host storage.
// The USB engine (ur_engine) runs inside the plug-in host process. It starts when the first client
// starts IO and stops (clean drain) when the last client stops IO. The device clock comes from the
// zero timestamps of the engine, so it follows the UR44 clock.
//
// The object model follows the Apple "NullAudio" sample driver.
#include <CoreAudio/AudioServerPlugIn.h>
#include <CoreFoundation/CoreFoundation.h>
#include <dispatch/dispatch.h>
#include <math.h>
#include <os/log.h>
#include <pthread.h>
#include <stdatomic.h>
#include <unistd.h>
#include <stddef.h>
#include <string.h>
#include "engine/ur_engine.h"

enum {
    kObjectID_PlugIn = kAudioObjectPlugInObject,
    kObjectID_Device = 2,
    kObjectID_Stream_Input = 3,
    kObjectID_Stream_Output = 4,
    kObjectID_Volume_Output = 5,
    kObjectID_Mute_Output = 6,
};

#define kDevice_Name CFSTR("Steinberg UR44")
#define kManufacturer CFSTR("Yamaha")
#define kDevice_UID CFSTR("UR44Driver:0499:150e")
#define kDevice_ModelUID CFSTR("UR44Driver:UR44")
#define kLatencyFrames 32   // converter latency (estimate)
#define kVolumeMinDB -64.0f
#define kVolumeMaxDB 0.0f

// Custom properties (data type: CFPropertyList, a CFDictionary).
//   'ucfg' (settable): USB queue configuration, applied with a device configuration change and kept
//          in the host storage. Keys: requests, microframes, outputMargin, inputMargin, direct
//          (1 = direct write, 2 = direct read, 3 = both).
//   'usta' (read only): engine statistics.
#define kQCPropertyConfig 'ucfg'
#define kQCPropertyStats 'usta'
#define kQCStorageKey CFSTR("UR44Config.v1")   // a new name discards the values of older versions
#define kQCVolumeStorageKey CFSTR("UR44Volume")
#define kQCRateStorageKey CFSTR("UR44Rate")

static os_log_t gLog;
static pthread_mutex_t gStateLock = PTHREAD_MUTEX_INITIALIZER;
static UInt32 gRefCount;
static AudioServerPlugInHostRef gHost;
static bool gHostReady;
static UInt32 gIOClients;
static Boolean gStreamActive[2] = {true, true};   // input, output
static CFDictionaryRef gPendingConfig;             // set by 'qcfg', applied in PerformDeviceConfigurationChange
static UInt32 gPendingRate;                        // a new nominal sample rate (0: none), applied there too
static _Atomic uint32_t gExternalRate;             // always 0: the UR44 has no external clock input
static atomic_bool gPresent;                       // the UR44 is connected in Yamaha mode
static _Atomic float gVolumeScalar = 1.0f;
static atomic_bool gMute;
static _Atomic float gTargetGain = 1.0f;           // from volume and mute; the IO thread ramps to it
static float gGainNow = 1.0f;                      // IO thread only

static const char *kInputNames[UR_IN_CHANNELS] = {"Input 1", "Input 2", "Input 3", "Input 4", "Input 5", "Input 6"};
static const char *kOutputNames[UR_OUT_CHANNELS] = {"Mix 1 L", "Mix 1 R", "Mix 2 L", "Mix 2 R", "FX Bus L", "FX Bus R"};

#pragma mark Helpers

static int dictInt(CFDictionaryRef d, CFStringRef key, int fallback) {
    CFNumberRef n = CFDictionaryGetValue(d, key);
    int v = fallback;
    if (n && CFGetTypeID(n) == CFNumberGetTypeID()) CFNumberGetValue(n, kCFNumberIntType, &v);
    return v;
}

static double dictDouble(CFDictionaryRef d, CFStringRef key, double fallback) {
    CFNumberRef n = CFDictionaryGetValue(d, key);
    double v = fallback;
    if (n && CFGetTypeID(n) == CFNumberGetTypeID()) CFNumberGetValue(n, kCFNumberDoubleType, &v);
    return v;
}

static void dictSet(CFMutableDictionaryRef d, CFStringRef key, double v) {
    CFNumberRef n = CFNumberCreate(NULL, kCFNumberDoubleType, &v);
    CFDictionarySetValue(d, key, n);
    CFRelease(n);
}

static void dictSetArray(CFMutableDictionaryRef d, CFStringRef key, const uint64_t *v, int count) {
    CFMutableArrayRef a = CFArrayCreateMutable(NULL, count, &kCFTypeArrayCallBacks);
    for (int i = 0; i < count; i++) {
        double x = (double)v[i];
        CFNumberRef n = CFNumberCreate(NULL, kCFNumberDoubleType, &x);
        CFArrayAppendValue(a, n);
        CFRelease(n);
    }
    CFDictionarySetValue(d, key, a);
    CFRelease(a);
}

static CFMutableDictionaryRef newDict(void) {
    return CFDictionaryCreateMutable(NULL, 0, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
}

static CFDictionaryRef copyConfigDictionary(void) {
    int req, tpr, direct;
    uint32_t outM, inM;
    ur_engine_get_config(&req, &tpr, &outM, &inM, &direct);
    CFMutableDictionaryRef d = newDict();
    dictSet(d, CFSTR("requests"), req);
    dictSet(d, CFSTR("microframes"), tpr);
    dictSet(d, CFSTR("outputMargin"), outM);
    dictSet(d, CFSTR("inputMargin"), inM);
    dictSet(d, CFSTR("direct"), direct);
    uint32_t g, sd, sp;
    ur_engine_get_timing(&g, &sd, &sp);
    dictSet(d, CFSTR("guardMicroframes"), g);
    dictSet(d, CFSTR("scanDelayMicroframes"), sd);
    dictSet(d, CFSTR("scanPeriodUs"), sp);
    dictSet(d, CFSTR("outputLeadFrames"), ur_engine_output_lead_frames());
    dictSet(d, CFSTR("inputDelayFrames"), ur_engine_input_delay_frames());
    return d;
}

static void applyConfigDictionary(CFDictionaryRef d) {
    int req, tpr, direct;
    uint32_t outM, inM;
    ur_engine_get_config(&req, &tpr, &outM, &inM, &direct);
    ur_engine_set_direct(dictInt(d, CFSTR("direct"), direct));
    uint32_t g, sd, sp;
    ur_engine_get_timing(&g, &sd, &sp);
    ur_engine_set_timing((uint32_t)dictInt(d, CFSTR("guardMicroframes"), (int)g),
                         (uint32_t)dictInt(d, CFSTR("scanDelayMicroframes"), (int)sd),
                         (uint32_t)dictInt(d, CFSTR("scanPeriodUs"), (int)sp));
    ur_engine_configure(dictInt(d, CFSTR("requests"), req), dictInt(d, CFSTR("microframes"), tpr),
                        (uint32_t)dictInt(d, CFSTR("outputMargin"), (int)outM),
                        (uint32_t)dictInt(d, CFSTR("inputMargin"), (int)inM));
    ur_engine_get_config(&req, &tpr, &outM, &inM, &direct);
    os_log(gLog, "configuration: %d requests x %d microframes, margins %u/%u, direct %d; output lead %u, input delay %u",
           req, tpr, outM, inM, direct, ur_engine_output_lead_frames(), ur_engine_input_delay_frames());
}

static CFDictionaryRef copyStatsDictionary(void) {
    UREngineStats st;
    ur_engine_get_stats(&st);
    CFMutableDictionaryRef d = newDict();
    dictSet(d, CFSTR("running"), ur_engine_is_running());
    dictSet(d, CFSTR("hostPid"), getpid());
    dictSet(d, CFSTR("inputFrames"), st.inputFrames);
    dictSet(d, CFSTR("inputMissing"), st.inputMissing);
    dictSet(d, CFSTR("outputLate"), st.outputLate);
    dictSet(d, CFSTR("emptyPackets"), st.emptyPackets);
    dictSet(d, CFSTR("inErrors"), st.inErrors);
    dictSet(d, CFSTR("outErrors"), st.outErrors);
    dictSet(d, CFSTR("resyncs"), st.resyncs);
    dictSet(d, CFSTR("gapFrames"), st.gapFrames);
    dictSet(d, CFSTR("zeroTimestamps"), st.zeroTimestamps);
    dictSet(d, CFSTR("measuredRate"), st.measuredRate);
    dictSet(d, CFSTR("inLagMaxMs"), st.inLagMaxMs);
    dictSet(d, CFSTR("outLagMaxMs"), st.outLagMaxMs);
    dictSetArray(d, CFSTR("inLagHist"), st.inLagHist, UR_LAG_BUCKETS);
    dictSetArray(d, CFSTR("outLagHist"), st.outLagHist, UR_LAG_BUCKETS);
    dictSet(d, CFSTR("directFrames"), st.directFrames);
    dictSet(d, CFSTR("ringFrames"), st.ringFrames);
    dictSet(d, CFSTR("races"), st.races);
    dictSet(d, CFSTR("scannerPriority"), st.scannerPriority);
    dictSet(d, CFSTR("callbackPriority"), st.callbackPriority);
    dictSet(d, CFSTR("callbackPolicy"), st.callbackPolicy);
    for (int i = 0; i < UR_PROF_N; i++) {
        char key[48];
        snprintf(key, sizeof key, "prof.%s.count", ur_engine_prof_name(i));
        CFStringRef k = CFStringCreateWithCString(NULL, key, kCFStringEncodingUTF8);
        dictSet(d, k, st.profCount[i]);
        CFRelease(k);
        snprintf(key, sizeof key, "prof.%s.us", ur_engine_prof_name(i));
        k = CFStringCreateWithCString(NULL, key, kCFStringEncodingUTF8);
        dictSet(d, k, st.profUs[i]);
        CFRelease(k);
    }
    return d;
}

// Volume: linear in dB from kVolumeMinDB (scalar 0 is silence) to kVolumeMaxDB.
static float clamp01(float v) { return v < 0.0f ? 0.0f : (v > 1.0f ? 1.0f : v); }
static float scalarToDB(float s) { return kVolumeMinDB + clamp01(s) * (kVolumeMaxDB - kVolumeMinDB); }
static float dbToScalar(float db) {
    if (db < kVolumeMinDB) db = kVolumeMinDB;
    if (db > kVolumeMaxDB) db = kVolumeMaxDB;
    return (db - kVolumeMinDB) / (kVolumeMaxDB - kVolumeMinDB);
}

static void updateGain(void) {
    float s = atomic_load(&gVolumeScalar);
    float g = (atomic_load(&gMute) || s <= 0.0f) ? 0.0f : powf(10.0f, scalarToDB(s) / 20.0f);
    atomic_store(&gTargetGain, g);
}

static void saveVolume(void) {
    if (!gHost) return;
    CFMutableDictionaryRef d = newDict();
    dictSet(d, CFSTR("volume"), atomic_load(&gVolumeScalar));
    dictSet(d, CFSTR("mute"), atomic_load(&gMute));
    gHost->WriteToStorage(gHost, kQCVolumeStorageKey, d);
    CFRelease(d);
}

static void loadStored(void) {
    CFPropertyListRef stored = NULL;
    if (gHost->CopyFromStorage(gHost, kQCStorageKey, &stored) == 0 && stored) {
        if (CFGetTypeID(stored) == CFDictionaryGetTypeID()) applyConfigDictionary(stored);
        CFRelease(stored);
    }
    stored = NULL;
    if (gHost->CopyFromStorage(gHost, kQCVolumeStorageKey, &stored) == 0 && stored) {
        if (CFGetTypeID(stored) == CFDictionaryGetTypeID()) {
            atomic_store(&gVolumeScalar, clamp01((float)dictDouble(stored, CFSTR("volume"), 1.0)));
            atomic_store(&gMute, dictInt(stored, CFSTR("mute"), 0) != 0);
        }
        CFRelease(stored);
    }
    updateGain();
    stored = NULL;
    if (gHost->CopyFromStorage(gHost, kQCRateStorageKey, &stored) == 0 && stored) {
        SInt32 r = 0;
        if (CFGetTypeID(stored) == CFNumberGetTypeID() && CFNumberGetValue(stored, kCFNumberSInt32Type, &r))
            ur_engine_set_rate((uint32_t)r);   // ignored if the rate is not supported
        CFRelease(stored);
    }
}

static void saveRate(void) {
    if (!gHost) return;
    SInt32 r = (SInt32)ur_engine_get_rate();
    CFNumberRef n = CFNumberCreate(NULL, kCFNumberSInt32Type, &r);
    gHost->WriteToStorage(gHost, kQCRateStorageKey, n);
    CFRelease(n);
}

// Ask the host for a configuration change to a new nominal sample rate.
static OSStatus requestRate(Float64 rate) {
    UInt32 r = (UInt32)llround(rate);
    if (fabs(rate - r) > 0.5 || !ur_engine_rate_supported(r)) return kAudioDeviceUnsupportedFormatError;
    const uint32_t ext = atomic_load(&gExternalRate);
    if (ext && r != ext) return kAudioDeviceUnsupportedFormatError;   // the COAXIAL IN clock sets the rate
    if (r == ur_engine_get_rate()) return 0;
    pthread_mutex_lock(&gStateLock);
    gPendingRate = r;
    pthread_mutex_unlock(&gStateLock);
    if (gHost) gHost->RequestDeviceConfigurationChange(gHost, kObjectID_Device, 2, NULL);
    return 0;
}

static void notify(AudioObjectID obj, AudioObjectPropertySelector sel1, AudioObjectPropertySelector sel2) {
    if (!gHost || !gHostReady) return;
    AudioObjectPropertyAddress a[2] = {
        {sel1, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain},
        {sel2, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain},
    };
    gHost->PropertiesChanged(gHost, obj, sel2 ? 2 : 1, a);
}

// The UR44 (Yamaha mode) connected or disconnected: show or remove the device.
static void presenceChanged(bool present) {
    bool was = atomic_exchange(&gPresent, present);
    if (was == present) return;
    os_log(gLog, "device %{public}s", present ? "added" : "removed");
    pthread_mutex_lock(&gStateLock);
    if (!present && ur_engine_is_running()) {
        ur_engine_stop();   // the device is gone: the drain completes quickly
    } else if (present && gIOClients > 0 && !ur_engine_is_running()) {
        int rc = ur_engine_start();
        if (rc) os_log_error(gLog, "reconnect: the USB engine did not start (%d)", rc);
    }
    pthread_mutex_unlock(&gStateLock);
    notify(kObjectID_PlugIn, kAudioPlugInPropertyDeviceList, kAudioObjectPropertyOwnedObjects);
}

static bool objectExists(AudioObjectID obj) {
    if (obj == kObjectID_PlugIn) return true;
    if (obj >= kObjectID_Device && obj <= kObjectID_Mute_Output) return atomic_load(&gPresent);
    return false;
}

#pragma mark Interface

static HRESULT UR_QueryInterface(void *inDriver, REFIID inUUID, LPVOID *outInterface);
static ULONG UR_AddRef(void *inDriver);
static ULONG UR_Release(void *inDriver);
static OSStatus UR_Initialize(AudioServerPlugInDriverRef inDriver, AudioServerPlugInHostRef inHost);
static OSStatus UR_CreateDevice(AudioServerPlugInDriverRef inDriver, CFDictionaryRef inDescription,
                                const AudioServerPlugInClientInfo *inClientInfo, AudioObjectID *outDeviceObjectID);
static OSStatus UR_DestroyDevice(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID);
static OSStatus UR_AddDeviceClient(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID,
                                   const AudioServerPlugInClientInfo *inClientInfo);
static OSStatus UR_RemoveDeviceClient(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID,
                                      const AudioServerPlugInClientInfo *inClientInfo);
static OSStatus UR_PerformDeviceConfigurationChange(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID,
                                                    UInt64 inChangeAction, void *inChangeInfo);
static OSStatus UR_AbortDeviceConfigurationChange(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID,
                                                  UInt64 inChangeAction, void *inChangeInfo);
static Boolean UR_HasProperty(AudioServerPlugInDriverRef inDriver, AudioObjectID inObjectID, pid_t inClientProcessID,
                              const AudioObjectPropertyAddress *inAddress);
static OSStatus UR_IsPropertySettable(AudioServerPlugInDriverRef inDriver, AudioObjectID inObjectID,
                                      pid_t inClientProcessID, const AudioObjectPropertyAddress *inAddress,
                                      Boolean *outIsSettable);
static OSStatus UR_GetPropertyDataSize(AudioServerPlugInDriverRef inDriver, AudioObjectID inObjectID,
                                       pid_t inClientProcessID, const AudioObjectPropertyAddress *inAddress,
                                       UInt32 inQualifierDataSize, const void *inQualifierData, UInt32 *outDataSize);
static OSStatus UR_GetPropertyData(AudioServerPlugInDriverRef inDriver, AudioObjectID inObjectID,
                                   pid_t inClientProcessID, const AudioObjectPropertyAddress *inAddress,
                                   UInt32 inQualifierDataSize, const void *inQualifierData, UInt32 inDataSize,
                                   UInt32 *outDataSize, void *outData);
static OSStatus UR_SetPropertyData(AudioServerPlugInDriverRef inDriver, AudioObjectID inObjectID,
                                   pid_t inClientProcessID, const AudioObjectPropertyAddress *inAddress,
                                   UInt32 inQualifierDataSize, const void *inQualifierData, UInt32 inDataSize,
                                   const void *inData);
static OSStatus UR_StartIO(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, UInt32 inClientID);
static OSStatus UR_StopIO(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, UInt32 inClientID);
static OSStatus UR_GetZeroTimeStamp(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID,
                                    UInt32 inClientID, Float64 *outSampleTime, UInt64 *outHostTime, UInt64 *outSeed);
static OSStatus UR_WillDoIOOperation(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID,
                                     UInt32 inClientID, UInt32 inOperationID, Boolean *outWillDo,
                                     Boolean *outWillDoInPlace);
static OSStatus UR_BeginIOOperation(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID,
                                    UInt32 inClientID, UInt32 inOperationID, UInt32 inIOBufferFrameSize,
                                    const AudioServerPlugInIOCycleInfo *inIOCycleInfo);
static OSStatus UR_DoIOOperation(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID,
                                 AudioObjectID inStreamObjectID, UInt32 inClientID, UInt32 inOperationID,
                                 UInt32 inIOBufferFrameSize, const AudioServerPlugInIOCycleInfo *inIOCycleInfo,
                                 void *ioMainBuffer, void *ioSecondaryBuffer);
static OSStatus UR_EndIOOperation(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID,
                                  UInt32 inClientID, UInt32 inOperationID, UInt32 inIOBufferFrameSize,
                                  const AudioServerPlugInIOCycleInfo *inIOCycleInfo);

static AudioServerPlugInDriverInterface gInterface = {
    NULL,
    UR_QueryInterface,
    UR_AddRef,
    UR_Release,
    UR_Initialize,
    UR_CreateDevice,
    UR_DestroyDevice,
    UR_AddDeviceClient,
    UR_RemoveDeviceClient,
    UR_PerformDeviceConfigurationChange,
    UR_AbortDeviceConfigurationChange,
    UR_HasProperty,
    UR_IsPropertySettable,
    UR_GetPropertyDataSize,
    UR_GetPropertyData,
    UR_SetPropertyData,
    UR_StartIO,
    UR_StopIO,
    UR_GetZeroTimeStamp,
    UR_WillDoIOOperation,
    UR_BeginIOOperation,
    UR_DoIOOperation,
    UR_EndIOOperation,
};
static AudioServerPlugInDriverInterface *gInterfacePtr = &gInterface;
static AudioServerPlugInDriverRef gDriverRef = &gInterfacePtr;

// Factory function (named in Info.plist, CFPlugInFactories).
__attribute__((visibility("default"))) void *UR44_Create(CFAllocatorRef inAllocator, CFUUIDRef inRequestedTypeUUID) {
    (void)inAllocator;
    if (!CFEqual(inRequestedTypeUUID, kAudioServerPlugInTypeUUID)) return NULL;
    if (!gLog) gLog = os_log_create("local.ur44.driver", "driver");
    os_log(gLog, "plug-in created");
    return gDriverRef;
}

static HRESULT UR_QueryInterface(void *inDriver, REFIID inUUID, LPVOID *outInterface) {
    if (inDriver != gDriverRef || !outInterface) return kAudioHardwareBadObjectError;
    CFUUIDRef requested = CFUUIDCreateFromUUIDBytes(NULL, inUUID);
    HRESULT result = E_NOINTERFACE;
    if (CFEqual(requested, IUnknownUUID) || CFEqual(requested, kAudioServerPlugInDriverInterfaceUUID)) {
        pthread_mutex_lock(&gStateLock);
        gRefCount++;
        pthread_mutex_unlock(&gStateLock);
        *outInterface = gDriverRef;
        result = S_OK;
    }
    CFRelease(requested);
    return result;
}

static ULONG UR_AddRef(void *inDriver) {
    if (inDriver != gDriverRef) return 0;
    pthread_mutex_lock(&gStateLock);
    ULONG r = ++gRefCount;
    pthread_mutex_unlock(&gStateLock);
    return r;
}

static ULONG UR_Release(void *inDriver) {
    if (inDriver != gDriverRef) return 0;
    pthread_mutex_lock(&gStateLock);
    if (gRefCount > 0) gRefCount--;
    ULONG r = gRefCount;
    pthread_mutex_unlock(&gStateLock);
    return r;
}

static OSStatus UR_Initialize(AudioServerPlugInDriverRef inDriver, AudioServerPlugInHostRef inHost) {
    if (inDriver != gDriverRef) return kAudioHardwareBadObjectError;
    gHost = inHost;
    if (gHost) loadStored();
    ur_engine_watch_device(presenceChanged);   // sets gPresent before it returns
    gHostReady = true;
    os_log(gLog, "plug-in initialized; UR44 %{public}s", atomic_load(&gPresent) ? "present" : "not present");
    return kAudioHardwareNoError;
}

static OSStatus UR_CreateDevice(AudioServerPlugInDriverRef inDriver, CFDictionaryRef inDescription,
                                const AudioServerPlugInClientInfo *inClientInfo, AudioObjectID *outDeviceObjectID) {
    (void)inDriver; (void)inDescription; (void)inClientInfo; (void)outDeviceObjectID;
    return kAudioHardwareUnsupportedOperationError;
}

static OSStatus UR_DestroyDevice(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID) {
    (void)inDriver; (void)inDeviceObjectID;
    return kAudioHardwareUnsupportedOperationError;
}

static OSStatus UR_AddDeviceClient(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID,
                                   const AudioServerPlugInClientInfo *inClientInfo) {
    (void)inClientInfo;
    if (inDriver != gDriverRef || inDeviceObjectID != kObjectID_Device) return kAudioHardwareBadObjectError;
    return kAudioHardwareNoError;
}

static OSStatus UR_RemoveDeviceClient(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID,
                                      const AudioServerPlugInClientInfo *inClientInfo) {
    (void)inClientInfo;
    if (inDriver != gDriverRef || inDeviceObjectID != kObjectID_Device) return kAudioHardwareBadObjectError;
    return kAudioHardwareNoError;
}

static OSStatus UR_PerformDeviceConfigurationChange(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID,
                                                    UInt64 inChangeAction, void *inChangeInfo) {
    (void)inChangeAction; (void)inChangeInfo;
    if (inDriver != gDriverRef || inDeviceObjectID != kObjectID_Device) return kAudioHardwareBadObjectError;
    pthread_mutex_lock(&gStateLock);
    if (gPendingConfig || gPendingRate) {
        // The engine can change its queue and its rate only while it does not run: stop, configure,
        // start again.
        bool running = ur_engine_is_running();
        if (running) ur_engine_stop();
        if (gPendingConfig) {
            applyConfigDictionary(gPendingConfig);
            CFRelease(gPendingConfig);
            gPendingConfig = NULL;
        }
        if (gPendingRate) {
            if (ur_engine_set_rate(gPendingRate)) {
                os_log(gLog, "nominal sample rate: %u Hz", gPendingRate);
                saveRate();
            }
            gPendingRate = 0;
        }
        if (running && gIOClients > 0) {
            int rc = ur_engine_start();
            if (rc) os_log_error(gLog, "configuration change: the USB engine did not start again (%d)", rc);
        }
    }
    pthread_mutex_unlock(&gStateLock);
    return kAudioHardwareNoError;
}

static OSStatus UR_AbortDeviceConfigurationChange(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID,
                                                  UInt64 inChangeAction, void *inChangeInfo) {
    (void)inChangeAction; (void)inChangeInfo;
    if (inDriver != gDriverRef || inDeviceObjectID != kObjectID_Device) return kAudioHardwareBadObjectError;
    pthread_mutex_lock(&gStateLock);
    if (gPendingConfig) { CFRelease(gPendingConfig); gPendingConfig = NULL; }
    gPendingRate = 0;
    pthread_mutex_unlock(&gStateLock);
    return kAudioHardwareNoError;
}

#pragma mark Properties

static AudioStreamBasicDescription streamFormat(AudioObjectID stream) {
    UInt32 ch = stream == kObjectID_Stream_Input ? UR_IN_CHANNELS : UR_OUT_CHANNELS;
    AudioStreamBasicDescription f;
    memset(&f, 0, sizeof(f));
    f.mSampleRate = ur_engine_get_rate();
    f.mFormatID = kAudioFormatLinearPCM;
    f.mFormatFlags = kAudioFormatFlagIsFloat | kAudioFormatFlagsNativeEndian | kAudioFormatFlagIsPacked;
    f.mBytesPerPacket = 4 * ch;
    f.mFramesPerPacket = 1;
    f.mBytesPerFrame = 4 * ch;
    f.mChannelsPerFrame = ch;
    f.mBitsPerChannel = 32;
    return f;
}

// Objects owned by the device in a scope (streams first, then controls).
static UInt32 deviceObjects(AudioObjectPropertyScope scope, bool withControls, AudioObjectID out[4]) {
    UInt32 n = 0;
    bool global = scope == kAudioObjectPropertyScopeGlobal;
    if (global || scope == kAudioObjectPropertyScopeInput) out[n++] = kObjectID_Stream_Input;
    if (global || scope == kAudioObjectPropertyScopeOutput) out[n++] = kObjectID_Stream_Output;
    if (withControls && (global || scope == kAudioObjectPropertyScopeOutput)) {
        out[n++] = kObjectID_Volume_Output;
        out[n++] = kObjectID_Mute_Output;
    }
    return n;
}

static UInt32 channelsInScope(AudioObjectPropertyScope scope) {
    return scope == kAudioObjectPropertyScopeInput ? UR_IN_CHANNELS : UR_OUT_CHANNELS;
}

static UInt32 layoutSize(AudioObjectPropertyScope scope) {
    return (UInt32)(offsetof(AudioChannelLayout, mChannelDescriptions) +
                    channelsInScope(scope) * sizeof(AudioChannelDescription));
}

static bool hasElementName(const AudioObjectPropertyAddress *a) {
    return (a->mScope == kAudioObjectPropertyScopeInput || a->mScope == kAudioObjectPropertyScopeOutput) &&
           a->mElement >= 1 && a->mElement <= channelsInScope(a->mScope);
}

// Size of a property, or an error if the object does not have it.
static OSStatus propertySize(AudioObjectID obj, const AudioObjectPropertyAddress *a, UInt32 *size) {
    AudioObjectID tmp[4];
    if (!objectExists(obj)) return kAudioHardwareBadObjectError;
    switch (obj) {
    case kObjectID_PlugIn:
        switch (a->mSelector) {
        case kAudioObjectPropertyBaseClass:
        case kAudioObjectPropertyClass: *size = sizeof(AudioClassID); return 0;
        case kAudioObjectPropertyOwner:
        case kAudioPlugInPropertyTranslateUIDToDevice: *size = sizeof(AudioObjectID); return 0;
        case kAudioObjectPropertyManufacturer:
        case kAudioPlugInPropertyResourceBundle: *size = sizeof(CFStringRef); return 0;
        case kAudioObjectPropertyOwnedObjects:
        case kAudioPlugInPropertyDeviceList: *size = atomic_load(&gPresent) ? sizeof(AudioObjectID) : 0; return 0;
        }
        break;
    case kObjectID_Device:
        switch (a->mSelector) {
        case kAudioObjectPropertyBaseClass:
        case kAudioObjectPropertyClass: *size = sizeof(AudioClassID); return 0;
        case kAudioObjectPropertyOwner:
        case kAudioDevicePropertyRelatedDevices: *size = sizeof(AudioObjectID); return 0;
        case kAudioObjectPropertyName:
        case kAudioObjectPropertyManufacturer:
        case kAudioDevicePropertyDeviceUID:
        case kAudioDevicePropertyModelUID: *size = sizeof(CFStringRef); return 0;
        case kAudioObjectPropertyElementName:
            if (!hasElementName(a)) return kAudioHardwareUnknownPropertyError;
            *size = sizeof(CFStringRef);
            return 0;
        case kAudioDevicePropertyTransportType:
        case kAudioDevicePropertyClockDomain:
        case kAudioDevicePropertyDeviceIsAlive:
        case kAudioDevicePropertyDeviceIsRunning:
        case kAudioDevicePropertyDeviceCanBeDefaultDevice:
        case kAudioDevicePropertyDeviceCanBeDefaultSystemDevice:
        case kAudioDevicePropertyLatency:
        case kAudioDevicePropertySafetyOffset:
        case kAudioDevicePropertyIsHidden:
        case kAudioDevicePropertyZeroTimeStampPeriod: *size = sizeof(UInt32); return 0;
        case kAudioObjectPropertyOwnedObjects: *size = deviceObjects(a->mScope, true, tmp) * sizeof(AudioObjectID); return 0;
        case kAudioDevicePropertyStreams: *size = deviceObjects(a->mScope, false, tmp) * sizeof(AudioObjectID); return 0;
        case kAudioObjectPropertyControlList: *size = 2 * sizeof(AudioObjectID); return 0;
        case kAudioDevicePropertyNominalSampleRate: *size = sizeof(Float64); return 0;
        case kAudioDevicePropertyAvailableNominalSampleRates:
            *size = (atomic_load(&gExternalRate) ? 1 : UR_NUM_RATES) * sizeof(AudioValueRange);
            return 0;
        case kAudioDevicePropertyPreferredChannelsForStereo: *size = 2 * sizeof(UInt32); return 0;
        case kAudioDevicePropertyPreferredChannelLayout: *size = layoutSize(a->mScope); return 0;
        case kAudioObjectPropertyCustomPropertyInfoList: *size = 2 * sizeof(AudioServerPlugInCustomPropertyInfo); return 0;
        case kQCPropertyConfig:
        case kQCPropertyStats: *size = sizeof(CFPropertyListRef); return 0;
        }
        break;
    case kObjectID_Stream_Input:
    case kObjectID_Stream_Output:
        switch (a->mSelector) {
        case kAudioObjectPropertyBaseClass:
        case kAudioObjectPropertyClass: *size = sizeof(AudioClassID); return 0;
        case kAudioObjectPropertyOwner: *size = sizeof(AudioObjectID); return 0;
        case kAudioObjectPropertyOwnedObjects: *size = 0; return 0;
        case kAudioStreamPropertyIsActive:
        case kAudioStreamPropertyDirection:
        case kAudioStreamPropertyTerminalType:
        case kAudioStreamPropertyStartingChannel:
        case kAudioStreamPropertyLatency: *size = sizeof(UInt32); return 0;
        case kAudioStreamPropertyVirtualFormat:
        case kAudioStreamPropertyPhysicalFormat: *size = sizeof(AudioStreamBasicDescription); return 0;
        case kAudioStreamPropertyAvailableVirtualFormats:
        case kAudioStreamPropertyAvailablePhysicalFormats: *size = UR_NUM_RATES * sizeof(AudioStreamRangedDescription); return 0;
        }
        break;
    case kObjectID_Volume_Output:
        switch (a->mSelector) {
        case kAudioObjectPropertyBaseClass:
        case kAudioObjectPropertyClass: *size = sizeof(AudioClassID); return 0;
        case kAudioObjectPropertyOwner: *size = sizeof(AudioObjectID); return 0;
        case kAudioObjectPropertyOwnedObjects: *size = 0; return 0;
        case kAudioControlPropertyScope: *size = sizeof(AudioObjectPropertyScope); return 0;
        case kAudioControlPropertyElement: *size = sizeof(AudioObjectPropertyElement); return 0;
        case kAudioLevelControlPropertyScalarValue:
        case kAudioLevelControlPropertyDecibelValue:
        case kAudioLevelControlPropertyConvertScalarToDecibels:
        case kAudioLevelControlPropertyConvertDecibelsToScalar: *size = sizeof(Float32); return 0;
        case kAudioLevelControlPropertyDecibelRange: *size = sizeof(AudioValueRange); return 0;
        }
        break;
    case kObjectID_Mute_Output:
        switch (a->mSelector) {
        case kAudioObjectPropertyBaseClass:
        case kAudioObjectPropertyClass: *size = sizeof(AudioClassID); return 0;
        case kAudioObjectPropertyOwner: *size = sizeof(AudioObjectID); return 0;
        case kAudioObjectPropertyOwnedObjects: *size = 0; return 0;
        case kAudioControlPropertyScope: *size = sizeof(AudioObjectPropertyScope); return 0;
        case kAudioControlPropertyElement: *size = sizeof(AudioObjectPropertyElement); return 0;
        case kAudioBooleanControlPropertyValue: *size = sizeof(UInt32); return 0;
        }
        break;
    }
    return kAudioHardwareUnknownPropertyError;
}

static Boolean UR_HasProperty(AudioServerPlugInDriverRef inDriver, AudioObjectID inObjectID, pid_t inClientProcessID,
                              const AudioObjectPropertyAddress *inAddress) {
    (void)inClientProcessID;
    UInt32 size;
    return inDriver == gDriverRef && inAddress && propertySize(inObjectID, inAddress, &size) == 0;
}

static OSStatus UR_IsPropertySettable(AudioServerPlugInDriverRef inDriver, AudioObjectID inObjectID,
                                      pid_t inClientProcessID, const AudioObjectPropertyAddress *inAddress,
                                      Boolean *outIsSettable) {
    (void)inClientProcessID;
    if (inDriver != gDriverRef || !inAddress || !outIsSettable) return kAudioHardwareIllegalOperationError;
    UInt32 size;
    OSStatus r = propertySize(inObjectID, inAddress, &size);
    if (r) return r;
    const AudioObjectPropertySelector sel = inAddress->mSelector;
    switch (inObjectID) {
    case kObjectID_Stream_Input:
    case kObjectID_Stream_Output:
        *outIsSettable = sel == kAudioStreamPropertyIsActive || sel == kAudioStreamPropertyVirtualFormat ||
                         sel == kAudioStreamPropertyPhysicalFormat;
        break;
    case kObjectID_Device: *outIsSettable = sel == kQCPropertyConfig || sel == kAudioDevicePropertyNominalSampleRate; break;
    case kObjectID_Volume_Output:
        *outIsSettable = sel == kAudioLevelControlPropertyScalarValue || sel == kAudioLevelControlPropertyDecibelValue;
        break;
    case kObjectID_Mute_Output: *outIsSettable = sel == kAudioBooleanControlPropertyValue; break;
    default: *outIsSettable = false; break;
    }
    return 0;
}

static OSStatus UR_GetPropertyDataSize(AudioServerPlugInDriverRef inDriver, AudioObjectID inObjectID,
                                       pid_t inClientProcessID, const AudioObjectPropertyAddress *inAddress,
                                       UInt32 inQualifierDataSize, const void *inQualifierData, UInt32 *outDataSize) {
    (void)inClientProcessID; (void)inQualifierDataSize; (void)inQualifierData;
    if (inDriver != gDriverRef || !inAddress || !outDataSize) return kAudioHardwareIllegalOperationError;
    return propertySize(inObjectID, inAddress, outDataSize);
}

#define NEED(sz) do { if (inDataSize < (sz)) return kAudioHardwareBadPropertySizeError; *outDataSize = (sz); } while (0)

static void copyIDs(const AudioObjectID *ids, UInt32 n, UInt32 inDataSize, UInt32 *outDataSize, void *outData) {
    UInt32 fit = inDataSize / sizeof(AudioObjectID);
    if (fit < n) n = fit;
    memcpy(outData, ids, n * sizeof(AudioObjectID));
    *outDataSize = n * sizeof(AudioObjectID);
}

static OSStatus UR_GetPropertyData(AudioServerPlugInDriverRef inDriver, AudioObjectID inObjectID,
                                   pid_t inClientProcessID, const AudioObjectPropertyAddress *inAddress,
                                   UInt32 inQualifierDataSize, const void *inQualifierData, UInt32 inDataSize,
                                   UInt32 *outDataSize, void *outData) {
    (void)inClientProcessID;
    if (inDriver != gDriverRef || !inAddress || !outDataSize || !outData) return kAudioHardwareIllegalOperationError;
    UInt32 size;
    OSStatus r = propertySize(inObjectID, inAddress, &size);
    if (r) return r;
    const AudioObjectPropertySelector sel = inAddress->mSelector;

    switch (inObjectID) {
    case kObjectID_PlugIn:
        switch (sel) {
        case kAudioObjectPropertyBaseClass: NEED(sizeof(AudioClassID)); *(AudioClassID *)outData = kAudioObjectClassID; return 0;
        case kAudioObjectPropertyClass: NEED(sizeof(AudioClassID)); *(AudioClassID *)outData = kAudioPlugInClassID; return 0;
        case kAudioObjectPropertyOwner: NEED(sizeof(AudioObjectID)); *(AudioObjectID *)outData = kAudioObjectUnknown; return 0;
        case kAudioObjectPropertyManufacturer: NEED(sizeof(CFStringRef)); *(CFStringRef *)outData = kManufacturer; return 0;
        case kAudioPlugInPropertyResourceBundle: NEED(sizeof(CFStringRef)); *(CFStringRef *)outData = CFSTR(""); return 0;
        case kAudioObjectPropertyOwnedObjects:
        case kAudioPlugInPropertyDeviceList: {
            AudioObjectID dev = kObjectID_Device;
            copyIDs(&dev, atomic_load(&gPresent) ? 1 : 0, inDataSize, outDataSize, outData);
            return 0;
        }
        case kAudioPlugInPropertyTranslateUIDToDevice: {
            NEED(sizeof(AudioObjectID));
            AudioObjectID id = kAudioObjectUnknown;
            if (inQualifierDataSize == sizeof(CFStringRef) && inQualifierData && atomic_load(&gPresent)) {
                CFStringRef uid = *(const CFStringRef *)inQualifierData;
                if (uid && CFStringCompare(uid, kDevice_UID, 0) == kCFCompareEqualTo) id = kObjectID_Device;
            }
            *(AudioObjectID *)outData = id;
            return 0;
        }
        }
        break;

    case kObjectID_Device:
        switch (sel) {
        case kAudioObjectPropertyBaseClass: NEED(sizeof(AudioClassID)); *(AudioClassID *)outData = kAudioObjectClassID; return 0;
        case kAudioObjectPropertyClass: NEED(sizeof(AudioClassID)); *(AudioClassID *)outData = kAudioDeviceClassID; return 0;
        case kAudioObjectPropertyOwner: NEED(sizeof(AudioObjectID)); *(AudioObjectID *)outData = kObjectID_PlugIn; return 0;
        case kAudioObjectPropertyName: NEED(sizeof(CFStringRef)); *(CFStringRef *)outData = kDevice_Name; return 0;
        case kAudioObjectPropertyManufacturer: NEED(sizeof(CFStringRef)); *(CFStringRef *)outData = kManufacturer; return 0;
        case kAudioDevicePropertyDeviceUID: NEED(sizeof(CFStringRef)); *(CFStringRef *)outData = kDevice_UID; return 0;
        case kAudioDevicePropertyModelUID: NEED(sizeof(CFStringRef)); *(CFStringRef *)outData = kDevice_ModelUID; return 0;
        case kAudioObjectPropertyElementName: {
            NEED(sizeof(CFStringRef));
            const char *name = inAddress->mScope == kAudioObjectPropertyScopeInput ? kInputNames[inAddress->mElement - 1]
                                                                                   : kOutputNames[inAddress->mElement - 1];
            *(CFStringRef *)outData = CFStringCreateWithCString(NULL, name, kCFStringEncodingUTF8);
            return 0;
        }
        case kAudioDevicePropertyTransportType: NEED(sizeof(UInt32)); *(UInt32 *)outData = kAudioDeviceTransportTypeUSB; return 0;
        case kAudioDevicePropertyRelatedDevices: NEED(sizeof(AudioObjectID)); *(AudioObjectID *)outData = kObjectID_Device; return 0;
        case kAudioDevicePropertyClockDomain: NEED(sizeof(UInt32)); *(UInt32 *)outData = 0; return 0;
        case kAudioDevicePropertyDeviceIsAlive: NEED(sizeof(UInt32)); *(UInt32 *)outData = atomic_load(&gPresent); return 0;
        case kAudioDevicePropertyDeviceIsRunning: {
            NEED(sizeof(UInt32));
            pthread_mutex_lock(&gStateLock);
            *(UInt32 *)outData = gIOClients > 0;
            pthread_mutex_unlock(&gStateLock);
            return 0;
        }
        case kAudioDevicePropertyDeviceCanBeDefaultDevice:
        case kAudioDevicePropertyDeviceCanBeDefaultSystemDevice: NEED(sizeof(UInt32)); *(UInt32 *)outData = 1; return 0;
        case kAudioDevicePropertyLatency: NEED(sizeof(UInt32)); *(UInt32 *)outData = kLatencyFrames; return 0;
        case kAudioDevicePropertySafetyOffset:
            NEED(sizeof(UInt32));
            *(UInt32 *)outData = inAddress->mScope == kAudioObjectPropertyScopeInput    ? ur_engine_input_delay_frames()
                                 : inAddress->mScope == kAudioObjectPropertyScopeOutput ? ur_engine_output_lead_frames()
                                                                                        : 0;
            return 0;
        case kAudioDevicePropertyIsHidden: NEED(sizeof(UInt32)); *(UInt32 *)outData = 0; return 0;
        case kAudioDevicePropertyZeroTimeStampPeriod: NEED(sizeof(UInt32)); *(UInt32 *)outData = UR_ZERO_PERIOD; return 0;
        case kAudioObjectPropertyOwnedObjects:
        case kAudioDevicePropertyStreams: {
            AudioObjectID ids[4];
            UInt32 n = deviceObjects(inAddress->mScope, sel == kAudioObjectPropertyOwnedObjects, ids);
            copyIDs(ids, n, inDataSize, outDataSize, outData);
            return 0;
        }
        case kAudioObjectPropertyControlList: {
            AudioObjectID ids[2] = {kObjectID_Volume_Output, kObjectID_Mute_Output};
            copyIDs(ids, 2, inDataSize, outDataSize, outData);
            return 0;
        }
        case kAudioDevicePropertyNominalSampleRate: NEED(sizeof(Float64)); *(Float64 *)outData = ur_engine_get_rate(); return 0;
        case kAudioDevicePropertyAvailableNominalSampleRates: {
            UInt32 n = inDataSize / sizeof(AudioValueRange);
            AudioValueRange *v = outData;
            const uint32_t ext = atomic_load(&gExternalRate);
            if (ext) {   // the COAXIAL IN clock sets the rate
                if (n > 1) n = 1;
                if (n) v[0].mMinimum = v[0].mMaximum = ext;
            } else {
                if (n > UR_NUM_RATES) n = UR_NUM_RATES;
                for (UInt32 i = 0; i < n; i++) v[i].mMinimum = v[i].mMaximum = ur_engine_rates[i];
            }
            *outDataSize = n * sizeof(AudioValueRange);
            return 0;
        }
        case kAudioDevicePropertyPreferredChannelsForStereo: {
            NEED(2 * sizeof(UInt32));
            ((UInt32 *)outData)[0] = 1;
            ((UInt32 *)outData)[1] = 2;
            return 0;
        }
        case kAudioDevicePropertyPreferredChannelLayout: {
            UInt32 ch = channelsInScope(inAddress->mScope);
            NEED(layoutSize(inAddress->mScope));
            AudioChannelLayout *l = outData;
            memset(l, 0, layoutSize(inAddress->mScope));
            l->mChannelLayoutTag = kAudioChannelLayoutTag_UseChannelDescriptions;
            l->mNumberChannelDescriptions = ch;
            static const AudioChannelLabel labels[4] = {kAudioChannelLabel_Left, kAudioChannelLabel_Right,
                                                         kAudioChannelLabel_Discrete_2, kAudioChannelLabel_Discrete_3};
            for (UInt32 i = 0; i < ch && i < 4; i++) l->mChannelDescriptions[i].mChannelLabel = labels[i];
            return 0;
        }
        case kAudioObjectPropertyCustomPropertyInfoList: {
            NEED(2 * sizeof(AudioServerPlugInCustomPropertyInfo));
            AudioServerPlugInCustomPropertyInfo *info = outData;
            info[0].mSelector = kQCPropertyConfig;
            info[1].mSelector = kQCPropertyStats;
            for (int i = 0; i < 2; i++) {
                info[i].mPropertyDataType = kAudioServerPlugInCustomPropertyDataTypeCFPropertyList;
                info[i].mQualifierDataType = kAudioServerPlugInCustomPropertyDataTypeNone;
            }
            return 0;
        }
        case kQCPropertyConfig: NEED(sizeof(CFPropertyListRef)); *(CFPropertyListRef *)outData = copyConfigDictionary(); return 0;
        case kQCPropertyStats: NEED(sizeof(CFPropertyListRef)); *(CFPropertyListRef *)outData = copyStatsDictionary(); return 0;
        }
        break;

    case kObjectID_Stream_Input:
    case kObjectID_Stream_Output: {
        const Boolean input = inObjectID == kObjectID_Stream_Input;
        switch (sel) {
        case kAudioObjectPropertyBaseClass: NEED(sizeof(AudioClassID)); *(AudioClassID *)outData = kAudioObjectClassID; return 0;
        case kAudioObjectPropertyClass: NEED(sizeof(AudioClassID)); *(AudioClassID *)outData = kAudioStreamClassID; return 0;
        case kAudioObjectPropertyOwner: NEED(sizeof(AudioObjectID)); *(AudioObjectID *)outData = kObjectID_Device; return 0;
        case kAudioObjectPropertyOwnedObjects: *outDataSize = 0; return 0;
        case kAudioStreamPropertyIsActive: {
            NEED(sizeof(UInt32));
            pthread_mutex_lock(&gStateLock);
            *(UInt32 *)outData = gStreamActive[input ? 0 : 1];
            pthread_mutex_unlock(&gStateLock);
            return 0;
        }
        case kAudioStreamPropertyDirection: NEED(sizeof(UInt32)); *(UInt32 *)outData = input ? 1 : 0; return 0;
        case kAudioStreamPropertyTerminalType:
            NEED(sizeof(UInt32));
            *(UInt32 *)outData = input ? kAudioStreamTerminalTypeMicrophone : kAudioStreamTerminalTypeHeadphones;
            return 0;
        case kAudioStreamPropertyStartingChannel: NEED(sizeof(UInt32)); *(UInt32 *)outData = 1; return 0;
        case kAudioStreamPropertyLatency: NEED(sizeof(UInt32)); *(UInt32 *)outData = 0; return 0;
        case kAudioStreamPropertyVirtualFormat:
        case kAudioStreamPropertyPhysicalFormat:
            NEED(sizeof(AudioStreamBasicDescription));
            *(AudioStreamBasicDescription *)outData = streamFormat(inObjectID);
            return 0;
        case kAudioStreamPropertyAvailableVirtualFormats:
        case kAudioStreamPropertyAvailablePhysicalFormats: {
            UInt32 n = inDataSize / sizeof(AudioStreamRangedDescription);
            if (n > UR_NUM_RATES) n = UR_NUM_RATES;
            AudioStreamRangedDescription *d = outData;
            for (UInt32 i = 0; i < n; i++) {
                d[i].mFormat = streamFormat(inObjectID);
                d[i].mFormat.mSampleRate = ur_engine_rates[i];
                d[i].mSampleRateRange.mMinimum = d[i].mSampleRateRange.mMaximum = ur_engine_rates[i];
            }
            *outDataSize = n * sizeof(AudioStreamRangedDescription);
            return 0;
        }
        }
        break;
    }

    case kObjectID_Volume_Output:
        switch (sel) {
        case kAudioObjectPropertyBaseClass: NEED(sizeof(AudioClassID)); *(AudioClassID *)outData = kAudioLevelControlClassID; return 0;
        case kAudioObjectPropertyClass: NEED(sizeof(AudioClassID)); *(AudioClassID *)outData = kAudioVolumeControlClassID; return 0;
        case kAudioObjectPropertyOwner: NEED(sizeof(AudioObjectID)); *(AudioObjectID *)outData = kObjectID_Device; return 0;
        case kAudioObjectPropertyOwnedObjects: *outDataSize = 0; return 0;
        case kAudioControlPropertyScope: NEED(sizeof(AudioObjectPropertyScope)); *(AudioObjectPropertyScope *)outData = kAudioObjectPropertyScopeOutput; return 0;
        case kAudioControlPropertyElement: NEED(sizeof(AudioObjectPropertyElement)); *(AudioObjectPropertyElement *)outData = kAudioObjectPropertyElementMain; return 0;
        case kAudioLevelControlPropertyScalarValue: NEED(sizeof(Float32)); *(Float32 *)outData = atomic_load(&gVolumeScalar); return 0;
        case kAudioLevelControlPropertyDecibelValue: NEED(sizeof(Float32)); *(Float32 *)outData = scalarToDB(atomic_load(&gVolumeScalar)); return 0;
        case kAudioLevelControlPropertyDecibelRange: {
            NEED(sizeof(AudioValueRange));
            AudioValueRange *v = outData;
            v->mMinimum = kVolumeMinDB;
            v->mMaximum = kVolumeMaxDB;
            return 0;
        }
        case kAudioLevelControlPropertyConvertScalarToDecibels:
            NEED(sizeof(Float32));
            *(Float32 *)outData = scalarToDB(*(Float32 *)outData);
            return 0;
        case kAudioLevelControlPropertyConvertDecibelsToScalar:
            NEED(sizeof(Float32));
            *(Float32 *)outData = dbToScalar(*(Float32 *)outData);
            return 0;
        }
        break;

    case kObjectID_Mute_Output:
        switch (sel) {
        case kAudioObjectPropertyBaseClass: NEED(sizeof(AudioClassID)); *(AudioClassID *)outData = kAudioBooleanControlClassID; return 0;
        case kAudioObjectPropertyClass: NEED(sizeof(AudioClassID)); *(AudioClassID *)outData = kAudioMuteControlClassID; return 0;
        case kAudioObjectPropertyOwner: NEED(sizeof(AudioObjectID)); *(AudioObjectID *)outData = kObjectID_Device; return 0;
        case kAudioObjectPropertyOwnedObjects: *outDataSize = 0; return 0;
        case kAudioControlPropertyScope: NEED(sizeof(AudioObjectPropertyScope)); *(AudioObjectPropertyScope *)outData = kAudioObjectPropertyScopeOutput; return 0;
        case kAudioControlPropertyElement: NEED(sizeof(AudioObjectPropertyElement)); *(AudioObjectPropertyElement *)outData = kAudioObjectPropertyElementMain; return 0;
        case kAudioBooleanControlPropertyValue: NEED(sizeof(UInt32)); *(UInt32 *)outData = atomic_load(&gMute); return 0;
        }
        break;
    }
    return kAudioHardwareUnknownPropertyError;
}

static OSStatus UR_SetPropertyData(AudioServerPlugInDriverRef inDriver, AudioObjectID inObjectID,
                                   pid_t inClientProcessID, const AudioObjectPropertyAddress *inAddress,
                                   UInt32 inQualifierDataSize, const void *inQualifierData, UInt32 inDataSize,
                                   const void *inData) {
    (void)inClientProcessID; (void)inQualifierDataSize; (void)inQualifierData;
    if (inDriver != gDriverRef || !inAddress || !inData) return kAudioHardwareIllegalOperationError;
    if (!objectExists(inObjectID)) return kAudioHardwareBadObjectError;
    const AudioObjectPropertySelector sel = inAddress->mSelector;

    switch (inObjectID) {
    case kObjectID_Device:
        if (sel == kQCPropertyConfig) {
            if (inDataSize != sizeof(CFPropertyListRef)) return kAudioHardwareBadPropertySizeError;
            CFPropertyListRef d = *(const CFPropertyListRef *)inData;
            if (!d || CFGetTypeID(d) != CFDictionaryGetTypeID()) return kAudioHardwareIllegalOperationError;
            pthread_mutex_lock(&gStateLock);
            if (gPendingConfig) CFRelease(gPendingConfig);
            gPendingConfig = CFDictionaryCreateCopy(NULL, d);
            pthread_mutex_unlock(&gStateLock);
            if (gHost) {
                gHost->WriteToStorage(gHost, kQCStorageKey, d);
                gHost->RequestDeviceConfigurationChange(gHost, kObjectID_Device, 1, NULL);
            }
            return 0;
        }
        if (sel == kAudioDevicePropertyNominalSampleRate) {
            if (inDataSize != sizeof(Float64)) return kAudioHardwareBadPropertySizeError;
            return requestRate(*(const Float64 *)inData);
        }
        return kAudioHardwareUnknownPropertyError;

    case kObjectID_Stream_Input:
    case kObjectID_Stream_Output:
        switch (sel) {
        case kAudioStreamPropertyIsActive:
            if (inDataSize != sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
            pthread_mutex_lock(&gStateLock);
            gStreamActive[inObjectID == kObjectID_Stream_Input ? 0 : 1] = *(const UInt32 *)inData != 0;
            pthread_mutex_unlock(&gStateLock);
            return 0;
        case kAudioStreamPropertyVirtualFormat:
        case kAudioStreamPropertyPhysicalFormat: {
            if (inDataSize != sizeof(AudioStreamBasicDescription)) return kAudioHardwareBadPropertySizeError;
            // One format at each supported rate: a new rate changes the rate of the device.
            const AudioStreamBasicDescription *f = inData;
            AudioStreamBasicDescription ours = streamFormat(inObjectID);
            if (f->mFormatID != ours.mFormatID || f->mChannelsPerFrame != ours.mChannelsPerFrame ||
                f->mBitsPerChannel != ours.mBitsPerChannel || f->mFormatFlags != ours.mFormatFlags)
                return kAudioDeviceUnsupportedFormatError;
            return requestRate(f->mSampleRate);
        }
        }
        return kAudioHardwareUnknownPropertyError;

    case kObjectID_Volume_Output: {
        if (inDataSize != sizeof(Float32)) return kAudioHardwareBadPropertySizeError;
        float v = *(const Float32 *)inData;
        if (sel == kAudioLevelControlPropertyScalarValue) atomic_store(&gVolumeScalar, clamp01(v));
        else if (sel == kAudioLevelControlPropertyDecibelValue) atomic_store(&gVolumeScalar, dbToScalar(v));
        else return kAudioHardwareUnknownPropertyError;
        updateGain();
        saveVolume();
        notify(kObjectID_Volume_Output, kAudioLevelControlPropertyScalarValue, kAudioLevelControlPropertyDecibelValue);
        return 0;
    }

    case kObjectID_Mute_Output:
        if (sel != kAudioBooleanControlPropertyValue) return kAudioHardwareUnknownPropertyError;
        if (inDataSize != sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
        atomic_store(&gMute, *(const UInt32 *)inData != 0);
        updateGain();
        saveVolume();
        notify(kObjectID_Mute_Output, kAudioBooleanControlPropertyValue, 0);
        return 0;
    }
    return kAudioHardwareUnknownPropertyError;
}

#pragma mark IO

static OSStatus UR_StartIO(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, UInt32 inClientID) {
    (void)inClientID;
    if (inDriver != gDriverRef || inDeviceObjectID != kObjectID_Device) return kAudioHardwareBadObjectError;
    if (!atomic_load(&gPresent)) return kAudioHardwareBadDeviceError;
    OSStatus r = 0;
    pthread_mutex_lock(&gStateLock);
    if (gIOClients == 0 || !ur_engine_is_running()) {
        int rc = ur_engine_start();
        if (rc) {
            os_log_error(gLog, "StartIO: the USB engine did not start (%d)", rc);
            r = kAudioHardwareNotRunningError;
        } else {
            os_log(gLog, "StartIO: USB engine running");
        }
    }
    if (r == 0) gIOClients++;
    pthread_mutex_unlock(&gStateLock);
    return r;
}

static OSStatus UR_StopIO(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, UInt32 inClientID) {
    (void)inClientID;
    if (inDriver != gDriverRef || inDeviceObjectID != kObjectID_Device) return kAudioHardwareBadObjectError;
    pthread_mutex_lock(&gStateLock);
    if (gIOClients > 0 && --gIOClients == 0) {
        UREngineStats st;
        ur_engine_get_stats(&st);
        ur_engine_stop();
        os_log(gLog, "StopIO: input frames %llu, missing %llu, output late %llu, empty packets %llu, "
                     "USB errors %llu/%llu, resyncs %llu, gap frames %llu, lag max %.2f/%.2f ms, "
                     "direct %llu, ring %llu, races %llu, callback priority %d, scanner priority %d",
               st.inputFrames, st.inputMissing, st.outputLate, st.emptyPackets, st.inErrors, st.outErrors,
               st.resyncs, st.gapFrames, st.inLagMaxMs, st.outLagMaxMs, st.directFrames, st.ringFrames, st.races,
               st.callbackPriority, st.scannerPriority);
    }
    pthread_mutex_unlock(&gStateLock);
    return 0;
}

static OSStatus UR_GetZeroTimeStamp(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID,
                                    UInt32 inClientID, Float64 *outSampleTime, UInt64 *outHostTime, UInt64 *outSeed) {
    (void)inClientID;
    if (inDriver != gDriverRef || inDeviceObjectID != kObjectID_Device) return kAudioHardwareBadObjectError;
    double s;
    uint64_t h, seed;
    if (!ur_engine_zero_timestamp(&s, &h, &seed)) return kAudioHardwareNotRunningError;
    *outSampleTime = s;
    *outHostTime = h;
    *outSeed = seed;
    return 0;
}

static OSStatus UR_WillDoIOOperation(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID,
                                     UInt32 inClientID, UInt32 inOperationID, Boolean *outWillDo,
                                     Boolean *outWillDoInPlace) {
    (void)inClientID;
    if (inDriver != gDriverRef || inDeviceObjectID != kObjectID_Device) return kAudioHardwareBadObjectError;
    Boolean will = inOperationID == kAudioServerPlugInIOOperationReadInput ||
                   inOperationID == kAudioServerPlugInIOOperationWriteMix;
    if (outWillDo) *outWillDo = will;
    if (outWillDoInPlace) *outWillDoInPlace = true;
    return 0;
}

static OSStatus UR_BeginIOOperation(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID,
                                    UInt32 inClientID, UInt32 inOperationID, UInt32 inIOBufferFrameSize,
                                    const AudioServerPlugInIOCycleInfo *inIOCycleInfo) {
    (void)inClientID; (void)inIOBufferFrameSize; (void)inIOCycleInfo;
    if (inDriver != gDriverRef || inDeviceObjectID != kObjectID_Device) return kAudioHardwareBadObjectError;
    return 0;
}

// Output volume and mute, with a linear ramp over the buffer (no zipper noise).
static void applyGain(float *buf, UInt32 frames) {
    const float target = atomic_load_explicit(&gTargetGain, memory_order_relaxed);
    const float start = gGainNow;
    if (start == 1.0f && target == 1.0f) return;
    const float step = frames ? (target - start) / (float)frames : 0.0f;
    for (UInt32 i = 0; i < frames; i++) {
        const float g = start + step * (float)(i + 1);
        for (int c = 0; c < UR_OUT_CHANNELS; c++) buf[i * UR_OUT_CHANNELS + c] *= g;
    }
    gGainNow = target;
}

static OSStatus UR_DoIOOperation(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID,
                                 AudioObjectID inStreamObjectID, UInt32 inClientID, UInt32 inOperationID,
                                 UInt32 inIOBufferFrameSize, const AudioServerPlugInIOCycleInfo *inIOCycleInfo,
                                 void *ioMainBuffer, void *ioSecondaryBuffer) {
    (void)inClientID; (void)ioSecondaryBuffer;
    if (inDriver != gDriverRef || inDeviceObjectID != kObjectID_Device) return kAudioHardwareBadObjectError;
    if (!ioMainBuffer || !inIOCycleInfo) return 0;
    if (inOperationID == kAudioServerPlugInIOOperationReadInput && inStreamObjectID == kObjectID_Stream_Input) {
        ur_engine_prof_add(UR_PROF_HAL_CYCLE, 1, 0);
        ur_engine_read_input((int64_t)llround(inIOCycleInfo->mInputTime.mSampleTime), inIOBufferFrameSize, ioMainBuffer);
    } else if (inOperationID == kAudioServerPlugInIOOperationWriteMix && inStreamObjectID == kObjectID_Stream_Output) {
        applyGain(ioMainBuffer, inIOBufferFrameSize);
        ur_engine_write_output((int64_t)llround(inIOCycleInfo->mOutputTime.mSampleTime), inIOBufferFrameSize, ioMainBuffer);
    }
    return 0;
}

static OSStatus UR_EndIOOperation(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID,
                                  UInt32 inClientID, UInt32 inOperationID, UInt32 inIOBufferFrameSize,
                                  const AudioServerPlugInIOCycleInfo *inIOCycleInfo) {
    (void)inClientID; (void)inIOBufferFrameSize; (void)inIOCycleInfo;
    if (inDriver != gDriverRef || inDeviceObjectID != kObjectID_Device) return kAudioHardwareBadObjectError;
    return 0;
}
