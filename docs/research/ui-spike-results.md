# UI framework resident-memory spike (2026-11-15)

Each spike: tray icon + on-demand 720x480 window with a 5-bar usage chart.
Measured by `measure.ps1` (30 s settle + 30 s idle). "closed" = after 10 open/close
cycles. Private WS = Task Manager "Memory" column; Private Bytes = committed memory.

| Build | State | Private WS | Private Bytes | Threads | Idle CPU / 30 s |
|---|---|---|---|---|---|
| clash-nyanpasu (baseline, window closed, re-measured) | closed | 14.8 MB | 16.8 MB | 24 | - |
| Avalonia 12.1.6 AOT, GPU (ANGLE/D3D) | open | 35.0 MB | 97.7 MB | 99 | 109 ms |
| | closed | 31.4 MB | 104.9 MB | 86 | 16 ms |
| Avalonia 12.1.6 AOT, `--software` | open | 20.5 MB | 24.1 MB | 21 | 47 ms |
| | **closed** | **12.0 MB** | **14.2 MB** | **8** | 16 ms |
| GPUI (gpui-kit 0.9.1), GPU (D3D11) | open | 17.4 MB | 85.8 MB | 59 | 359 ms |
| | closed | 23.8 MB | 94.4 MB | 68 | 94 ms |
| GPUI, `GPUI_SPIKE_WARP=1` (patched) | open | 32.7 MB | 46.4 MB | 48 | 578 ms |
| | closed | 34.7 MB | 44.1 MB | 44 | 47 ms |

`--trim` (EmptyWorkingSet) drops Private WS to ~2 MB in every build but leaves
Private Bytes unchanged; it only moves pages to the standby list.

Findings:
- In-process GPU rendering loads the GPU driver, adding ~60-90 MB of commit that
  never unloads. clash-nyanpasu avoids this because WebView2 renders in child
  processes that exit when the window is destroyed.
- Avalonia with software rendering beats the clash-nyanpasu baseline when closed.
- GPUI does not release resources after its last window closes, and keeps 44+ threads.

clash-nyanpasu correction: the first baseline (12.1 / 27.0 MB) was taken without
checking the window state. With the window closed (default `WindowCloseBehavior::Destroy`),
the main process is the only process left; all WebView2 children exit. While the
window is open, its six WebView2 processes add ~145 MB Private WS / ~586 MB Private Bytes.

GPU driver breakdown (Avalonia spike, RTX 4060 Laptop, driver 621.14, VirtualQueryEx):

| Mode | Private committed | Image mapped | Private Bytes | Threads |
|---|---|---|---|---|
| GPU (ANGLE/D3D11) | 59.7 MB | 331.0 MB | 102.0 MB | 89 |
| Software | 6.6 MB | 92.5 MB | 13.6 MB | 11 |

GPU mode loads nvgpucomp64.dll (110 MB, shader compiler), nvwgf2umx.dll (89 MB,
D3D user-mode driver), nvppex.dll, nvspcap64.dll (NVIDIA App capture/overlay hook),
ANGLE and d3dcompiler_47. Code pages are shared between processes; the per-process
cost is the driver heaps, writable DLL data and worker threads.

Artifacts:
- `avalonia/bin/Release/net10.0/win-x64/publish/SpikeAvalonia.exe` (+3 native DLLs, ~33 MB total)
- `gpui/target/release/spike-gpui.exe` (single 11.3 MB exe)

## Rendering throughput: software vs GPU (Avalonia, `--bench --chart X`)

Machine: i7-14700HX, RTX 4060 Laptop, 2560x1600 @150%, 165 Hz. Each chart redraws
every frame for 10 s after a 3 s warmup. "small" = 1280x800 logical (1920x1200 px),
"large" = 1660x1000 logical (2490x1500 px). CPU is % of one core.
`ui_draw` = UI-thread time recording draw commands per frame.

| Chart | Mode | small fps / CPU | large fps / CPU | ui_draw | Private Bytes while animating |
|---|---|---|---|---|---|
| Timeline (3000 segments, pan + zoom) | GPU | 98.9 / 47% | 91.5 / 46% | 0.5 ms | 146-159 MB |
| | Software | 56.5 / 98% | **26.2** / 95% | 0.4 ms | 61-65 MB |
| Area (1440 points, scrolling) | GPU | 85.7 / 48% | 82.5 / 41% | 0.1 ms | 234-239 MB |
| | Software | 119.7 / 98% | 75.8 / 100% | 0.1 ms | 106-129 MB |
| Heatmap (8760 cells, all changing) | GPU | 31.2 / 80% | 33.8 / 69% | 2.0 ms | 149-168 MB |
| | Software | 30.7 / 103% | 23.0 / 100% | 1.8 ms | 64-89 MB |

Findings:
- Software rendering rasterizes on one render thread; cost scales with pixel count.
  Full-window pan/zoom is ~57 fps at 1920x1200 but drops to ~26 fps near fullscreen.
- The heatmap is ~30 fps in both modes: per-draw-op overhead in Avalonia's compositor,
  not rasterization. Thousands of primitives must be cached (bitmap/layer), not redrawn.
- GPU mode is not capped at 60 but stays under the 165 Hz refresh in these scenes.
- Numbers are for continuous animation; idle windows cost ~0 CPU in both modes.

## Cost of opening the UI once (`startup.ps1`, `gaptest.ps1`)

GPUI: a new `spike-gpui --ui-once` process per open; latency from process creation
to the first frame. Tauri: a resident `spike-tauri` process builds a webview window;
latency from `WebviewWindowBuilder` to the page's first paint (double rAF, then
`invoke('ready')`). Resources sampled 1.5 s after first paint with Get-Process
(WMI perf queries took ~1 s each and disturbed launches, so Private WS is omitted).

| | Processes while open | Private Bytes while open | CPU per open | First paint, warm | First paint, cold |
|---|---|---|---|---|---|
| GPUI UI process | 1 | 84.1-84.9 MB | 0.42-0.89 s | 0.50-0.92 s | 3.6-4.6 s |
| Tauri webview window | 7 (main + 6 WebView2) | 160-170 MB | 1.06-1.59 s | 0.56-0.61 s | 1.8-2.1 s |

Tauri's WebView2 tree while open: browser 40 MB, gpu-process 73 MB, renderer 24 MB,
two utility 7-11 MB, crashpad 3 MB (Private Bytes).

GPUI phases (ms since process creation):

| Run set | platform created | `run` callback entered | window open | first frame |
|---|---|---|---|---|
| 0.5 s gap, fast runs | ~180 | ~450-490 | ~490-580 | ~500-580 |
| 0.5 s gap, slow runs | ~180 | ~3930-4420 | ~4080-4500 | ~4080-4500 |
| 15 s gap, all runs | ~140-200 | ~3630-4390 | ~3720-4520 | ~3720-4520 |
| 15 s gap, `GPUI_SPIKE_WARP=1` | ~135-160 | ~155-190 | ~260-310 | **~260-310** |

The cold delay is D3D11 device creation on the RTX 4060 (GPUI's Windows backend
creates it in `Application::run` before the launch callback: `connect_initially`
-> `DirectXDevices::new`). With the WARP adapter the same launch takes ~0.3 s even
after 15 s idle, so GPUI itself is fast; the ~3.5-4 s is the dGPU waking and its
driver initializing. Tauri shows the same fast/slow alternation, partly hidden
because Chromium starts its GPU process in parallel with other work.
