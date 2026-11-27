import { join } from "@std/path";
import { repository, run } from "./common.ts";

const targets = ["chromium", "firefox"] as const;

async function sourceFiles(sourceDir: string): Promise<string[]> {
  const files: string[] = [];
  for await (const entry of Deno.readDir(sourceDir)) {
    if (
      entry.isFile &&
      entry.name.endsWith(".js") &&
      !entry.name.endsWith("_test.js")
    ) {
      files.push(entry.name);
    }
  }
  files.sort();
  if (files.length === 0) {
    throw new Error(`No extension source files found in ${sourceDir}`);
  }
  return files;
}

async function assembleTarget(
  extensionRoot: string,
  outputRoot: string,
  files: readonly string[],
  target: (typeof targets)[number],
): Promise<string> {
  const targetDir = join(outputRoot, target);
  const targetSrcDir = join(targetDir, "src");
  try {
    await Deno.remove(targetDir, { recursive: true });
  } catch (error) {
    if (!(error instanceof Deno.errors.NotFound)) {
      throw error;
    }
  }
  await Deno.mkdir(targetSrcDir, { recursive: true });
  await Deno.copyFile(
    join(extensionRoot, `manifest.${target}.json`),
    join(targetDir, "manifest.json"),
  );
  for (const file of files) {
    await Deno.copyFile(
      join(extensionRoot, "src", file),
      join(targetSrcDir, file),
    );
  }
  return targetDir;
}

async function main(): Promise<void> {
  const extensionRoot = join(repository, "extension");
  const outputRoot = join(repository, "target/extension");
  const files = await sourceFiles(join(extensionRoot, "src"));
  for (const target of targets) {
    const dir = await assembleTarget(extensionRoot, outputRoot, files, target);
    console.log(`Packaged ${target} extension in ${dir}`);
  }
}

if (import.meta.main) await run(main);
