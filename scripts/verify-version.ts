import { githubOutput, run, workspaceVersion } from "./common.ts";

async function main(): Promise<void> {
  if (Deno.args.length > 1) {
    throw new Error("Usage: deno task verify-version [v<version>]");
  }
  const tag = Deno.args[0] ?? Deno.env.get("GITHUB_REF_NAME");
  if (!tag) throw new Error("Pass a tag argument or set GITHUB_REF_NAME");
  const version = await workspaceVersion();
  if (tag !== `v${version}`) {
    throw new Error(
      `Tag ${JSON.stringify(tag)} does not match workspace version ${
        JSON.stringify(version)
      }`,
    );
  }
  await githubOutput({ version });
  console.log(`Tag ${tag} matches workspace version ${version}`);
}

if (import.meta.main) await run(main);
