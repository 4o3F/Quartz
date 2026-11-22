import { parseArgs } from "@std/cli/parse-args";
import { dirname, join, resolve } from "@std/path";
import { run } from "./common.ts";
import {
  machineMetadata,
  type Process,
  type Snapshot,
  Windows,
} from "./windows.ts";

const sleep = (ms: number) => new Promise<void>((done) => setTimeout(done, ms));
const MiB = 1024 * 1024;
const messages = { close: 0x10, trayCallback: 0x8001, select: 0x400 };
export type Mode = "resident" | "on-demand" | "core-only";
export type Scenario =
  | "core-only"
  | "hidden-never-opened"
  | "hidden-after-close"
  | "visible-static";

/** The core baseline precedes all window opens; Hidden is sampled on both sides of the first open. */
export function scenarioSequence(mode: Mode): Scenario[] {
  switch (mode) {
    case "resident":
      return [
        "core-only",
        "hidden-never-opened",
        "hidden-after-close",
        "visible-static",
      ];
    case "on-demand":
      return ["core-only", "visible-static"];
    case "core-only":
      return ["core-only"];
  }
}

/** Nearest-rank percentiles, matching the query benchmark method. */
export function percentile(
  values: readonly number[],
  fraction: number,
): number {
  if (
    !values.length || !Number.isFinite(fraction) ||
    values.some((value) => !Number.isFinite(value)) ||
    fraction <= 0 || fraction > 1
  ) {
    throw new Error("Percentile requires finite samples and 0 < fraction <= 1");
  }
  return [...values].sort((a, b) =>
    a - b
  )[Math.ceil(values.length * fraction) - 1];
}

/** Parse only complete first-frame log records; tolerate tracing ANSI styling and quoted values. */
export function firstFrameSamples(log: string): number[] {
  // ANSI escape sequences are deliberately stripped from tracing console records.
  // deno-lint-ignore no-control-regex
  return log.replace(/\x1b\[[0-9;]*m/g, "").split("\n").slice(0, -1).flatMap(
    (line) => {
      if (!line.includes("UI first frame acknowledged")) return [];
      const match = /\bclick_to_first_frame_ms=(?:"(\d+)"|(\d+))(?=\s|$)/.exec(
        line,
      );
      return match ? [Number(match[1] ?? match[2])] : [];
    },
  );
}

interface Segment {
  scenario: Scenario;
  process: "core" | "ui";
  pid: number;
  seconds: number;
  cpuMs: number;
  samples: Snapshot[];
}
interface Cycle {
  cycle: number;
  pid: number;
  uiPrivateBytes: number;
  core: Snapshot;
}
const metrics = ["PrivateBytes", "PrivateWS", "Threads", "Handles"] as const;
export function segmentSummary(samples: readonly Snapshot[]) {
  return Object.fromEntries(metrics.map((key) => [key, {
    median: percentile(samples.map((sample) => sample[key]), 0.5),
    max: Math.max(...samples.map((sample) => sample[key])),
  }]));
}

function parameters() {
  const aliases: Record<string, string> = {
    exe: "Exe",
    settleseconds: "SettleSeconds",
    idleseconds: "IdleSeconds",
    screenshotpath: "ScreenshotPath",
  };
  const args = Deno.args.map((arg) =>
    arg.replace(
      /^--?(Exe|SettleSeconds|IdleSeconds|ScreenshotPath)(?==|$)/i,
      (_, name: string) => `--${aliases[name.toLowerCase()]}`,
    )
  );
  const options = parseArgs(args, {
    string: [
      "Exe",
      "SettleSeconds",
      "IdleSeconds",
      "ScreenshotPath",
      "mode",
      "segments",
      "opens",
      "output",
    ],
    default: {
      SettleSeconds: "30",
      IdleSeconds: "600",
      mode: "resident",
      segments: "3",
      opens: "20",
    },
    unknown: (arg) => {
      throw new Error(`Unknown argument: ${arg}`);
    },
  });
  if (!options.Exe || options._.length) {
    throw new Error(
      "Use deno task measure --Exe <quartz.exe> [--mode resident|on-demand|core-only] [--segments 3] [--opens 20] [--output <directory>]",
    );
  }
  const integer = (value: string, max: number) => {
    if (!/^\d+$/.test(value) || Number(value) < 1 || Number(value) > max) {
      throw new Error(`Expected integer 1..${max}: ${value}`);
    }
    return Number(value);
  };
  if (!["resident", "on-demand", "core-only"].includes(options.mode)) {
    throw new Error("Invalid --mode");
  }
  return {
    exe: options.Exe,
    mode: options.mode as Mode,
    settle: integer(options.SettleSeconds, 3600),
    idle: integer(options.IdleSeconds, 3600),
    segments: integer(options.segments, 20),
    opens: integer(options.opens, 100),
    screenshot: options.ScreenshotPath,
    output: options.output,
  };
}

async function main(): Promise<void> {
  const options = parameters();
  if (Deno.build.os !== "windows" || Deno.build.arch !== "x86_64") {
    throw new Error("Measurement requires x86_64 Windows desktop");
  }
  const exe = await Deno.realPath(options.exe);
  const output = resolve(
    options.output ?? `target/windows/measure-${options.mode}-${Date.now()}`,
  );
  await Deno.mkdir(output, { recursive: true });
  const windows = new Windows();
  const previousLocalAppData = Deno.env.get("LOCALAPPDATA");
  let core: Process | undefined;
  let ui: Process | undefined;
  let maxCloseMs = 0;
  const segments: Segment[] = [];
  const cycles: Cycle[] = [];
  const latencies: number[] = [];
  const coreModules = new Set<string>();
  let uiModules: string[] = [];
  let cycleBaseline: Snapshot | undefined;
  let cycleAfter: Snapshot | undefined;
  const metadata = machineMetadata();
  const report = {
    mode: options.mode,
    date: new Date().toISOString(),
    metadata,
    segments,
    cycles,
    latencies,
  };
  const logPath = join(output, "data", "Quartz", "logs", "core.log");
  const waitFor = async (
    predicate: () => boolean,
    description: string,
    timeout = 30000,
  ) => {
    const deadline = performance.now() + timeout;
    while (!predicate()) {
      if (core && windows.wait(core, 0)) {
        throw new Error("Core exited unexpectedly");
      }
      if (performance.now() >= deadline) {
        throw new Error(`Timed out: ${description}`);
      }
      await sleep(50);
    }
  };
  const children = () => windows.matchingProcesses(exe, core!.id);
  const childCount = () => {
    const found = children();
    for (const child of found) windows.closeProcess(child);
    return found.length;
  };
  const settings = async (resident: boolean) => {
    const result = await new Deno.Command(exe, {
      args: [
        "settings",
        "set",
        "--resident-enabled",
        String(resident),
        "--prewarm-enabled",
        "false",
      ],
      stdout: "piped",
      stderr: "piped",
    }).output();
    if (!result.success) {
      throw new Error(
        `settings set failed: ${new TextDecoder().decode(result.stderr)}`,
      );
    }
  };
  const acquireUi = async () => {
    await waitFor(() => {
      const found = children();
      try {
        if (found.length > 1) throw new Error("Multiple UI children");
        if (found.length === 1) {
          ui = found[0];
          return true;
        }
        return false;
      } finally {
        for (const child of found) {
          if (child !== ui) windows.closeProcess(child);
        }
      }
    }, "one UI child");
  };
  const openUi = async () => {
    const window = windows.findCore();
    if (!window || windows.windowProcessId(window) !== core!.id) {
      throw new Error("Core window ownership mismatch");
    }
    windows.post(window, messages.trayCallback, messages.select);
    if (!ui) await acquireUi();
    await waitFor(() => {
      if (windows.wait(ui!, 0)) throw new Error("UI exited while opening");
      return !!windows.mainWindow(ui!.id);
    }, "visible UI");
  };
  const closeUi = async () => {
    const child = ui!;
    const window = windows.mainWindow(child.id);
    if (!window) throw new Error("UI has no visible window");
    const start = performance.now();
    windows.post(window, messages.close);
    if (options.mode === "resident") {
      await waitFor(
        () => {
          if (windows.wait(child, 0)) {
            throw new Error("Resident UI exited on close");
          }
          return !windows.mainWindow(child.id);
        },
        "Hidden",
        1000,
      );
      if (childCount() !== 1) throw new Error("Resident child count changed");
    } else {
      await waitFor(() => windows.wait(child, 0), "on-demand UI exit", 1000);
      if (windows.exitCode(child) !== 0) throw new Error("UI exit failed");
      windows.closeProcess(child);
      ui = undefined;
      await waitFor(() => childCount() === 0, "no UI child", 1000);
    }
    maxCloseMs = Math.max(maxCloseMs, performance.now() - start);
    await sleep(200);
  };
  const sample = async (scenario: Scenario, seconds: number) => {
    const processes = [
      { process: "core" as const, handle: core! },
      ...(ui ? [{ process: "ui" as const, handle: ui }] : []),
    ];
    const rows = processes.map(({ process, handle }) => ({
      scenario,
      process,
      pid: handle.id,
      seconds,
      cpuMs: 0,
      samples: [windows.snapshot(handle, scenario)],
    }));
    const starts = rows.map((row) => row.samples[0].CpuMs);
    const start = performance.now();
    for (let second = 1; second <= seconds; second++) {
      await sleep(Math.max(0, start + second * 1000 - performance.now()));
      processes.forEach(({ handle }, index) => {
        if (windows.wait(handle, 0)) {
          throw new Error(`PID ${handle.id} exited during ${scenario}`);
        }
        rows[index].samples.push(windows.snapshot(handle, scenario));
      });
      if (ui && childCount() !== 1) {
        throw new Error("UI child count changed during sampling");
      }
    }
    rows.forEach((row, index) => {
      row.cpuMs = row.samples.at(-1)!.CpuMs - starts[index];
      segments.push(row);
      console.log(
        JSON.stringify({
          scenario,
          process: row.process,
          pid: row.pid,
          seconds,
          cpuMs: row.cpuMs,
          ...segmentSummary(row.samples),
        }),
      );
    });
  };
  const log = async () => await Deno.readTextFile(logPath);
  try {
    if (windows.findCore()) {
      throw new Error(
        "Exit installed Quartz before measurement; LOCALAPPDATA does not isolate the pipe",
      );
    }
    const existing = windows.matchingProcesses(exe);
    for (const process of existing) windows.closeProcess(process);
    if (existing.length) {
      throw new Error("Exit existing Quartz processes before measurement");
    }
    Deno.env.set("LOCALAPPDATA", join(output, "data"));
    core = windows.start(exe);
    await waitFor(() => !!windows.findCore(), "core startup");
    if (windows.windowProcessId(windows.findCore()!) !== core.id) {
      throw new Error("Core window belongs to another process");
    }
    await settings(false);
    await waitFor(() => childCount() === 0, "resident disabled");
    for (const scenario of scenarioSequence(options.mode)) {
      if (scenario === "hidden-never-opened") {
        await settings(true);
        await acquireUi();
        // Startup may be cooling down after the initial default-resident launch.
        await waitFor(() => !windows.mainWindow(ui!.id), "resident Hidden");
      } else if (scenario === "hidden-after-close") {
        const pid = ui!.id;
        await openUi();
        await sleep(500);
        await closeUi();
        if (ui!.id !== pid) throw new Error("Resident PID changed after close");
      } else if (scenario === "visible-static") {
        await openUi();
      }
      await sleep(options.settle * 1000);
      for (
        let segment = 0;
        segment < (scenario === "core-only" ? 1 : options.segments);
        segment++
      ) {
        await sample(scenario, scenario === "core-only" ? options.idle : 30);
      }
      for (const module of windows.modules(core)) coreModules.add(module);
      if (ui) uiModules = windows.modules(ui);
      if (scenario === "visible-static") {
        if (options.screenshot) {
          await windows.saveWindow(
            windows.mainWindow(ui!.id)!,
            options.screenshot,
          );
        }
        await closeUi();
      }
    }
    if (options.mode !== "core-only") {
      const residentPid = ui?.id;
      const baseline = windows.snapshot(core, "before cycles");
      cycleBaseline = baseline;
      for (let cycle = 1; cycle <= 20; cycle++) {
        await openUi();
        await sleep(500);
        const visible = windows.snapshot(ui!, "visible cycle");
        const pid = ui!.id;
        if (residentPid !== undefined && pid !== residentPid) {
          throw new Error("Resident PID changed across cycles");
        }
        await closeUi();
        cycles.push({
          cycle,
          pid,
          uiPrivateBytes: ui
            ? windows.snapshot(ui, "Hidden cycle").PrivateBytes
            : visible.PrivateBytes,
          core: windows.snapshot(core, `after cycle ${cycle}`),
        });
      }
      await sleep(options.settle * 1000);
      cycleAfter = windows.snapshot(core, "after 20 cycles settled");
      console.log(
        `Core 20-cycle Private Bytes delta MiB: ${
          (cycleAfter.PrivateBytes - baseline.PrivateBytes) / MiB
        }`,
      );
      for (let open = 0; open < options.opens; open++) {
        const before = await log();
        await openUi();
        let samples: number[] = [];
        const deadline = performance.now() + 30000;
        do {
          const current = await log();
          if (!current.startsWith(before)) {
            throw new Error("core.log rotated during latency sampling");
          }
          samples = firstFrameSamples(current.slice(before.length));
          if (samples.length) break;
          await sleep(50);
        } while (performance.now() < deadline);
        if (samples.length !== 1) {
          throw new Error(
            `Expected one first-frame record, got ${samples.length}`,
          );
        }
        latencies.push(samples[0]);
        await closeUi();
      }
    }
    for (const module of windows.modules(core)) coreModules.add(module);
    const grouped = new Map<string, Segment[]>();
    for (const segment of segments) {
      const key = `${segment.scenario}/${segment.process}`;
      grouped.set(key, [...(grouped.get(key) ?? []), segment]);
    }
    const aggregates = [...grouped].map(([scenario, rows]) => ({
      scenario,
      segmentCount: rows.length,
      cpuMedianMs: percentile(rows.map((row) => row.cpuMs), 0.5),
      cpuMaxMs: Math.max(...rows.map((row) => row.cpuMs)),
      ...segmentSummary(rows.flatMap((row) => row.samples)),
    }));
    const cycleDelta = cycleBaseline && cycleAfter
      ? Object.fromEntries(
        metrics.map((key) => [key, cycleAfter![key] - cycleBaseline![key]]),
      )
      : null;
    const latency = latencies.length
      ? {
        count: latencies.length,
        p50: percentile(latencies, 0.5),
        p95: percentile(latencies, 0.95),
        max: Math.max(...latencies),
      }
      : null;
    const nvModules = [...coreModules].filter((module) =>
      /^nv.*\.dll$/i.test(module)
    );
    const sizes: Record<string, number> = {
      "quartz.exe": (await Deno.stat(exe)).size,
    };
    try {
      sizes["quartz-host.exe"] =
        (await Deno.stat(join(dirname(exe), "quartz-host.exe"))).size;
    } catch (error) {
      if (!(error instanceof Deno.errors.NotFound)) throw error;
    }
    await Deno.writeTextFile(
      join(output, "report.json"),
      JSON.stringify(
        {
          ...report,
          latency,
          aggregates,
          cycleBaseline,
          cycleAfter,
          cycleDelta,
          maxCloseMs,
          coreModules: [...coreModules],
          uiModules,
          sizes,
          summaries: segments.map((segment) => ({
            scenario: segment.scenario,
            process: segment.process,
            ...segmentSummary(segment.samples),
          })),
        },
        null,
        2,
      ) + "\n",
    );
    const table = [
      "| Scenario | Process | PID | Seconds | CPU ms | Private MiB median / max | Private WS MiB median / max | Threads median / max | Handles median / max |",
      "|---|---|---:|---:|---:|---:|---:|---:|---:|",
      ...segments.map((segment) =>
        `| ${segment.scenario} | ${segment.process} | ${segment.pid} | ${segment.seconds} | ${
          segment.cpuMs.toFixed(3)
        } | ${
          metrics.map((key) => {
            const values = segment.samples.map((sample) =>
              sample[key] / (key.startsWith("Private") ? MiB : 1)
            );
            return `${percentile(values, 0.5).toFixed(3)} / ${
              Math.max(...values).toFixed(3)
            }`;
          }).join(" | ")
        } |`
      ),
    ];
    const cpuTable = [
      "| Scenario / process | Segments | CPU median ms | CPU max ms |",
      "|---|---:|---:|---:|",
      ...aggregates.map((row) =>
        `| ${row.scenario} | ${row.segmentCount} | ${
          row.cpuMedianMs.toFixed(3)
        } | ${row.cpuMaxMs.toFixed(3)} |`
      ),
    ];
    const cycleTable = [
      "| Cycle | UI PID | UI Private MiB | Core Private MiB |",
      "|---:|---:|---:|---:|",
      ...cycles.map((cycle) =>
        `| ${cycle.cycle} | ${cycle.pid} | ${
          (cycle.uiPrivateBytes / MiB).toFixed(3)
        } | ${(cycle.core.PrivateBytes / MiB).toFixed(3)} |`
      ),
    ];
    await Deno.writeTextFile(
      join(output, "report.md"),
      `# M3 ${options.mode} measurement\n\n${report.date}\n\n${
        JSON.stringify(metadata)
      }\n\n${table.join("\n")}\n\n${cpuTable.join("\n")}\n\n${
        cycleTable.join("\n")
      }\n\nCore settled cycle delta (bytes / counts): ${
        JSON.stringify(cycleDelta)
      }.\n\nFirst frame (ms): ${JSON.stringify(latency)}. Maximum close: ${
        maxCloseMs.toFixed(3)
      } ms.\n\nCore nv modules: ${JSON.stringify(nvModules)}. Sizes: ${
        JSON.stringify(sizes)
      }.\n`,
    );
    console.log(
      `First frame ms: ${JSON.stringify(latency)}; reports: ${output}`,
    );
    if (nvModules.length) throw new Error("Core loaded NVIDIA modules");
    if (
      cycleBaseline && cycleAfter && (
        cycleAfter.PrivateBytes - cycleBaseline.PrivateBytes > 2 * MiB ||
        cycleAfter.Threads > cycleBaseline.Threads ||
        cycleAfter.Handles > cycleBaseline.Handles
      )
    ) {
      throw new Error(
        "Core grew beyond the 20-cycle acceptance limit; inspect report",
      );
    }
  } finally {
    // Stop the core first so its resident restart policy cannot race cleanup.
    try {
      if (core) {
        const window = windows.findCore();
        if (window && windows.windowProcessId(window) === core.id) {
          windows.post(window, messages.close);
        }
        if (!windows.wait(core, 5000)) windows.terminate(core);
        for (const child of children()) {
          try {
            windows.terminate(child);
          } finally {
            windows.closeProcess(child);
          }
        }
      }
    } finally {
      if (ui) windows.closeProcess(ui);
      if (core) windows.closeProcess(core);
      if (previousLocalAppData === undefined) Deno.env.delete("LOCALAPPDATA");
      else Deno.env.set("LOCALAPPDATA", previousLocalAppData);
      windows.close();
      // Preserve partial samples when a lifecycle assertion fails.
      await Deno.writeTextFile(
        join(output, "samples.json"),
        JSON.stringify(report, null, 2) + "\n",
      );
    }
  }
}
if (import.meta.main) await run(main);
