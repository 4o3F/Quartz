import { parseArgs } from "@std/cli/parse-args";
import { dirname, join } from "@std/path";
import { run } from "./common.ts";
import {
  machineMetadata,
  type Process,
  type Snapshot,
  Windows,
} from "./windows.ts";

const sleep = (milliseconds: number) =>
  new Promise<void>((resolve) => setTimeout(resolve, milliseconds));
const mebibyte = 1024 * 1024;
const messages = { close: 0x10, trayCallback: 0x8001, select: 0x400 };

function parameters(): {
  exe: string;
  settle: number;
  idle: number;
  screenshot?: string;
} {
  // Accept the original PowerShell parameter spellings as well as --name.
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
    string: ["Exe", "SettleSeconds", "IdleSeconds", "ScreenshotPath"],
    default: { SettleSeconds: "30", IdleSeconds: "600" },
    unknown: (arg) => {
      throw new Error(`Unknown argument: ${arg}`);
    },
  });
  if (!options.Exe) {
    throw new Error(
      "Usage: deno task measure --Exe <quartz.exe> [--SettleSeconds 30] [--IdleSeconds 600] [--ScreenshotPath <file.png>]",
    );
  }
  const seconds = (value: string, name: string) => {
    const number = Number(value);
    if (
      !/^\d+$/.test(value) || !Number.isInteger(number) || number < 1 ||
      number > 3600
    ) {
      throw new Error(`${name} must be an integer from 1 to 3600`);
    }
    return number;
  };
  return {
    exe: options.Exe,
    settle: seconds(options.SettleSeconds, "SettleSeconds"),
    idle: seconds(options.IdleSeconds, "IdleSeconds"),
    screenshot: options.ScreenshotPath,
  };
}

function timestamp(): string {
  const date = new Date();
  const pad = (value: number) => String(value).padStart(2, "0");
  const offset = -date.getTimezoneOffset();
  const zone = `${offset < 0 ? "-" : "+"}${
    pad(Math.floor(Math.abs(offset) / 60))
  }:${pad(Math.abs(offset) % 60)}`;
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${
    pad(date.getDate())
  } ${pad(date.getHours())}:${pad(date.getMinutes())}:${
    pad(date.getSeconds())
  } ${zone}`;
}

// PowerShell's Math.Round uses ties-to-even for the summary values.
function rounded(value: number): string {
  const scaled = Math.abs(value) * 1000;
  const lower = Math.floor(scaled);
  const integer = scaled - lower === 0.5
    ? lower + (lower % 2)
    : Math.round(scaled);
  return String(Math.sign(value) * integer / 1000);
}

async function main(): Promise<void> {
  const options = parameters();
  if (Deno.build.os !== "windows" || Deno.build.arch !== "x86_64") {
    throw new Error(
      "Resource measurement requires x86_64 Windows in a desktop session",
    );
  }
  const exe = await Deno.realPath(options.exe);
  const windows = new Windows();
  let core: Process | undefined;
  let maxCloseMs = 0;
  try {
    if (windows.findCore()) {
      throw new Error(
        "A Quartz core is already running; close it before measuring",
      );
    }
    const running = windows.matchingProcesses(exe);
    for (const process of running) windows.closeProcess(process);
    if (running.length) {
      throw new Error(
        "The executable is already running; close it before measuring",
      );
    }
    // Machine metadata is read before sampling; WMI performance counters are unused.
    const metadata = machineMetadata();
    core = windows.start(exe);
    const coreProcess = core;
    const uiChildren = () => windows.matchingProcesses(exe, coreProcess.id);
    const countChildren = () => {
      const children = uiChildren();
      for (const child of children) windows.closeProcess(child);
      return children.length;
    };
    const openUi = async (): Promise<Process> => {
      const coreWindow = windows.findCore();
      if (!coreWindow) throw new Error("Core hidden window was not created");
      windows.post(coreWindow, messages.trayCallback, messages.select);
      const deadline = performance.now() + 30000;
      do {
        if (windows.wait(coreProcess, 0)) {
          throw new Error("Core exited while opening UI");
        }
        const children = uiChildren();
        let selected: Process | undefined;
        try {
          if (children.length > 1) {
            throw new Error(`Expected one UI child, found ${children.length}`);
          }
          if (children.length === 1) {
            const child = children[0];
            if (windows.mainWindow(child.id)) {
              selected = child;
              return child;
            }
            if (windows.wait(child, 0)) {
              throw new Error(
                `UI exited before opening a window: ${windows.exitCode(child)}`,
              );
            }
          }
        } finally {
          for (const child of children) {
            if (child !== selected) windows.closeProcess(child);
          }
        }
        await sleep(50);
      } while (performance.now() < deadline);
      throw new Error(
        "Timed out waiting for UI window; inspect Quartz/logs/ui.log",
      );
    };
    const closeUi = async (process: Process) => {
      try {
        const window = windows.mainWindow(process.id);
        if (!window) throw new Error(`UI PID ${process.id} has no main window`);
        const start = performance.now();
        windows.post(window, messages.close);
        if (!windows.wait(process, 1000)) {
          throw new Error(
            `UI PID ${process.id} did not exit within one second of WM_CLOSE`,
          );
        }
        const elapsed = performance.now() - start;
        const code = windows.exitCode(process);
        if (code !== 0) throw new Error(`UI exit code ${code}`);
        maxCloseMs = Math.max(maxCloseMs, elapsed);
      } finally {
        windows.closeProcess(process);
      }
      // Let the core's process-handle notification restore its launch state.
      await sleep(150);
      if (countChildren() !== 0) {
        throw new Error("UI children remain after WM_CLOSE");
      }
    };
    await sleep(options.settle * 1000);
    const coreWindow = windows.findCore();
    if (!coreWindow) throw new Error("Core hidden window was not created");
    if (windows.windowProcessId(coreWindow) !== core.id) {
      throw new Error("Hidden window belongs to a different core");
    }
    const start = windows.snapshot(core, "core idle start");
    await sleep(options.idle * 1000);
    const before = windows.snapshot(core, "core idle");
    before.IdleMs = before.CpuMs - start.CpuMs;
    const modulesBefore = windows.modules(core);

    const ui = await openUi();
    let uiOpen: Snapshot;
    let uiModules: string[];
    const uiIdleSeconds = Math.min(30, options.idle);
    try {
      await sleep(options.settle * 1000);
      const uiStart = windows.snapshot(ui, "UI idle start");
      await sleep(uiIdleSeconds * 1000);
      uiOpen = windows.snapshot(ui, "UI open");
      uiOpen.IdleMs = uiOpen.CpuMs - uiStart.CpuMs;
      uiModules = windows.modules(ui);
      if (options.screenshot) {
        const window = windows.mainWindow(ui.id);
        if (!window) throw new Error("UI window disappeared before screenshot");
        await windows.saveWindow(window, options.screenshot);
      }
    } catch (error) {
      windows.closeProcess(ui);
      throw error;
    }
    await closeUi(ui);
    const cycleBefore = windows.snapshot(
      core,
      "core before 20 cycles (first UI closed)",
    );
    const cycleRows: Snapshot[] = [];
    for (let cycle = 1; cycle <= 20; cycle++) {
      const child = await openUi();
      // Hold the visible window for rendering; its working set is left intact.
      await sleep(500);
      await closeUi(child);
      if (cycle % 5 === 0) {
        cycleRows.push(windows.snapshot(core, `core after ${cycle} cycles`));
      }
    }
    await sleep(options.settle * 1000);
    const afterStart = windows.snapshot(core, "core after cycles idle start");
    await sleep(uiIdleSeconds * 1000);
    const after = windows.snapshot(core, "core after 20 cycles, settled");
    after.IdleMs = after.CpuMs - afterStart.CpuMs;
    const modulesAfter = windows.modules(core);
    const coreModules = [...new Set([...modulesBefore, ...modulesAfter])];
    const nvModules = coreModules.filter((name) => /^nv.*\.dll$/i.test(name));
    const d3dModules = coreModules.filter((name) => /^d3d.*\.dll$/i.test(name));
    const childrenLeft = countChildren();
    const privateDelta = (after.PrivateBytes - cycleBefore.PrivateBytes) /
      mebibyte;
    const wsDelta = (after.PrivateWS - cycleBefore.PrivateWS) / mebibyte;
    const threadDelta = after.Threads - cycleBefore.Threads;
    const handleDelta = after.Handles - cycleBefore.Handles;

    console.log(`Date: ${timestamp()}`);
    console.log(
      `CPU: ${metadata.cpu}; GPU: ${metadata.gpu}; OS: ${metadata.caption}, build ${metadata.build}`,
    );
    console.log(
      `Settle: ${options.settle} s; core idle: ${options.idle} s; UI/post-cycle idle: ${uiIdleSeconds} s`,
    );
    console.log(
      "| Scenario | Private Bytes (MiB) | Private WS (MiB) | Threads | Handles | Total CPU (ms) | Idle CPU (ms) |",
    );
    console.log("|---|---:|---:|---:|---:|---:|---:|");
    for (const row of [before, uiOpen, cycleBefore, ...cycleRows, after]) {
      const idle = row.IdleMs === null ? "-" : row.IdleMs.toFixed(3);
      console.log(
        `| ${row.Scenario} | ${(row.PrivateBytes / mebibyte).toFixed(3)} | ${
          (row.PrivateWS / mebibyte).toFixed(3)
        } | ${row.Threads} | ${row.Handles} | ${
          row.CpuMs.toFixed(3)
        } | ${idle} |`,
      );
    }
    console.log(
      `Core 20-cycle deltas (from first UI closed): Private Bytes ${
        rounded(privateDelta)
      } MiB; Private WS ${
        rounded(wsDelta)
      } MiB; threads ${threadDelta}; handles ${handleDelta}.`,
    );
    console.log(
      `Core first-UI deltas (from initial idle): Private Bytes ${
        rounded((cycleBefore.PrivateBytes - before.PrivateBytes) / mebibyte)
      } MiB; Private WS ${
        rounded((cycleBefore.PrivateWS - before.PrivateWS) / mebibyte)
      } MiB; threads ${cycleBefore.Threads - before.Threads}; handles ${
        cycleBefore.Handles - before.Handles
      }.`,
    );
    console.log(
      `UI children remaining: ${childrenLeft}; maximum WM_CLOSE-to-exit: ${
        rounded(maxCloseMs)
      } ms.`,
    );
    console.log(
      `Core nv*.dll check: ${
        nvModules.length ? `FAIL: ${nvModules.join(", ")}` : "PASS (none)"
      }.`,
    );
    console.log(
      `Core d3d*.dll check: ${
        d3dModules.length
          ? `PRESENT (static-import observation): ${d3dModules.join(", ")}`
          : "PASS (none)"
      }.`,
    );
    console.log(
      `Core Private Bytes growth: ${
        privateDelta <= 1
          ? "PASS (target <= 1 MiB)"
          : privateDelta <= 2
          ? "above target, within 2 MiB limit"
          : "FAIL (> 2 MiB)"
      }.`,
    );
    console.log(
      `Core threads/handles growth: ${
        threadDelta <= 0 && handleDelta <= 0 ? "PASS (no growth)" : "FAIL"
      }.`,
    );
    const moduleList = (names: string[]) => `\`${names.join("\`, \`")}\``;
    console.log(`Core loaded modules before UI: ${moduleList(modulesBefore)}.`);
    console.log(
      `Core loaded modules after cycles: ${moduleList(modulesAfter)}.`,
    );
    console.log(`UI loaded modules: ${moduleList(uiModules)}.`);
    console.log(`quartz.exe: ${(await Deno.stat(exe)).size} bytes.`);
    const hostExe = join(dirname(exe), "quartz-host.exe");
    try {
      console.log(`quartz-host.exe: ${(await Deno.stat(hostExe)).size} bytes.`);
    } catch (error) {
      if (!(error instanceof Deno.errors.NotFound)) throw error;
    }
    if (
      nvModules.length || childrenLeft || privateDelta > 2 || threadDelta > 0 ||
      handleDelta > 0
    ) {
      throw new Error("Resource acceptance failed; see the measurements above");
    }
  } finally {
    try {
      if (core) {
        try {
          for (const child of windows.matchingProcesses(exe, core.id)) {
            try {
              windows.terminate(child);
            } finally {
              windows.closeProcess(child);
            }
          }
        } finally {
          try {
            if (!windows.wait(core, 0)) {
              const window = windows.findCore();
              if (window) windows.post(window, messages.close);
              if (!windows.wait(core, 5000)) windows.terminate(core);
            }
          } finally {
            windows.closeProcess(core);
          }
        }
      }
    } finally {
      windows.close();
    }
  }
}

if (import.meta.main) await run(main);
