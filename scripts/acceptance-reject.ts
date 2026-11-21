import { fromFileUrl } from "@std/path";
import { run } from "./common.ts";

class RejectNonQuartzClient {
  async run(): Promise<void> {
    if (Deno.build.os !== "windows") {
      throw new Error(
        "Run acceptance-reject from Windows Deno in the acceptance PowerShell",
      );
    }
    const [flag, root, ...extra] = Deno.args;
    if (flag !== "--data-root" || !root || extra.length) {
      throw new Error(
        "Usage: deno task acceptance-reject --data-root <isolated Quartz directory>",
      );
    }
    const result = await new Deno.Command("powershell.exe", {
      args: [
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        fromFileUrl(new URL("./acceptance-reject.ps1", import.meta.url)),
        "-DataRoot",
        root,
      ],
      stdout: "inherit",
      stderr: "inherit",
    }).output();
    if (!result.success) {
      throw new Error(
        `Non-Quartz rejection probe failed (exit ${result.code})`,
      );
    }
  }
}
if (import.meta.main) await run(() => new RejectNonQuartzClient().run());
