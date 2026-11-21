// ur_engine_test: use ur_engine the same way as the Core Audio plug-in, without Core Audio.
// A loop that runs every 2 ms follows the device clock from the zero timestamps, writes the
// output ur_engine_output_lead_frames() early, and reads the input ur_engine_input_delay_frames() late.
//
// Usage: ur_engine_test [-s seconds] [-T] [-o output_dir] [-n requests] [-t microframes]
//                       [-m output_margin_frames] [-i input_margin_frames] [-r rate]
//   -r sets the sample rate: 44100 (default), 48000, 96000 or 192000 Hz.
//   -b sets the client cycle in frames; -N skips recording and its allocation/file cost.
//   -p sets the backup scan period in microseconds (125..10000).
//   -a sets the output end target ahead of now (default 512); at -b 32, -a 64
//      leaves about one buffer before the next block begins, as HAL normally does.
//   Streaming CPU/cycles are process work between start and stop, excluding WAV writing.
//   -T plays the test tone: 440 Hz left (0.3-2.3 s), then 660 Hz right (2.7-4.7 s), -30 dBFS.
#import <Foundation/Foundation.h>
#include <mach/mach.h>
#include <mach/mach_time.h>
#include <mach/thread_policy.h>
#include <math.h>
#include <getopt.h>
#include <libproc.h>
#include <unistd.h>
#include <sys/resource.h>
#include "engine/ur_engine.h"

static void put16(FILE *f, uint16_t v) { fwrite(&v, 2, 1, f); }
static void put32(FILE *f, uint32_t v) { fwrite(&v, 4, 1, f); }

static void writeWav24(const char *path, const float *s, size_t frames, int ch, uint32_t rate) {
    FILE *f = fopen(path, "wb");
    if (!f) return;
    uint32_t bytes = (uint32_t)(frames * ch * 3);
    fwrite("RIFF", 1, 4, f); put32(f, 36 + bytes); fwrite("WAVEfmt ", 1, 8, f);
    put32(f, 16); put16(f, 1); put16(f, ch); put32(f, rate); put32(f, rate * ch * 3); put16(f, ch * 3); put16(f, 24);
    fwrite("data", 1, 4, f); put32(f, bytes);
    for (size_t i = 0; i < frames * ch; i++) {
        float v = s[i] > 1.0f ? 1.0f : (s[i] < -1.0f ? -1.0f : s[i]);
        int32_t x = (int32_t)lrintf(v * 8388607.0f);
        uint8_t b[3] = {(uint8_t)x, (uint8_t)(x >> 8), (uint8_t)(x >> 16)};
        fwrite(b, 1, 3, f);
    }
    fclose(f);
}

static double dbfs(double v) { return v > 0 ? 20.0 * log10(v) : -INFINITY; }

int main(int argc, char **argv) {
    @autoreleasepool {
        double seconds = 5.0;
        bool tone = false, record = true;
        uint32_t block = 0, scanPeriod = 0, lookahead = 512;
        NSString *outDir = [NSFileManager.defaultManager.currentDirectoryPath stringByAppendingPathComponent:@"recordings"];
        int ch, nReq = 0, tpr = 0;
        int outMargin = -1, inMargin = -1;
        while ((ch = getopt(argc, argv, "s:To:n:t:m:i:d:r:b:p:a:N")) != -1) {
            if (ch == 'a') {
                int n = atoi(optarg);
                if (n < 0 || n > 4096) { fprintf(stderr, "lookahead must be 0..4096 frames\n"); return 2; }
                lookahead = (uint32_t)n; continue;
            }
            if (ch == 'p') {
                int n = atoi(optarg);
                if (n < 125 || n > 10000) { fprintf(stderr, "scan period must be 125..10000 us\n"); return 2; }
                scanPeriod = (uint32_t)n; continue;
            }
            if (ch == 'N') { record = false; continue; }
            if (ch == 'b') {
                int n = atoi(optarg);
                if (n < 1 || n > 1024) { fprintf(stderr, "block must be 1..1024 frames\n"); return 2; }
                block = (uint32_t)n; continue;
            }
            if (ch == 'd') { ur_engine_set_direct(atoi(optarg)); continue; }
            if (ch == 'r') {
                if (!ur_engine_set_rate((uint32_t)atoi(optarg))) { fprintf(stderr, "unsupported rate\n"); return 2; }
                continue;
            }
            if (ch == 's') seconds = atof(optarg);
            else if (ch == 'T') tone = true;
            else if (ch == 'o') outDir = @(optarg);
            else if (ch == 'n') nReq = atoi(optarg);
            else if (ch == 't') tpr = atoi(optarg);
            else if (ch == 'm') outMargin = atoi(optarg);
            else if (ch == 'i') inMargin = atoi(optarg);
            else { fprintf(stderr, "usage: ur_engine_test [-s seconds] [-T] [-o dir] [-n requests] [-t microframes] [-m out_margin] [-i in_margin] [-r rate] [-b client_frames] [-p scan_period_us] [-a output_lookahead_frames] [-N (no recording)]\n"); return 2; }
        }
        if (nReq || tpr || outMargin >= 0 || inMargin >= 0)
            ur_engine_configure(nReq ? nReq : 8, tpr ? tpr : 128, outMargin >= 0 ? outMargin : 44, inMargin >= 0 ? inMargin : 44);
        if (scanPeriod) {
            uint32_t guard, scanDelay, oldPeriod;
            ur_engine_get_timing(&guard, &scanDelay, &oldPeriod);
            ur_engine_set_timing(guard, scanDelay, scanPeriod);
        }
        const double rate = ur_engine_get_rate();
        const int64_t lead = ur_engine_output_lead_frames(), delay = ur_engine_input_delay_frames();
        printf("%.0f Hz. Output lead %lld frames (%.1f ms), input delay %lld frames (%.1f ms)\n", rate, lead,
               lead * 1000.0 / rate, delay, delay * 1000.0 / rate);
        uint32_t actualGuard, actualDelay, actualPeriod;
        ur_engine_get_timing(&actualGuard, &actualDelay, &actualPeriod);
        printf("Client block %u; output lookahead %u frames; backup scan period %u us\n", block, lookahead, actualPeriod);
        if (tone && seconds < 5.0) seconds = 5.0;

        mach_timebase_info_data_t tb;
        mach_timebase_info(&tb);
        const double ticksPerFrame = 1e9 / rate * tb.denom / tb.numer;

        uint64_t t0 = mach_absolute_time();
        // The loop below runs in real time, as the Core Audio IO thread does (2 ms period).
        {
            thread_time_constraint_policy_data_t pol;
            const double ticksPerUs = 1000.0 * tb.denom / tb.numer;
            pol.period = (uint32_t)(block ? block * ticksPerFrame : 2000 * ticksPerUs);
            pol.computation = (uint32_t)(300 * ticksPerUs);
            pol.constraint = (uint32_t)(1000 * ticksPerUs);
            pol.preemptible = 1;
            if (thread_policy_set(mach_thread_self(), THREAD_TIME_CONSTRAINT_POLICY, (thread_policy_t)&pol,
                                  THREAD_TIME_CONSTRAINT_POLICY_COUNT) != KERN_SUCCESS)
                fprintf(stderr, "real-time policy refused\n");
        }
        int rc = ur_engine_start();
        double startMs = (double)(mach_absolute_time() - t0) * tb.numer / tb.denom / 1e6;
        if (rc) { fprintf(stderr, "ur_engine_start failed: %d (see: log show --last 1m --predicate 'subsystem == \"local.ur44.driver\"')\n", rc); return 1; }
        printf("Engine started in %.0f ms\n", startMs);

        size_t capMax = record ? (size_t)((seconds + 1) * rate) : 1024;
        float *cap = calloc(capMax * UR_IN_CHANNELS, sizeof(float));
        size_t capFrames = 0;
        float buf[1024 * UR_OUT_CHANNELS];
        int64_t outPos = INT64_MIN, inPos = INT64_MIN, outStart = 0;
        double lastZeroSample = -1;
        int zeroBackwards = 0;
        uint64_t loops = 0;
        // Input lag: frames between the current sample time and the end of the data in the ring.
        double lagMax = 0, lagSum = 0;
        int lagHist[8] = {0};   // < 512, < 1024, < 1536, < 2048, < 3072, < 4096, < 8192, more
        uint64_t lastMissing = 0;
        int missEvents = 0;
        double missAt[16];

        if (block) {
            // Like Core Audio's IO thread: real-time, one period per client block.
            double tpf = ticksPerFrame;
            thread_time_constraint_policy_data_t pol = {
                .period = (uint32_t)(block * tpf), .computation = (uint32_t)(block * tpf / 4),
                .constraint = (uint32_t)(block * tpf / 2), .preemptible = 1};
            thread_policy_set(mach_thread_self(), THREAD_TIME_CONSTRAINT_POLICY, (thread_policy_t)&pol,
                              THREAD_TIME_CONSTRAINT_POLICY_COUNT);
        }
        struct rusage_info_v4 perf0 = {0}, perf1 = {0};
        bool havePerf = proc_pid_rusage(getpid(), RUSAGE_INFO_V4, (rusage_info_t *)&perf0) == 0;
        struct rusage cpu0, cpu1;
        getrusage(RUSAGE_SELF, &cpu0);
        uint64_t streamStart = mach_absolute_time(), wake = streamStart;
        uint64_t tEnd = streamStart + (uint64_t)(seconds * 1e9 * tb.denom / tb.numer);
        while (mach_absolute_time() < tEnd) {
            if (block) {
                wake += (uint64_t)(block * ticksPerFrame);
                uint64_t now = mach_absolute_time();
                if (wake < now) wake = now;
                mach_wait_until(wake);
            } else usleep(2000);
            loops++;
            double zs; uint64_t zh, seed;
            if (!ur_engine_zero_timestamp(&zs, &zh, &seed)) { fprintf(stderr, "engine stopped\n"); break; }
            if (zs < lastZeroSample) zeroBackwards++;
            lastZeroSample = zs;
            uint64_t now = mach_absolute_time();
            double cur = zs + ((double)now - (double)zh) / ticksPerFrame;
            UREngineStats ls;
            ur_engine_get_stats(&ls);
            double lag = cur - (double)ls.inputFrames;
            if (lag > lagMax) lagMax = lag;
            lagSum += lag;
            static const double edges[7] = {512, 1024, 1536, 2048, 3072, 4096, 8192};
            int b = 0;
            while (b < 7 && lag >= edges[b]) b++;
            lagHist[b]++;
            if (ls.inputMissing != lastMissing) {
                if (missEvents < 16) missAt[missEvents] = cur / rate;
                missEvents++;
                lastMissing = ls.inputMissing;
            }

            // Output: configurable lookahead, with frame-sized client calls when -b is set.
            int64_t want = (int64_t)cur + lead + lookahead;
            if (outPos == INT64_MIN) outPos = outStart = (int64_t)cur + lead + (block ? lookahead : 0);
            while (outPos < want) {
                uint32_t n = block ? block : (uint32_t)MIN(1024, want - outPos);
                for (uint32_t i = 0; i < n; i++) {
                    double t = (double)(outPos + i - outStart) / rate;
                    float l = 0, r = 0;
                    if (tone) {
                        const double amp = pow(10.0, -30.0 / 20.0), fade = 0.03;
                        if (t >= 0.3 && t < 2.3) {
                            double a = t - 0.3, b = 2.3 - t, env = 1;
                            if (a < fade) env = 0.5 - 0.5 * cos(M_PI * a / fade);
                            if (b < fade) env = 0.5 - 0.5 * cos(M_PI * b / fade);
                            l = (float)(amp * env * sin(2 * M_PI * 440.0 * a));
                        } else if (t >= 2.7 && t < 4.7) {
                            double a = t - 2.7, b = 4.7 - t, env = 1;
                            if (a < fade) env = 0.5 - 0.5 * cos(M_PI * a / fade);
                            if (b < fade) env = 0.5 - 0.5 * cos(M_PI * b / fade);
                            r = (float)(amp * env * sin(2 * M_PI * 660.0 * a));
                        }
                    }
                    for (int c = 0; c < UR_OUT_CHANNELS; c++) buf[i * UR_OUT_CHANNELS + c] = 0.0f;
                    buf[i * UR_OUT_CHANNELS] = l;
                    buf[i * UR_OUT_CHANNELS + 1] = r;
                }
                ur_engine_write_output(outPos, n, buf);
                outPos += n;
            }

            // Input: read delay late.
            int64_t avail = (int64_t)cur - delay;
            if (inPos == INT64_MIN) inPos = avail;
            while (inPos + (block ? block : 1) <= avail && (!record || capFrames < capMax)) {
                uint32_t n = block ? block : (uint32_t)MIN(1024, avail - inPos);
                if (record) n = (uint32_t)MIN(n, capMax - capFrames);
                ur_engine_read_input(inPos, n, cap + (record ? capFrames : 0) * UR_IN_CHANNELS);
                capFrames += n;
                inPos += n;
            }
        }

        getrusage(RUSAGE_SELF, &cpu1);
        double elapsed = (mach_absolute_time() - streamStart) * (double)tb.numer / tb.denom / 1e9;
        double user = cpu1.ru_utime.tv_sec - cpu0.ru_utime.tv_sec + (cpu1.ru_utime.tv_usec - cpu0.ru_utime.tv_usec) / 1e6;
        double system = cpu1.ru_stime.tv_sec - cpu0.ru_stime.tv_sec + (cpu1.ru_stime.tv_usec - cpu0.ru_stime.tv_usec) / 1e6;
        printf("Streaming CPU: %.6f user + %.6f system = %.6f s over %.3f s (%.6f s/min); client block %u\n",
               user, system, user + system, elapsed, (user + system) * 60 / elapsed, block);
        if (havePerf && proc_pid_rusage(getpid(), RUSAGE_INFO_V4, (rusage_info_t *)&perf1) == 0)
            printf("Streaming work: %.0f cycles/s; %.0f instructions/s (process, excludes startup/drain)\n",
                   (perf1.ri_cycles - perf0.ri_cycles) / elapsed,
                   (perf1.ri_instructions - perf0.ri_instructions) / elapsed);
        UREngineStats st;
        ur_engine_get_stats(&st);
        for (int i = 0; i < UR_PROF_N; i++)
            if (st.profCount[i])
                printf("prof %-16s %10llu calls/s %9.1f us/s\n", ur_engine_prof_name(i),
                       (unsigned long long)(st.profCount[i] / elapsed), st.profUs[i] / elapsed);
        ur_engine_stop();

        printf("Loops: %llu; zero timestamps: %llu (backwards: %d); measured device rate: %.2f Hz\n",
               loops, st.zeroTimestamps, zeroBackwards, st.measuredRate);
        printf("Input lag (frames behind real time): average %.0f, maximum %.0f; histogram <512:%d <1024:%d <1536:%d <2048:%d <3072:%d <4096:%d <8192:%d more:%d\n",
               lagSum / (loops ? loops : 1), lagMax, lagHist[0], lagHist[1], lagHist[2], lagHist[3], lagHist[4],
               lagHist[5], lagHist[6], lagHist[7]);
        if (missEvents) {
            printf("Missing input seen at (s):");
            for (int i = 0; i < missEvents && i < 16; i++) printf(" %.3f", missAt[i]);
            printf("\n");
        }
        static const char *bk[UR_LAG_BUCKETS] = {"<0.5", "<1", "<2", "<4", "<8", "<16", "<32", ">=32"};
        printf("Callback lag after the end of a request (ms): capture max %.2f, playback max %.2f\n  capture: ",
               st.inLagMaxMs, st.outLagMaxMs);
        for (int b = 0; b < UR_LAG_BUCKETS; b++) printf(" %s:%llu", bk[b], st.inLagHist[b]);
        printf("\n  playback:");
        for (int b = 0; b < UR_LAG_BUCKETS; b++) printf(" %s:%llu", bk[b], st.outLagHist[b]);
        printf("\nResyncs: %llu, gap frames: %llu; direct %llu, ring %llu, races %llu; callback priority %d, scanner priority %d\n",
               st.resyncs, st.gapFrames, st.directFrames, st.ringFrames, st.races, st.callbackPriority, st.scannerPriority);
        printf("Input frames: %llu, missing: %llu, empty packets: %llu; output late: %llu; USB errors in/out: %llu/%llu\n",
               st.inputFrames, st.inputMissing, st.emptyPackets, st.outputLate, st.inErrors, st.outErrors);
        if (!record) { free(cap); return 0; }
        for (int c = 0; c < UR_IN_CHANNELS; c++) {
            double peak = 0, sum = 0;
            for (size_t i = 0; i < capFrames; i++) {
                double v = cap[i * UR_IN_CHANNELS + c];
                if (fabs(v) > peak) peak = fabs(v);
                sum += v * v;
            }
            printf("  input %d: peak %6.1f dBFS, rms %6.1f dBFS\n", c + 1, dbfs(peak), dbfs(sqrt(sum / (capFrames ? capFrames : 1))));
        }
        [NSFileManager.defaultManager createDirectoryAtPath:outDir withIntermediateDirectories:YES attributes:nil error:nil];
        NSString *path = [outDir stringByAppendingPathComponent:@"engine-test-in1-2.wav"];
        writeWav24(path.UTF8String, cap, capFrames, UR_IN_CHANNELS, (uint32_t)rate);
        printf("Wrote %s (%.2f s)\n", path.UTF8String, capFrames / rate);
    }
    return 0;
}
