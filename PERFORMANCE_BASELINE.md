# LVOS 0.1.7 Phase 5 performance baseline

This baseline was measured from the optimized Windows x86_64 release binaries on Windows 11
10.0.26200, with 20 logical processors, 31.8 GiB of RAM, and `SLINT_SCALE_FACTOR=1.25`.
Run `powershell -NoProfile -File scripts/measure-phase5-performance.ps1` to reproduce it. The
script keeps raw timing and process samples under `target/phase5-performance/`.

| Measurement | LVOS 0.1.7 result | Initial target | Result |
| --- | ---: | ---: | --- |
| Agent-only Private Bytes maximum | 2.68 MiB | 20 MiB | Met |
| Cold Agent request to rendered Popup p95, 10 process starts | 293.37 ms | 250 ms | Optimization target missed |
| Warm Agent request to rendered Popup p95, 499 samples | 16.93 ms | 50 ms | Met |
| Popup idle-exit request after a 1 s timeout | 1,132.43 ms | 0-2,000 ms | Met |
| 500-cycle UI Private Bytes peak | 195.45 MiB | Baseline only | Recorded |

The Agent-only interval includes the optimized Agent binary, Tokio runtime, authenticated IPC
listener, and no GUI child. The 500-cycle run uses one warm GUI process, renders every lookup,
opens and closes Main UI five times, and switches from light animation to dark reduced-motion
halfway through. The external sampler observed no GUI process during the Agent-only interval and
confirmed that the GUI process exited within the configured timeout plus five seconds.

For memory stability, the steady sample's early and late Private Bytes medians were 157.89 MiB and
188.28 MiB. The fitted trend had R-squared 0.465 after renderer warm-up and did not meet the
diagnostic's definition of sustained linear growth (more than 8 MiB with R-squared at least 0.8).
The GUI process then exited and released all of its memory. These figures establish the 0.1.7
engineering baseline; only the warm latency and lifecycle invariants are release gates. The
250 ms cold-start figure remains an optimization target because this reference run measured
293.37 ms to an actual rendered frame.
