// ur_engine: USB audio engine for the Steinberg UR44 in Yamaha mode (0499:150e, device class 255),
// 44.1, 48, 88.2, 96, 176.4 and 192 kHz. Port of qc_engine (QuadCaptureMac, MIT).
//
// The engine streams capture and playback through IOUSBHost and exchanges audio with its client
// through two ring buffers indexed by device sample time. Sample time 0 is the first capture
// frame after the stream is stable. The engine publishes zero timestamps (sample time, host time)
// from the capture packets, so the client clock follows the device clock.
//
// Thread model:
//   ur_engine_configure / ur_engine_start / ur_engine_stop: a normal thread (they block).
//   ur_engine_read_input / ur_engine_write_output / ur_engine_zero_timestamp: real-time safe
//   (no locks, no allocation).
#pragma once
#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define UR_SAMPLE_RATE 44100.0  // the default sample rate
#define UR_IN_CHANNELS 6        // capture: INPUT 1-6
#define UR_OUT_CHANNELS 6       // playback: MIX 1 L/R, MIX 2 L/R, FX BUS L/R (into REV-X)
#define UR_ZERO_PERIOD 16384    // frames between zero timestamps
#define UR_RING_FRAMES 65536    // ring buffer size in frames (power of 2)
#define UR_MAX_REQUESTS 32
#define UR_MAX_MICROFRAMES 512

// Lag histogram buckets (ms): <0.5, <1, <2, <4, <8, <16, <32, >=32.
#define UR_LAG_BUCKETS 8

// Profiling sections (counts and time since start), exported in the statistics.
enum {
    UR_PROF_ZERO_CALL, UR_PROF_READ_CALL, UR_PROF_WRITE_CALL, UR_PROF_SCAN_IO, UR_PROF_SCAN_BACKUP,
    UR_PROF_SCAN_NONE, UR_PROF_SCAN_LOCKED, UR_PROF_SCAN_MICROS_IO, UR_PROF_SCAN_MICROS_BACKUP,
    UR_PROF_BACKUP_WAKE, UR_PROF_IN_DONE, UR_PROF_OUT_DONE, UR_PROF_ENQ_IN, UR_PROF_ENQ_OUT,
    UR_PROF_REUSE_RETRY, UR_PROF_HAL_CYCLE, UR_PROF_READ_READY,
    UR_PROF_REF_RETRY, UR_PROF_ZERO_RETRY, UR_PROF_N
};
const char *ur_engine_prof_name(int section);
void ur_engine_prof_add(int section, uint64_t count, uint64_t ticks);

typedef struct {
    uint64_t inputFrames;        // capture frames written to the ring
    uint64_t inputMissing;       // frames that a client read before they were in the ring
    uint64_t outputLate;         // frames that a client wrote after the USB side read them
    uint64_t emptyPackets;       // empty capture packets after the stream became stable
    uint64_t inErrors, outErrors;
    uint64_t resyncs;            // requests moved to a later frame because they were too late
    uint64_t gapFrames;          // capture frames of silence put in for skipped microframes
    uint64_t zeroTimestamps;
    double measuredRate;         // device sample rate measured against the host clock
    double inLagMaxMs, outLagMaxMs;   // completion callback lag after the end of the request
    uint64_t inLagHist[UR_LAG_BUCKETS], outLagHist[UR_LAG_BUCKETS];
    uint64_t directFrames;       // output frames written directly into queued USB requests
    uint64_t ringFrames;         // output frames written to the ring (read when a request is queued)
    uint64_t races;              // direct writes into a request that changed during the write
    int callbackPriority, callbackPolicy;   // scheduling of the USB callback thread (sampled)
    int scannerPriority;                    // priority of the real-time capture scanner (97 = real time)
    uint64_t profCount[UR_PROF_N];        // profiling: calls per section (see ur_engine_prof_name)
    double profUs[UR_PROF_N];            // profiling: time spent per section, microseconds
} UREngineStats;

// Direct write (default on): ur_engine_write_output writes into the queued USB requests until
// guardMicroframes before the transfer. Off: playback data comes from the ring when a request is queued.
// Direct read (default on): client calls and a backup thread scan queued capture data
// after each microframe and publish the clock. Off: the USB callbacks do it.
// With both on, the latency does not depend on the queue depth or on the time of the callbacks.
// Call before ur_engine_start.
#define UR_DIRECT_WRITE 1
#define UR_DIRECT_READ 2
void ur_engine_set_direct(int mask);

// Queue size: requests in flight for each direction, and microframes (125 us) per request
// (a multiple of 8). The margins (frames) cover the callback lag. Call before ur_engine_start.
void ur_engine_configure(int requests, int microframes, uint32_t outputMarginFrames, uint32_t inputMarginFrames);

// A client must write the output this number of frames before the current sample time, and can
// read the input this number of frames after it. These values follow the configuration.
// Timing of direct write and direct read (latency knobs): direct writes stop `guardMicroframes`
// (125 us) before the transfer; the scanner wakes every `scanPeriodUs` and reads a microframe
// `scanDelayMicroframes` after its end. Defaults 1, 0, 125 (tuned on the UR44 2026-11-07; QuadCaptureMac used 8, 2, 1000). Call before ur_engine_start.
void ur_engine_set_timing(uint32_t guardMicroframes, uint32_t scanDelayMicroframes, uint32_t scanPeriodUs);
void ur_engine_get_timing(uint32_t *guardMicroframes, uint32_t *scanDelayMicroframes, uint32_t *scanPeriodUs);

void ur_engine_get_config(int *requests, int *microframes, uint32_t *outputMarginFrames, uint32_t *inputMarginFrames,
                          int *direct);

// Sample rate: 44100, 48000, 88200, 96000, 176400 or 192000 Hz. ur_engine_set_rate returns false for
// other values or while the engine runs. ur_engine_start sets the device clock to this rate (USB Audio 1.0
// SET_CUR sampling frequency on both endpoints, after the alternate setting is selected).
#define UR_NUM_RATES 6
extern const uint32_t ur_engine_rates[UR_NUM_RATES];
bool ur_engine_rate_supported(uint32_t rate);
bool ur_engine_set_rate(uint32_t rate);
uint32_t ur_engine_get_rate(void);

// The margins of ur_engine_configure are frames at 44.1 kHz. At other rates the engine scales them.
uint32_t ur_engine_output_lead_frames(void);
uint32_t ur_engine_input_delay_frames(void);

// Device presence. The callback runs on a private queue when the QUAD-CAPTURE connects (true) or
// disconnects (false), and once at registration with the current state.
typedef void (*URPresenceCallback)(bool present);
void ur_engine_watch_device(URPresenceCallback callback);

// 0 on success, or a negative error code. The text of the error goes to the log.
// If the device disconnects while the engine runs, the engine stops its USB requests;
// ur_engine_zero_timestamp then returns false, and the client must call ur_engine_stop.
int ur_engine_start(void);
void ur_engine_stop(void);
bool ur_engine_is_running(void);
bool ur_engine_device_present(void);

// The most recent zero timestamp. Returns false if the engine does not run.
bool ur_engine_zero_timestamp(double *sampleTime, uint64_t *hostTime, uint64_t *seed);

// Interleaved float samples (UR_IN_CHANNELS / UR_OUT_CHANNELS per frame).
void ur_engine_read_input(int64_t sampleTime, uint32_t frames, float *dst);
void ur_engine_write_output(int64_t sampleTime, uint32_t frames, const float *src);

void ur_engine_get_stats(UREngineStats *stats);

#ifdef __cplusplus
}
#endif
