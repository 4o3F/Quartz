import { parseArgs } from "@std/cli/parse-args";
import { join } from "@std/path";
import { repository, run } from "./common.ts";

const install =
  "cargo install diesel_cli --version 2.4.0 --no-default-features --features sqlite-bundled --locked";

async function main(): Promise<void> {
  const options = parseArgs(Deno.args, {
    string: ["diesel"],
    default: { diesel: "diesel" },
    unknown: (argument) => {
      throw new Error(`Unknown argument: ${argument}`);
    },
  });
  const scratch = await Deno.makeTempDir({ prefix: "quartz-schema-" });
  try {
    const config = join(scratch, "diesel.toml");
    const database = join(scratch, "schema.db");
    await Deno.writeTextFile(config, "");
    const command = (args: string[]) =>
      new Deno.Command(options.diesel, {
        args,
        cwd: repository,
        stdin: "null",
        stderr: "inherit",
      });
    const migrated = await command([
      "migration",
      "run",
      "--database-url",
      database,
      "--migration-dir",
      "crates/quartz/migrations",
      "--config-file",
      config,
    ]).output();
    await Deno.stdout.write(migrated.stdout);
    if (!migrated.success) {
      throw new Error(`Diesel migration failed (exit ${migrated.code})`);
    }
    // print-schema uses the repository's diesel.toml and schema.patch.
    const generated = await command([
      "print-schema",
      "--database-url",
      database,
    ]).output();
    if (!generated.success) {
      throw new Error(`Diesel print-schema failed (exit ${generated.code})`);
    }
    const tracked = join(repository, "crates/quartz/src/store/schema.rs");
    const expected = await Deno.readFile(tracked);
    if (
      expected.length === generated.stdout.length &&
      expected.every((byte, i) => byte === generated.stdout[i])
    ) {
      console.log("Schema matches crates/quartz/src/store/schema.rs");
      return;
    }
    const actual = join(scratch, "schema.rs");
    await Deno.writeFile(actual, generated.stdout);
    const diff = await new Deno.Command("git", {
      args: [
        "--no-pager",
        "diff",
        "--no-index",
        "--no-ext-diff",
        "--text",
        "--",
        tracked,
        actual,
      ],
      cwd: repository,
      stdout: "inherit",
      stderr: "inherit",
    }).spawn().status;
    if (diff.code > 1) {
      throw new Error(`Could not display schema diff (git exit ${diff.code})`);
    }
    throw new Error(
      "Schema drift detected; regenerate crates/quartz/src/store/schema.rs with Diesel CLI 2.4.0",
    );
  } catch (error) {
    if (
      error instanceof Deno.errors.NotFound &&
      error.message.includes(options.diesel)
    ) {
      throw new Error(
        `Diesel CLI was not found: ${options.diesel}\nInstall it with:\n${install}`,
      );
    }
    throw error;
  } finally {
    await Deno.remove(scratch, { recursive: true });
  }
}

if (import.meta.main) await run(main);
