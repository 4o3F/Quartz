import { fromFileUrl } from "@std/path";
import { parse } from "@std/toml";

export const repository = fromFileUrl(new URL("../", import.meta.url));

export async function workspaceVersion(): Promise<string> {
  const manifest = parse(
    await Deno.readTextFile(new URL("../Cargo.toml", import.meta.url)),
  );
  const workspace = manifest.workspace as Record<string, unknown> | undefined;
  const metadata = workspace?.package as Record<string, unknown> | undefined;
  const version = metadata?.version;
  if (
    typeof version !== "string" ||
    !/^\d+\.\d+\.\d+(?:-[\w.-]+)?(?:\+[\w.-]+)?$/.test(version)
  ) {
    throw new Error(
      "Cargo.toml must contain a valid [workspace.package] version",
    );
  }
  return version;
}

export async function githubOutput(
  values: Record<string, string>,
): Promise<void> {
  const output = Deno.env.get("GITHUB_OUTPUT");
  if (output) {
    await Deno.writeTextFile(
      output,
      Object.entries(values).map(([key, value]) => `${key}=${value}\n`).join(
        "",
      ),
      { append: true },
    );
  }
}

export async function run(main: () => Promise<void>): Promise<void> {
  try {
    await main();
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    Deno.exit(1);
  }
}
