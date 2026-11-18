import { parseArgs } from "@std/cli/parse-args";
import { join, resolve } from "@std/path";
import { zipSync } from "fflate";
import { githubOutput, repository, run, workspaceVersion } from "./common.ts";

const binaries = ["quartz.exe", "quartz-host.exe"];

async function hash(bytes: Uint8Array<ArrayBuffer>): Promise<string> {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  return Array.from(digest, (byte) => byte.toString(16).padStart(2, "0")).join(
    "",
  );
}

async function main(): Promise<void> {
  const options = parseArgs(Deno.args, {
    string: ["source", "output", "version"],
    default: { source: join(repository, "target/release") },
    unknown: (argument) => {
      if (!argument.startsWith("-")) return true;
      throw new Error(`Unknown argument: ${argument}`);
    },
  });
  const [mode] = options._;
  if (
    (mode !== "collect" && mode !== "release") || options._.length !== 1 ||
    !options.output
  ) {
    throw new Error(
      "Usage: deno task package <collect|release> --output <directory> [--source <directory>] [--version <version>]",
    );
  }
  const source = resolve(options.source);
  const output = resolve(options.output);
  if (source === output) {
    throw new Error("Source and output directories must differ");
  }
  const version = options.version ?? await workspaceVersion();
  if (!/^\d+\.\d+\.\d+(?:-[\w.-]+)?(?:\+[\w.-]+)?$/.test(version)) {
    throw new Error(`Invalid package version: ${version}`);
  }
  // Read both binaries before writing output, so missing input fails without a partial package.
  const entries: Record<string, Uint8Array<ArrayBuffer>> = {};
  const sums: string[] = [];
  for (const binary of binaries) {
    const bytes = await Deno.readFile(join(source, binary));
    entries[binary] = bytes;
    sums.push(`${await hash(bytes)}  ${binary}\n`);
  }
  entries.SHA256SUMS = new TextEncoder().encode(sums.join(""));
  await Deno.mkdir(output, { recursive: true });
  for (const [name, bytes] of Object.entries(entries)) {
    await Deno.writeFile(join(output, name), bytes);
  }
  if (mode === "collect") {
    console.log(`Collected release binaries and SHA256SUMS in ${output}`);
    return;
  }
  const name = `quartz-${version}-x86_64-pc-windows-msvc.zip`;
  const archive = join(output, name);
  const checksum = `${archive}.sha256`;
  const zip = zipSync(entries, { level: 9 });
  await Deno.writeFile(archive, zip);
  await Deno.writeTextFile(checksum, `${await hash(zip)}  ${name}\n`);
  await githubOutput({ archive, checksum });
  console.log(`Packaged ${archive}`);
}

if (import.meta.main) await run(main);
