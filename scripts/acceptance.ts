import { run } from "./common.ts";

export const usage =
  `Usage: deno task acceptance [dump.jsonl|-] [--before pre-crash.jsonl]
Export running-core data with quartz dump --date YYYY-MM-DD --format json.
Use quartz dump --offline for a stopped or suspended core and frozen pre-crash dumps.`;

const dayMs = 86_400_000;
const kinds = [
  "run",
  "clock_segment",
  "foreground",
  "no_foreground",
  "availability",
  "capture_gap",
  "timed_policy",
] as const;
type Kind = typeof kinds[number];
export type Rule =
  | "format"
  | "coverage"
  | "idle"
  | "midnight"
  | "recovery"
  | "privacy"
  | "clock";
export const rules: readonly Rule[] = [
  "format",
  "coverage",
  "idle",
  "midnight",
  "recovery",
  "privacy",
  "clock",
];
export interface Finding {
  rule: Rule;
  detail: string;
}
export interface Fact extends Record<string, unknown> {
  kind: Kind;
  id: number;
  run_id: number | null;
  segment_id: number | null;
  start_utc_ms: number;
  end_utc_ms: number | null;
  confirmed_utc_ms: number | null;
  start_mono_ms: number | null;
  end_mono_ms: number | null;
  confirmed_mono_ms: number | null;
  duration_ms: number | null;
  end_reason: string | null;
}
const isInteger = (value: unknown): value is number =>
  typeof value === "number" && Number.isSafeInteger(value);
const present = (value: unknown) => value !== null && value !== undefined;
const endpoint = (row: Fact) => row.end_utc_ms ?? row.confirmed_utc_ms!;
const monoEnd = (row: Fact) => row.end_mono_ms ?? row.confirmed_mono_ms!;
const isInterval = (row: Fact) =>
  row.kind === "foreground" || row.kind === "no_foreground";
const isSpan = (row: Fact) =>
  isInterval(row) || row.kind === "availability" || row.kind === "capture_gap";
const key = (row: Fact) =>
  `${isInterval(row) ? "interval" : row.kind}:${row.id}`;
const label = (row: Fact) =>
  `${row.kind} ${row.id} (run ${row.run_id}, segment ${row.segment_id})`;

/** Parse dump JSON Lines, retaining unknown fields for privacy checks. */
export function parseDump(input: string): Fact[] {
  const rows: Fact[] = [];
  for (
    const [index, line] of input.replace(/^\uFEFF/, "").split(/\r?\n/).entries()
  ) {
    if (!line.trim()) continue;
    const fail = (detail: string): never => {
      throw new Error(`line ${index + 1}: ${detail}`);
    };
    let value: unknown;
    try {
      value = JSON.parse(line);
    } catch {
      fail("invalid JSON");
    }
    if (typeof value !== "object" || value === null || Array.isArray(value)) {
      fail("expected an object");
    }
    const object = value as Record<string, unknown>;
    if (!kinds.includes(object.kind as Kind)) fail("unknown kind");
    for (const name of ["id", "start_utc_ms"]) {
      if (!isInteger(object[name])) fail(`${name} must be a safe integer`);
    }
    for (
      const name of [
        "run_id",
        "segment_id",
        "end_utc_ms",
        "confirmed_utc_ms",
        "start_mono_ms",
        "end_mono_ms",
        "confirmed_mono_ms",
        "duration_ms",
      ]
    ) {
      if (object[name] !== null && !isInteger(object[name])) {
        fail(`${name} must be an integer or null`);
      }
    }
    if (object.end_reason !== null && typeof object.end_reason !== "string") {
      fail("invalid end_reason");
    }
    const row = object as Fact;
    const closed = row.end_utc_ms !== null;
    if (closed === (row.confirmed_utc_ms !== null)) {
      fail("exactly one UTC endpoint is required");
    }
    if (
      row.kind !== "capture_gap" && row.kind !== "timed_policy" &&
      closed !== (row.end_reason !== null)
    ) fail("end_reason must agree with open/closed state");
    if (row.kind !== "run" && row.kind !== "timed_policy") {
      if (
        !isInteger(row.start_mono_ms) || row.start_mono_ms < 0 ||
        (closed
          ? !isInteger(row.end_mono_ms) || row.confirmed_mono_ms !== null
          : !isInteger(row.confirmed_mono_ms) || row.end_mono_ms !== null)
      ) fail("invalid monotonic endpoints");
    }
    if (
      row.kind !== "timed_policy" && (!isInteger(row.run_id) || row.run_id <= 0)
    ) fail("missing run_id");
    if (
      row.kind !== "run" && row.kind !== "timed_policy" &&
      (!isInteger(row.segment_id) || row.segment_id <= 0)
    ) fail("missing segment_id");
    if (row.kind === "run" && row.id !== row.run_id) {
      fail("run identity mismatch");
    }
    if (row.kind === "clock_segment" && row.id !== row.segment_id) {
      fail("clock segment identity mismatch");
    }
    if (isInterval(row)) {
      for (const name of ["sequence", "session_key"]) {
        if (!isInteger(row[name])) fail(`missing ${name}`);
      }
      if (
        !["full", "title_hidden", "excluded"].includes(
          String(row.capture_level),
        )
      ) fail("invalid capture_level");
    }
    if (
      row.kind === "availability" &&
      !["idle", "locked", "disconnected", "suspended", "paused"].includes(
        String(row.reason),
      )
    ) fail("invalid availability reason");
    if (
      row.kind === "capture_gap" &&
      !["event_overload", "write_queue_overload", "database_error"].includes(
        String(row.reason),
      )
    ) fail("invalid gap reason");
    if (!isInteger(row.duration_ms)) fail("missing duration_ms");
    rows.push(row);
  }
  return rows;
}

/** Adjacent day dumps repeat envelopes and midnight rows; conflicting snapshots fail. */
export function uniqueRows(rows: readonly Fact[]): Fact[] {
  const found = new Map<string, Fact>();
  const canonical = (row: Fact) =>
    JSON.stringify(
      Object.fromEntries(
        Object.entries(row).sort(([a], [b]) => a.localeCompare(b)),
      ),
    );
  for (const row of rows) {
    const previous = found.get(key(row));
    if (previous && canonical(previous) !== canonical(row)) {
      throw new Error(
        `conflicting snapshots for ${
          key(row)
        }; dump all dates with quartz dump --offline while the core is stopped or suspended`,
      );
    }
    found.set(key(row), row);
  }
  return [...found.values()];
}

function checkPrivacy(
  row: Fact,
  add: (rule: Rule, detail: string) => void,
): void {
  const title = /^(title|title_id|tab_title|tab_title_id)$/;
  const app =
    /^(app|application)(_|$)|^(exe|exe_name|exe_path|aumid|identity_key|window_instance_id|hwnd|pid|window_class|window_generation)$/;
  const domain = /^(domain|host|url)(_|$)/;
  for (const [name, value] of Object.entries(row)) {
    if (!present(value)) continue;
    if (
      row.kind === "capture_gap" &&
      (title.test(name) || app.test(name) || domain.test(name))
    ) add("privacy", `${label(row)} carries ${name}`);
    if (
      ["title_hidden", "excluded"].includes(String(row.capture_level)) &&
      title.test(name)
    ) add("privacy", `${label(row)} carries ${name}`);
    if (row.capture_level === "excluded" && app.test(name)) {
      add("privacy", `${label(row)} carries ${name}`);
    }
  }
}

/** Sweep half-open boundaries; multiple stop reasons contribute one coverage unit. */
function checkCoverage(
  segment: Fact,
  spans: readonly Fact[],
  add: (rule: Rule, detail: string) => void,
): void {
  const changes = new Map<number, number[]>();
  const at = (position: number) => {
    let value = changes.get(position);
    if (!value) {
      value = [0, 0, 0, 0];
      changes.set(position, value);
    }
    return value;
  };
  at(segment.start_mono_ms!);
  at(monoEnd(segment));
  for (const row of spans) {
    const start = row.start_mono_ms!, end = monoEnd(row);
    if (start < segment.start_mono_ms! || end > monoEnd(segment)) {
      add(
        row.reason === "idle" ? "idle" : "coverage",
        `${label(row)} lies outside its segment/run`,
      );
    }
    const lane = row.kind === "capture_gap"
      ? 2
      : row.kind === "availability"
      ? row.reason === "idle" ? 3 : 1
      : 0;
    at(start)[lane]++;
    at(end)[lane]--;
  }
  const cuts = [...changes.keys()].sort((a, b) => a - b);
  const counts = [0, 0, 0, 0];
  for (let i = 0; i + 1 < cuts.length; i++) {
    const left = cuts[i], right = cuts[i + 1];
    const delta = changes.get(left)!;
    for (let lane = 0; lane < 4; lane++) counts[lane] += delta[lane];
    if (counts[3] > 1) {
      add(
        "idle",
        `segment ${segment.id} overlapping idle masks [${left}, ${right})`,
      );
    }
    if (
      left >= segment.start_mono_ms! && right <= monoEnd(segment) &&
      counts[0] + Number(counts[1] > 0) + counts[2] !== 1
    ) {
      add(
        "coverage",
        `segment ${segment.id} [${left}, ${right}): facts=${counts[0]}, stops=${
          counts[1]
        }, gaps=${counts[2]}`,
      );
    }
  }
}

function checkClock(
  segment: Fact,
  spans: readonly Fact[],
  add: (rule: Rule, detail: string) => void,
): void {
  for (const row of spans) {
    if (
      row.start_utc_ms < segment.start_utc_ms ||
      (endpoint(row) > endpoint(segment) &&
        !(row.kind === "availability" && segment.end_reason === "resume"))
    ) add("clock", `${label(row)}: UTC range lies outside its clock segment`);
  }
  // Equal monotonic instants can span sleep wall time. Compare distinct instants only.
  const points = new Map<number, { min: number; max: number }>();
  // Sleep masks are interpolated by wall time when split at midnight, while the
  // old segment closes at its pre-sleep UTC projection. Check those masks below.
  const projected = segment.end_reason === "resume"
    ? spans.filter((row) => row.kind !== "availability")
    : spans;
  for (const row of [segment, ...projected]) {
    for (
      const [mono, utc] of [[row.start_mono_ms!, row.start_utc_ms], [
        monoEnd(row),
        endpoint(row),
      ]]
    ) {
      const point = points.get(mono);
      points.set(mono, {
        min: Math.min(point?.min ?? utc, utc),
        max: Math.max(point?.max ?? utc, utc),
      });
    }
  }
  let last = -Infinity;
  for (const [mono, point] of [...points].sort(([a], [b]) => a - b)) {
    if (point.min < last) {
      add(
        "clock",
        `segment ${segment.id}: UTC moves backwards at mono ${mono}`,
      );
    }
    last = point.max;
  }
  const facts = spans.filter(isInterval).sort((a, b) =>
    Number(a.sequence) - Number(b.sequence)
  );
  for (let i = 1; i < facts.length; i++) {
    const previous = facts[i - 1], current = facts[i];
    if (
      previous.sequence === current.sequence ||
      current.start_mono_ms! < monoEnd(previous) ||
      current.start_utc_ms < endpoint(previous)
    ) add("clock", `${label(current)} is out of sequence/UTC order`);
  }
  const idle = spans.filter((row) => row.reason === "idle").sort((a, b) =>
    a.start_utc_ms - b.start_utc_ms
  );
  for (let i = 1; i < idle.length; i++) {
    if (idle[i].start_utc_ms < endpoint(idle[i - 1])) {
      add("idle", `segment ${segment.id}: overlapping idle UTC ranges`);
    }
  }
  for (
    const reason of ["idle", "locked", "disconnected", "suspended", "paused"]
  ) {
    const masks = spans.filter((row) =>
      row.kind === "availability" && row.reason === reason
    )
      .sort((a, b) =>
        a.start_mono_ms! - b.start_mono_ms! ||
        a.start_utc_ms - b.start_utc_ms || a.id - b.id
      );
    for (let i = 1; i < masks.length; i++) {
      if (masks[i].start_utc_ms < endpoint(masks[i - 1])) {
        add(
          "clock",
          `${label(masks[i])}: availability UTC order reversed/overlapping`,
        );
      }
    }
  }
}

/** Before is a frozen pre-crash dump: closed rows no longer store confirmations. */
function checkRecovery(
  rows: readonly Fact[],
  before: readonly Fact[],
  add: (rule: Rule, detail: string) => void,
): void {
  const recovered = rows.filter((row) => row.end_reason === "crash_recovered");
  if (!recovered.length && !before.length) return;
  const consumed = new Set<string>();
  for (const original of before.filter((row) => row.end_utc_ms === null)) {
    const oldRun = rows.find((row) =>
      row.kind === "run" && row.id === original.run_id
    );
    if (oldRun?.end_reason !== "crash_recovered") {
      add("recovery", `${label(original)}: old run was not crash recovered`);
      continue;
    }
    let current = rows.find((row) => key(row) === key(original));
    if (
      current?.run_id !== original.run_id ||
      current?.segment_id !== original.segment_id ||
      current?.start_utc_ms !== original.start_utc_ms ||
      current?.start_mono_ms !== original.start_mono_ms
    ) {
      add(
        "recovery",
        `${label(original)}: recovered row identity/start changed`,
      );
      continue;
    }
    const visited = new Set<string>();
    while (
      current && current.end_reason === "midnight_split" &&
      !visited.has(key(current))
    ) {
      visited.add(key(current));
      const piece: Fact = current;
      const next: Fact[] = rows.filter((row) =>
        row.kind === piece.kind && row.run_id === piece.run_id &&
        row.segment_id === piece.segment_id &&
        row.start_utc_ms === endpoint(piece) &&
        row.start_mono_ms === monoEnd(piece) && (isInterval(row)
          ? row.session_key === original.session_key
          : row.reason === original.reason)
      );
      current = next.length === 1 ? next[0] : undefined;
    }
    if (
      !current || current.end_reason !== "crash_recovered" ||
      endpoint(current) !== original.confirmed_utc_ms ||
      (original.start_mono_ms !== null &&
        monoEnd(current) !== original.confirmed_mono_ms)
    ) {
      add(
        "recovery",
        `${
          label(original)
        }: recovered endpoint differs from pre-crash confirmation`,
      );
    } else consumed.add(key(current));
  }
  for (const row of recovered) {
    if (!consumed.has(key(row))) {
      add(
        "recovery",
        `${
          label(row)
        }: missing matching pre-crash confirmation; supply --before`,
      );
    }
    if (row.kind !== "run") {
      const parent = rows.find((other) =>
        other.kind === "run" && other.id === row.run_id
      );
      const segment = rows.find((other) =>
        other.kind === "clock_segment" && other.id === row.segment_id
      );
      if (
        parent?.end_reason !== "crash_recovered" ||
        segment?.end_reason !== "crash_recovered"
      ) {
        add(
          "recovery",
          `${label(row)}: run and clock segment must also be crash_recovered`,
        );
      }
    } else if (
      !rows.some((other) =>
        other.kind === "clock_segment" && other.run_id === row.id &&
        other.end_reason === "crash_recovered"
      )
    ) add("recovery", `${label(row)}: missing recovered clock segment`);
  }
}

export function checkFacts(
  input: readonly Fact[],
  baseline: readonly Fact[] = [],
): Finding[] {
  const findings: Finding[] = [];
  const add = (rule: Rule, detail: string) => findings.push({ rule, detail });
  let rows: Fact[], before: Fact[];
  try {
    rows = uniqueRows(input);
    before = uniqueRows(baseline);
  } catch (error) {
    return [{ rule: "format", detail: String(error) }];
  }
  const runs = rows.filter((row) => row.kind === "run").sort((a, b) =>
    a.id - b.id
  );
  if (!runs.length) {
    add("coverage", "no capture runs; provide complete dump dates");
  }
  const segments = rows.filter((row) => row.kind === "clock_segment");
  const spans = rows.filter(isSpan);
  const runMap = new Map(runs.map((row) => [row.id, row]));
  const segmentMap = new Map(segments.map((row) => [row.id, row]));
  for (const row of rows) {
    checkPrivacy(row, add);
    // A run can end before its UTC start after a clock rollback. It has no mono columns.
    if (
      row.kind !== "run" &&
      (endpoint(row) < row.start_utc_ms || row.duration_ms! < 0 ||
        (row.start_mono_ms !== null &&
          (monoEnd(row) < row.start_mono_ms ||
            row.duration_ms !== monoEnd(row) - row.start_mono_ms)))
    ) add("coverage", `${label(row)}: negative or inconsistent duration`);
    if (row.kind !== "timed_policy" && !runMap.has(row.run_id!)) {
      add("coverage", `${label(row)}: missing run envelope`);
    }
    if (!isSpan(row)) continue;
    const segment = segmentMap.get(row.segment_id!);
    if (!segment || segment.run_id !== row.run_id) {
      add("coverage", `${label(row)}: missing or foreign segment`);
    }
    if (
      row.end_utc_ms === null &&
      (runMap.get(row.run_id!)?.end_utc_ms !== null ||
        segment?.end_utc_ms !== null)
    ) add("coverage", `${label(row)}: open row in closed run/segment`);
    // Availability is split when closed, including long locks recovered after a crash.
    if (
      !(row.kind === "availability" && row.end_utc_ms === null) &&
      endpoint(row) - row.start_utc_ms > dayMs
    ) add("midnight", `${label(row)} exceeds 24 hours`);
    if (
      row.end_utc_ms !== null && row.kind !== "capture_gap" &&
      endpoint(row) > row.start_utc_ms &&
      Math.floor(row.start_utc_ms / dayMs) !==
        Math.floor((endpoint(row) - 1) / dayMs)
    ) add("midnight", `${label(row)} crosses UTC midnight without splitting`);
    if (row.end_reason === "midnight_split") {
      if (endpoint(row) % dayMs !== 0) {
        add("midnight", `${label(row)}: split is not at UTC midnight`);
      }
      if (isInterval(row)) {
        const next = spans.filter((other) =>
          other.kind === row.kind && other.run_id === row.run_id &&
          other.start_utc_ms === endpoint(row) &&
          other.start_mono_ms === monoEnd(row) &&
          Number(other.sequence) > Number(row.sequence)
        ).sort((a, b) =>
          Number(a.sequence) - Number(b.sequence)
        )[0];
        if (!next || next.session_key !== row.session_key) {
          add(
            "midnight",
            `${
              label(row)
            }: missing continuation with shared session_key (include adjacent dates)`,
          );
        }
      }
    }
    if (row.kind === "availability" && row.reason === "idle") {
      const last = row.last_input_utc_ms,
        detected = row.detected_utc_ms,
        threshold = row.threshold_ms,
        version = row.policy_version;
      if (
        !isInteger(last) || !isInteger(detected) || !isInteger(threshold) ||
        threshold < 0 || !isInteger(version) || version <= 0 ||
        detected < last + threshold
      ) add("idle", `${label(row)}: missing or inconsistent idle evidence`);
    }
  }
  for (const runRow of runs) {
    const ordered = segments.filter((row) => row.run_id === runRow.id).sort((
      a,
      b,
    ) => a.id - b.id);
    if (!ordered.length) {
      add("coverage", `${label(runRow)}: missing clock segments`);
      continue;
    }
    if (
      ordered[0].start_utc_ms !== runRow.start_utc_ms ||
      endpoint(ordered[ordered.length - 1]) !== endpoint(runRow)
    ) {
      add(
        "coverage",
        `${
          label(runRow)
        }: segments do not cover run endpoints (include every intersecting date)`,
      );
    }
    for (let i = 0; i < ordered.length; i++) {
      const segment = ordered[i];
      if (i > 0 && ordered[i - 1].end_mono_ms !== segment.start_mono_ms) {
        add("coverage", `${label(segment)}: noncontiguous monotonic segments`);
      }
      if (
        segment.end_utc_ms === null &&
        (i !== ordered.length - 1 || runRow.end_utc_ms !== null)
      ) add("coverage", `${label(segment)}: invalid open tail`);
      const members = spans.filter((row) =>
        row.segment_id === segment.id && row.run_id === segment.run_id
      );
      if (segment.end_reason === "resume") {
        const next = ordered[i + 1];
        const limit = Math.max(
          endpoint(segment),
          next?.start_utc_ms ?? -Infinity,
        );
        if (!next || members.some((row) => endpoint(row) > limit)) {
          add("clock", `${label(segment)}: sleep range exceeds resume anchor`);
        }
      }
      checkCoverage(segment, members, add);
      checkClock(segment, members, add);
    }
    const sequence = spans.filter((row) =>
      row.run_id === runRow.id && isInterval(row)
    ).sort((a, b) => Number(a.sequence) - Number(b.sequence));
    for (let i = 1; i < sequence.length; i++) {
      if (
        sequence[i].sequence === sequence[i - 1].sequence ||
        sequence[i].segment_id! < sequence[i - 1].segment_id!
      ) add("clock", `${label(sequence[i])}: sequence crosses segment order`);
    }
  }
  checkRecovery(rows, before, add);
  return findings;
}

export function inspectDump(input: string, before = ""): Finding[] {
  try {
    return checkFacts(parseDump(input), parseDump(before));
  } catch (error) {
    return [{
      rule: "format",
      detail: error instanceof Error ? error.message : String(error),
    }];
  }
}

async function main(): Promise<void> {
  const args = [...Deno.args];
  const beforeIndex = args.indexOf("--before");
  let before = "";
  if (beforeIndex >= 0) {
    const path = args[beforeIndex + 1];
    if (!path || path.startsWith("--")) {
      throw new Error(
        "--before requires a frozen pre-crash dump path exported with quartz dump --offline",
      );
    }
    before = await Deno.readTextFile(path);
    args.splice(beforeIndex, 2);
  }
  if (args.length > 1 || args.some((arg) => arg.startsWith("--"))) {
    throw new Error(usage);
  }
  const input = args[0] && args[0] !== "-"
    ? await Deno.readTextFile(args[0])
    : await new Response(Deno.stdin.readable).text();
  const findings = inspectDump(input, before);
  for (const rule of rules) {
    const failed = findings.filter((finding) => finding.rule === rule);
    console.log(
      `${failed.length ? "FAIL" : "PASS"} ${rule}${
        failed.length ? ` (${failed.length})` : ""
      }`,
    );
    for (const finding of failed) console.log(`  ${finding.detail}`);
  }
  console.log(
    findings.length
      ? `FAIL: ${findings.length} finding(s)`
      : "PASS: all dump invariants satisfied",
  );
  if (findings.length) Deno.exit(1);
}
if (import.meta.main) await run(main);
