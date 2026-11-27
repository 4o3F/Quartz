import { parseArgs } from "@std/cli/parse-args";
import { join, resolve } from "@std/path";
import { run } from "./common.ts";
import { Windows } from "./windows.ts";

const sleep = (milliseconds: number) =>
  new Promise<void>((done) => setTimeout(done, milliseconds));
// 07 section 2: default window size; 11 section 9.3: appearance and language matrix.
export const languages = ["zh-CN", "en", "en-XA"] as const;
export const scenarios = [
  "today",
  "loading",
  "empty-today",
  "empty-past",
  "stopped-paused",
  "stopped-persistence-failed",
  "query-failed",
  "date-picker",
  "today-nav",
  "today-details",
  "today-app-details",
  "core-not-running",
  "core-not-running-starting",
  "core-start-failed",
  "connect-failed",
  "migration-failed",
  "newer-database",
  "version-mismatch",
  "other-session",
  "core-not-running-expanded",
  "connect-failed-expanded",
  "other-session-expanded",
  "core-not-running-starting-expanded",
  "rules",
  "rules-categories",
  "rule-editor",
  "rule-editor-preview",
  "rule-import",
  "unclassified",
  "progress",
  "today-annotations",
  "annotation-editor",
  "timeline-select",
  "classify-menu",
  "week",
  "week-loading",
  "settings",
  "settings-resident-off",
  "settings-data",
  "settings-delete-confirm",
  "settings-about",
] as const;
export const narrowScenarios = [
  "today",
  "migration-failed",
  "rules",
  "rules-categories",
  "rule-editor",
  "rule-editor-preview",
  "rule-import",
  "unclassified",
  "progress",
  "today-annotations",
  "annotation-editor",
  "timeline-select",
  "classify-menu",
  "week",
  "settings",
] as const;
const closeMessage = 0x10;
const windowDeadlineMs = 30000;
const settleMs = 1200;
// 11 section 9.3: allow debug GPU teardown during capture; record the actual exit time.
const closeDeadlineMs = 5000;

interface Shot {
  name: string;
  theme: string;
  fallback: string;
  contrast: string;
  placement: string;
}
const variants: Shot[] = [
  {
    name: "light",
    theme: "light",
    fallback: "0",
    contrast: "0",
    placement: "1",
  },
  { name: "dark", theme: "dark", fallback: "0", contrast: "0", placement: "1" },
  {
    name: "fallback-light",
    theme: "light",
    fallback: "1",
    contrast: "0",
    placement: "1",
  },
  {
    name: "fallback-dark",
    theme: "dark",
    fallback: "1",
    contrast: "0",
    placement: "1",
  },
  {
    name: "high-contrast",
    theme: "dark",
    fallback: "0",
    contrast: "1",
    placement: "1",
  },
];

/** 11 section 2: a visibly blue wallpaper isolates native material tint from a neutral user wallpaper. */
const micaReference = { width: 8, height: 8, red: 20, green: 80, blue: 180 };
/** Windows BMP file header, BITMAPINFOHEADER and 24-bit BGR pixels for the Mica test fixture. */
const bitmapFormat = {
  headerBytes: 54,
  infoBytes: 40,
  signature: 0x4d42,
  planes: 1,
  bitsPerPixel: 24,
  bytesPerPixel: 3,
};
function referenceWallpaper(): Uint8Array {
  const pixels = micaReference.width * micaReference.height *
    bitmapFormat.bytesPerPixel;
  const image = new Uint8Array(bitmapFormat.headerBytes + pixels);
  const header = new DataView(image.buffer);
  header.setUint16(0, bitmapFormat.signature, true);
  header.setUint32(2, image.length, true);
  header.setUint32(10, bitmapFormat.headerBytes, true);
  header.setUint32(14, bitmapFormat.infoBytes, true);
  header.setInt32(18, micaReference.width, true);
  header.setInt32(22, micaReference.height, true);
  header.setUint16(26, bitmapFormat.planes, true);
  header.setUint16(28, bitmapFormat.bitsPerPixel, true);
  header.setUint32(34, pixels, true);
  for (
    let offset = bitmapFormat.headerBytes;
    offset < image.length;
    offset += bitmapFormat.bytesPerPixel
  ) {
    image.set(
      [micaReference.blue, micaReference.green, micaReference.red],
      offset,
    );
  }
  return image;
}

async function main(): Promise<void> {
  if (Deno.build.os !== "windows") {
    throw new Error("Preview screenshots require a Windows desktop");
  }
  const options = parseArgs(Deno.args, {
    string: ["exe", "mica-wallpaper"],
    default: { "mica-wallpaper": "current" },
    unknown: (argument) => {
      throw new Error(`Unknown argument: ${argument}`);
    },
  });
  if (options._.length) {
    throw new Error("Use --exe=<preview.exe> for an explicit preview path");
  }
  const wallpaper = options["mica-wallpaper"];
  if (wallpaper !== "current" && wallpaper !== "reference") {
    throw new Error("--mica-wallpaper must be current or reference");
  }
  const exe = await Deno.realPath(
    options.exe ?? "target/windows/debug/examples/preview.exe",
  );
  if (!exe.toLowerCase().endsWith("\\preview.exe")) {
    throw new Error("Use the mock-data preview.exe");
  }
  const destination = resolve("target/m3-screenshots");
  await Deno.mkdir(destination, { recursive: true });
  const windows = new Windows();
  const results: {
    file: string;
    backdrop: number;
    active: boolean;
    capture: string;
    closeMs: number;
    wallpaper: string;
  }[] = [];
  const variables = [
    "THEME",
    "STATE",
    "LANG",
    "OFFSCREEN",
    "SIZE",
    "TEXT_SCALE",
    "MICA_FALLBACK",
    "HIGH_CONTRAST",
    "DIAGNOSTICS",
  ].map((name) => `QUARTZ_PREVIEW_${name}`);
  const previous = new Map(variables.map((name) => [name, Deno.env.get(name)]));
  const capture = async (
    shot: Shot,
    state: string,
    language: string,
    minimum = false,
  ) => {
    const width = minimum ? 640 : 1280;
    const height = minimum ? 480 : 800;
    await Deno.mkdir(join(destination, language), { recursive: true });
    const settings: Record<string, string> = {
      THEME: shot.theme,
      STATE: state,
      LANG: language,
      OFFSCREEN: shot.placement,
      SIZE: `${width}x${height}`,
      TEXT_SCALE: "1",
      MICA_FALLBACK: shot.fallback,
      HIGH_CONTRAST: shot.contrast,
    };
    for (const [key, value] of Object.entries(settings)) {
      Deno.env.set(`QUARTZ_PREVIEW_${key}`, value);
    }
    if (shot.placement === "0") {
      Deno.env.set(
        "QUARTZ_PREVIEW_DIAGNOSTICS",
        join(destination, "real-mica-layers.json"),
      );
    } else Deno.env.delete("QUARTZ_PREVIEW_DIAGNOSTICS");
    const child = windows.start(exe);
    try {
      let window: Deno.PointerValue = null;
      const deadline = performance.now() + windowDeadlineMs;
      while (!window && performance.now() < deadline) {
        if (windows.wait(child, 0)) {
          throw new Error(`Preview exited: ${windows.exitCode(child)}`);
        }
        window = windows.mainWindow(child.id);
        if (!window) await sleep(50);
      }
      if (!window) throw new Error("Timed out waiting for preview window");
      await sleep(settleMs);
      const mode = shot.placement === "0" ? "desktop" : "window";
      if (mode === "desktop" && !windows.isForeground(window)) {
        windows.activate(window);
      }
      await sleep(settleMs);
      const file = `${language}/${state}${
        minimum ? "-640x480" : ""
      }-${shot.name}.png`;
      await windows.saveWindow(window, join(destination, file), mode, {
        width,
        height,
      });
      const observation = {
        file,
        backdrop: windows.backdrop(window),
        active: windows.isForeground(window),
        capture: mode,
        wallpaper: shot.placement === "0" ? wallpaper : "current",
      };
      const closing = performance.now();
      windows.post(window, closeMessage);
      if (!windows.wait(child, closeDeadlineMs)) {
        throw new Error(`Preview did not close within ${closeDeadlineMs} ms`);
      }
      if (windows.exitCode(child) !== 0) {
        throw new Error(`Preview failed: ${windows.exitCode(child)}`);
      }
      const completed = {
        ...observation,
        closeMs: Math.round(performance.now() - closing),
      };
      results.push(completed);
      console.log(JSON.stringify(completed));
    } finally {
      windows.terminate(child);
      windows.closeProcess(child);
    }
  };
  try {
    for (const variant of variants) {
      for (const state of scenarios) {
        for (const language of languages) {
          await capture(variant, state, language);
        }
      }
    }
    for (const variant of variants) {
      for (const language of languages) {
        for (const state of narrowScenarios) {
          await capture(variant, state, language, true);
        }
      }
    }
    const captureMica = () =>
      capture(
        {
          name: "real-mica-active",
          theme: "light",
          fallback: "0",
          contrast: "0",
          placement: "0",
        },
        "today",
        "zh-CN",
      );
    if (wallpaper === "reference") {
      const fixture = join(destination, "mica-wallpaper.bmp");
      await Deno.writeFile(fixture, referenceWallpaper());
      await windows.withDesktopWallpaper(fixture, captureMica);
    } else await captureMica();
    await Deno.writeTextFile(
      join(destination, "index.md"),
      "# M3 screenshot review\n\n" +
        languages.map((language) =>
          `## ${language}\n\n` +
          results.filter((result) => result.file.startsWith(`${language}/`))
            .map((result) =>
              `### ${result.file}\n\n![${result.file}](${result.file})\n`
            ).join("\n")
        ).join("\n"),
    );
    await Deno.writeTextFile(
      join(destination, "observations.json"),
      JSON.stringify(results, null, 2) + "\n",
    );
  } finally {
    for (const [name, value] of previous) {
      if (value === undefined) Deno.env.delete(name);
      else Deno.env.set(name, value);
    }
    windows.close();
  }
}

if (import.meta.main) await run(main);
