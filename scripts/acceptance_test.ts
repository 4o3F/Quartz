import {
  type Fact,
  inspectDump,
  parseDump,
  type Rule,
  usage,
} from "./acceptance.ts";

function row(
  kind: Fact["kind"],
  id: number,
  start = 0,
  end = 100,
  extra: Record<string, unknown> = {},
): Fact {
  return {
    kind,
    id,
    run_id: 1,
    segment_id: kind === "run" ? null : 1,
    start_utc_ms: start,
    end_utc_ms: end,
    confirmed_utc_ms: null,
    start_mono_ms: kind === "run" ? null : start,
    end_mono_ms: kind === "run" ? null : end,
    confirmed_mono_ms: null,
    duration_ms: end - start,
    end_reason: kind === "run"
      ? "shutdown"
      : kind === "clock_segment"
      ? "run_end"
      : kind === "capture_gap"
      ? null
      : "shutdown",
    ...extra,
  };
}
function fixture(): Fact[] {
  return [
    row("run", 1),
    row("clock_segment", 1),
    row("foreground", 1, 0, 100, {
      capture_level: "full",
      sequence: 1,
      session_key: 1,
      title: "测试\n标题",
    }),
  ];
}
const jsonl = (rows: readonly Fact[]) =>
  rows.map((value) => JSON.stringify(value)).join("\n");
function passes(rows: Fact[], before: Fact[] = []): void {
  const findings = inspectDump(jsonl(rows), jsonl(before));
  if (findings.length) throw new Error(JSON.stringify(findings));
}
function fails(rows: Fact[], rule: Rule, before: Fact[] = []): void {
  const findings = inspectDump(jsonl(rows), jsonl(before));
  if (!findings.some((finding) => finding.rule === rule)) {
    throw new Error(`Expected ${rule}: ${JSON.stringify(findings)}`);
  }
}
const open = (value: Fact): Fact => ({
  ...value,
  end_utc_ms: null,
  confirmed_utc_ms: value.end_utc_ms,
  end_mono_ms: null,
  confirmed_mono_ms: value.end_mono_ms,
  end_reason: null,
});
const idle = (id = 2, start = 20, end = 80): Fact =>
  row("availability", id, start, end, {
    reason: "idle",
    last_input_utc_ms: 0,
    detected_utc_ms: 20,
    threshold_ms: 20,
    policy_version: 1,
  });

Deno.test("complete run, escaped titles, BOM and CRLF parse", () => {
  const input = `\uFEFF${jsonl(fixture()).replaceAll("\n", "\r\n")}\r\n`;
  if (parseDump(input).length !== 3 || inspectDump(input).length) {
    throw new Error("valid JSON Lines failed");
  }
});
Deno.test("multiple runs and repeated adjacent-day envelopes", () => {
  const first = fixture();
  const second = first.map((value) => ({
    ...value,
    id: value.id + 10,
    run_id: 11,
    segment_id: value.segment_id === null ? null : 11,
  }));
  passes([...first, ...second, ...first]);
});
Deno.test("empty input fails", () => fails([], "coverage"));
for (
  const input of [
    "bad",
    "[]",
    "null",
    '{"kind":"web_interval"}',
    '{"kind":"run","id":9007199254740992}',
  ]
) {
  Deno.test(`malformed record ${input}`, () => {
    if (!inspectDump(input).some((finding) => finding.rule === "format")) {
      throw new Error("invalid record passed");
    }
  });
}
Deno.test("conflicting snapshots fail", () => {
  const rows = fixture();
  fails([...rows, { ...rows[2], title: "changed" }], "format");
});
Deno.test("open run ends at confirmation without a fabricated live tail", () =>
  passes(fixture().map(open)));
Deno.test("a hole before an open confirmation fails", () => {
  const rows = fixture().map(open);
  rows[2].confirmed_mono_ms = 99;
  rows[2].confirmed_utc_ms = 99;
  rows[2].duration_ms = 99;
  fails(rows, "coverage");
});
Deno.test("open row inside closed run fails", () => {
  const rows = fixture();
  rows[2] = open(rows[2]);
  fails(rows, "coverage");
});
Deno.test("mixed endpoint states fail", () => {
  const rows = fixture();
  rows[2].confirmed_utc_ms = 100;
  fails(rows, "format");
});
Deno.test("missing monotonic confirmation fails", () => {
  const rows = fixture().map(open);
  rows[2].confirmed_mono_ms = null;
  fails(rows, "format");
});
Deno.test("missing run fails", () => fails(fixture().slice(1), "coverage"));
Deno.test("foreign segment fails", () => {
  const rows = fixture();
  rows[2].segment_id = 4;
  fails(rows, "coverage");
});
Deno.test("coverage hole fails", () => {
  const rows = fixture();
  rows[2] = row("foreground", 1, 0, 99, rows[2]);
  rows[2].end_utc_ms = 99;
  rows[2].end_mono_ms = 99;
  rows[2].duration_ms = 99;
  fails(rows, "coverage");
});
Deno.test("overlapping foreground fails", () => {
  const rows = fixture();
  fails([...rows, { ...rows[2], id: 2, sequence: 2 }], "coverage");
});
Deno.test("negative UTC interval fails", () => {
  const rows = fixture();
  rows[2].end_utc_ms = -1;
  fails(rows, "coverage");
});
Deno.test("negative monotonic interval fails", () => {
  const rows = fixture();
  rows[2].end_mono_ms = -1;
  rows[2].duration_ms = -1;
  fails(rows, "coverage");
});
Deno.test("stored duration mismatch fails", () => {
  const rows = fixture();
  rows[2].duration_ms = 101;
  fails(rows, "coverage");
});
Deno.test("span outside run fails", () => {
  const rows = fixture();
  rows[2].end_mono_ms = 101;
  rows[2].end_utc_ms = 101;
  rows[2].duration_ms = 101;
  fails(rows, "coverage");
});
Deno.test("stop reasons form a union and idle remains independent", () => {
  const rows = fixture().slice(0, 2);
  rows.push(
    row("availability", 1, 0, 100, { reason: "locked" }),
    row("availability", 2, 10, 90, { reason: "paused" }),
    idle(3),
  );
  passes(rows);
});
Deno.test("no_foreground and gap cover run in order", () => {
  const rows = fixture().slice(0, 2);
  rows.push(
    row("no_foreground", 1, 0, 50, {
      sequence: 1,
      session_key: 1,
      capture_level: "full",
      no_foreground_reason: "unknown_desktop",
    }),
    row("capture_gap", 1, 50, 100, { reason: "event_overload" }),
  );
  passes(rows);
});
for (const reason of ["locked", "disconnected", "suspended", "paused"]) {
  Deno.test(`foreground overlapping ${reason} fails`, () =>
    fails(
      [...fixture(), row("availability", 2, 0, 100, { reason })],
      "coverage",
    ));
}
Deno.test("gap overlapping facts fails", () =>
  fails([
    ...fixture(),
    row("capture_gap", 1, 0, 50, { reason: "database_error" }),
  ], "coverage"));
Deno.test("idle alone cannot cover a run", () =>
  fails([...fixture().slice(0, 2), idle(1, 20, 100)], "coverage"));
Deno.test("valid independent idle evidence", () =>
  passes([...fixture(), idle()]));
Deno.test("overlapping idle masks fail", () =>
  fails([...fixture(), idle(), idle(3, 30, 90)], "idle"));
Deno.test("idle outside run fails", () =>
  fails([...fixture(), idle(2, 20, 110)], "idle"));
for (
  const field of [
    "last_input_utc_ms",
    "detected_utc_ms",
    "threshold_ms",
    "policy_version",
  ]
) {
  Deno.test(`idle missing ${field} fails`, () => {
    const mask = idle();
    delete mask[field];
    fails([...fixture(), mask], "idle");
  });
}
Deno.test("idle threshold evidence must precede onset", () =>
  fails([...fixture(), { ...idle(), threshold_ms: 21 }], "idle"));
function midnight(): Fact[] {
  const day = 86_400_000;
  return [
    row("run", 1, day - 10, day + 10),
    row("clock_segment", 1, day - 10, day + 10),
    row("foreground", 1, day - 10, day, {
      sequence: 1,
      session_key: 7,
      capture_level: "full",
      end_reason: "midnight_split",
    }),
    row("foreground", 2, day, day + 10, {
      sequence: 2,
      session_key: 7,
      capture_level: "full",
    }),
  ];
}
Deno.test("midnight foreground pieces retain session key", () =>
  passes(midnight()));
Deno.test("midnight session mismatch fails", () => {
  const rows = midnight();
  rows[3].session_key = 8;
  fails(rows, "midnight");
});
Deno.test("missing next midnight piece fails", () =>
  fails(midnight().slice(0, 3), "midnight"));
Deno.test("midnight_split away from midnight fails", () => {
  const rows = fixture();
  rows[2].end_reason = "midnight_split";
  fails(rows, "midnight");
});
Deno.test("fact longer than 24 hours fails; run envelopes may be longer", () => {
  const rows = fixture().map((value) => ({
    ...value,
    end_utc_ms: 86_400_001,
    end_mono_ms: value.kind === "run" ? null : 86_400_001,
    duration_ms: 86_400_001,
  }));
  fails(rows, "midnight");
});
Deno.test("open long availability is split only on close", () => {
  const end = 2 * 86_400_000;
  const rows = [
    row("run", 1, 0, end),
    row("clock_segment", 1, 0, end),
    row("availability", 1, 0, end, { reason: "locked" }),
  ].map(open);
  passes(rows);
});
Deno.test("hidden title fields fail including empty strings", () => {
  const rows = fixture();
  rows[2].capture_level = "title_hidden";
  rows[2].title = "";
  fails(rows, "privacy");
});
Deno.test("hidden title id fails", () => {
  const rows = fixture();
  rows[2].capture_level = "title_hidden";
  delete rows[2].title;
  rows[2].title_id = 1;
  fails(rows, "privacy");
});
for (
  const field of [
    "app_id",
    "app_display_name",
    "exe",
    "window_instance_id",
    "hwnd",
  ]
) {
  Deno.test(`excluded ${field} fails`, () => {
    const rows = fixture();
    rows[2].capture_level = "excluded";
    delete rows[2].title;
    rows[2][field] = "private";
    fails(rows, "privacy");
  });
}
for (const field of ["app_id", "title", "domain", "domain_id"]) {
  Deno.test(`gap ${field} fails`, () =>
    fails([
      ...fixture().slice(0, 2),
      row("capture_gap", 1, 0, 100, {
        reason: "write_queue_overload",
        [field]: "secret",
      }),
    ], "privacy"));
}
Deno.test("null redacted fields are permitted", () => {
  const rows = fixture();
  rows[2].capture_level = "excluded";
  rows[2].title = null;
  rows[2].app_id = null;
  passes(rows);
});
function rollback(): Fact[] {
  const rows = fixture();
  rows[0].end_utc_ms = -800;
  rows[0].duration_ms = -800;
  rows[1].end_reason = "time_change";
  rows[2].end_reason = "clock_jump";
  rows.push(
    row("clock_segment", 2, -900, -800, {
      segment_id: 2,
      start_mono_ms: 100,
      end_mono_ms: 200,
      duration_ms: 100,
    }),
    row("foreground", 2, -900, -800, {
      segment_id: 2,
      start_mono_ms: 100,
      end_mono_ms: 200,
      duration_ms: 100,
      sequence: 2,
      session_key: 2,
      capture_level: "full",
    }),
  );
  return rows;
}
Deno.test("UTC rollback across segments and negative UTC run extent are valid", () =>
  passes(rollback()));
Deno.test("UTC rollback within one segment fails", () => {
  const rows = fixture();
  rows[2].start_utc_ms = -10;
  fails(rows, "clock");
});
Deno.test("noncontiguous clock segments fail", () => {
  const rows = rollback();
  rows[3].start_mono_ms = 101;
  rows[3].duration_ms = 99;
  fails(rows, "coverage");
});
Deno.test("foreground sequence must preserve segment order", () => {
  const rows = rollback();
  rows[4].sequence = 0;
  fails(rows, "clock");
});
Deno.test("duplicate sequence fails", () => {
  const rows = midnight();
  rows[3].sequence = 1;
  fails(rows, "clock");
});
Deno.test("sleep wall time may advance at a fixed monotonic position", () => {
  const rows = fixture().slice(0, 2);
  rows[0].end_utc_ms = 1000;
  rows[0].duration_ms = 1000;
  rows[1].end_reason = "resume";
  rows.push(
    row("availability", 1, 0, 900, {
      reason: "suspended",
      end_reason: "clock_jump",
      end_mono_ms: 100,
      duration_ms: 100,
    }),
    row("clock_segment", 2, 900, 1000, {
      segment_id: 2,
      start_mono_ms: 100,
      end_mono_ms: 200,
      duration_ms: 100,
    }),
    row("foreground", 1, 900, 1000, {
      segment_id: 2,
      sequence: 1,
      session_key: 1,
      capture_level: "full",
      start_mono_ms: 100,
      end_mono_ms: 200,
      duration_ms: 100,
    }),
  );
  passes(rows);
});
const recovered = (rows: Fact[]) =>
  rows.map((value) => ({ ...value, end_reason: "crash_recovered" }));
Deno.test("crash endpoints match frozen confirmations for run, segment, foreground and idle", () => {
  const rows = [...fixture(), idle()];
  passes(recovered(rows), rows.map(open));
});
Deno.test("crash without baseline fails", () =>
  fails(recovered(fixture()), "recovery"));
for (const index of [0, 1, 2]) {
  Deno.test(`crash confirmation mismatch at row ${index} fails`, () => {
    const rows = fixture();
    const before = rows.map(open);
    before[index].confirmed_utc_ms = 99;
    fails(recovered(rows), "recovery", before);
  });
}
Deno.test("crash mono confirmation mismatch fails", () => {
  const rows = fixture();
  const before = rows.map(open);
  before[2].confirmed_mono_ms = 99;
  fails(recovered(rows), "recovery", before);
});
Deno.test("crash parent reason must match", () => {
  const rows = recovered(fixture());
  rows[1].end_reason = "run_end";
  fails(rows, "recovery", fixture().map(open));
});
Deno.test("old open idle must also be recovered", () =>
  fails(recovered(fixture()), "recovery", [...fixture(), idle()].map(open)));
Deno.test("crash midnight continuation can have a new row id", () => {
  const rows = midnight();
  rows[0].end_reason = "crash_recovered";
  rows[1].end_reason = "crash_recovered";
  rows[3].end_reason = "crash_recovered";
  const before = [
    open(rows[0]),
    open(rows[1]),
    open({
      ...rows[2],
      end_utc_ms: rows[3].end_utc_ms,
      end_mono_ms: rows[3].end_mono_ms,
      duration_ms: 20,
    }),
  ];
  passes(rows, before);
});
Deno.test("short closed row crossing UTC midnight must split", () => {
  const rows = midnight();
  rows[2].end_utc_ms = rows[3].end_utc_ms;
  rows[2].end_mono_ms = rows[3].end_mono_ms;
  rows[2].duration_ms = 20;
  rows[2].end_reason = "shutdown";
  fails(rows.slice(0, 3), "midnight");
});
Deno.test("zero duration facts do not add coverage", () => {
  const rows = fixture();
  rows.push(
    row("foreground", 2, 100, 100, {
      sequence: 2,
      session_key: 2,
      capture_level: "full",
    }),
  );
  passes(rows);
});
Deno.test("timed policies do not contribute coverage", () => {
  passes([
    ...fixture(),
    row("timed_policy", 1, 0, 1000, {
      run_id: null,
      segment_id: null,
      start_mono_ms: null,
      end_mono_ms: null,
      end_reason: null,
      policy_kind: "pause",
    }),
  ]);
});
Deno.test("multi-midnight sleep masks use wall time while old segment uses its anchor", () => {
  const day = 86_400_000;
  const start = day - 100;
  const resume = 2 * day + 100;
  const rows = [
    row("run", 1, start, resume + 10),
    row("clock_segment", 1, start, start + 20, {
      start_mono_ms: 0,
      end_mono_ms: 20,
      duration_ms: 20,
      end_reason: "resume",
    }),
    row("foreground", 1, start, start + 10, {
      start_mono_ms: 0,
      end_mono_ms: 10,
      duration_ms: 10,
      sequence: 1,
      session_key: 1,
      capture_level: "full",
      end_reason: "suspended",
    }),
    row("availability", 1, start + 10, day, {
      start_mono_ms: 10,
      end_mono_ms: 10,
      duration_ms: 0,
      reason: "suspended",
      end_reason: "midnight_split",
    }),
    row("availability", 2, day, 2 * day, {
      start_mono_ms: 10,
      end_mono_ms: 19,
      duration_ms: 9,
      reason: "suspended",
      end_reason: "midnight_split",
    }),
    row("availability", 3, 2 * day, resume, {
      start_mono_ms: 19,
      end_mono_ms: 20,
      duration_ms: 1,
      reason: "suspended",
      end_reason: "clock_jump",
    }),
    row("clock_segment", 2, resume, resume + 10, {
      segment_id: 2,
      start_mono_ms: 20,
      end_mono_ms: 30,
      duration_ms: 10,
    }),
    row("foreground", 2, resume, resume + 10, {
      segment_id: 2,
      start_mono_ms: 20,
      end_mono_ms: 30,
      duration_ms: 10,
      sequence: 2,
      session_key: 2,
      capture_level: "full",
    }),
  ];
  passes(rows);
});
Deno.test("long locked crash recovery splits availability and preserves final confirmation", () => {
  const day = 86_400_000;
  const rows = [
    row("run", 1, 0, 2 * day + 10, { end_reason: "crash_recovered" }),
    row("clock_segment", 1, 0, 2 * day + 10, { end_reason: "crash_recovered" }),
    row("availability", 1, 0, day, {
      reason: "locked",
      end_reason: "midnight_split",
    }),
    row("availability", 2, day, 2 * day, {
      reason: "locked",
      end_reason: "midnight_split",
    }),
    row("availability", 3, 2 * day, 2 * day + 10, {
      reason: "locked",
      end_reason: "crash_recovered",
    }),
  ];
  const before = [
    open(rows[0]),
    open(rows[1]),
    open(row("availability", 1, 0, 2 * day + 10, { reason: "locked" })),
  ];
  passes(rows, before);
});
Deno.test("crash recovery cannot change the original start", () => {
  const rows = recovered(fixture());
  const before = fixture().map(open);
  before[2].start_utc_ms = -1;
  fails(rows, "recovery", before);
});
Deno.test("no_foreground midnight pieces also retain session key", () => {
  const rows = midnight().map((value): Fact =>
    value.kind === "foreground"
      ? { ...value, kind: "no_foreground", no_foreground_reason: "empty" }
      : value
  );
  passes(rows);
});
Deno.test("idle evidence survives reopening after UTC rollback", () => {
  const rows = rollback();
  rows.push(idle(3, 20, 100));
  rows[5].end_reason = "clock_jump";
  rows.push(
    row("availability", 4, -900, -850, {
      segment_id: 2,
      start_mono_ms: 100,
      end_mono_ms: 150,
      duration_ms: 50,
      reason: "idle",
      last_input_utc_ms: 0,
      detected_utc_ms: 20,
      threshold_ms: 20,
      policy_version: 1,
    }),
  );
  passes(rows);
});
Deno.test("midnight continuation may be zero length before a policy boundary", () => {
  const day = 86_400_000;
  const rows = midnight();
  rows[3] = {
    ...rows[3],
    end_utc_ms: day,
    end_mono_ms: day,
    duration_ms: 0,
    end_reason: "policy_changed",
  };
  rows.push(
    row("foreground", 3, day, day + 10, {
      sequence: 3,
      session_key: 8,
      capture_level: "full",
    }),
  );
  passes(rows);
});
Deno.test("sleep resume can also open a backwards UTC segment", () => {
  const rows = [
    row("run", 1, 0, 0),
    row("clock_segment", 1, 0, 100, { end_reason: "resume" }),
    row("availability", 1, 0, 100, {
      reason: "suspended",
      end_reason: "clock_jump",
    }),
    row("clock_segment", 2, -100, 0, {
      segment_id: 2,
      start_mono_ms: 100,
      end_mono_ms: 200,
      duration_ms: 100,
    }),
    row("foreground", 1, -100, 0, {
      segment_id: 2,
      sequence: 1,
      session_key: 1,
      capture_level: "full",
      start_mono_ms: 100,
      end_mono_ms: 200,
      duration_ms: 100,
    }),
  ];
  passes(rows);
});

Deno.test("export guidance requires explicit offline for frozen and stopped snapshots", () => {
  const findings = inspectDump(
    jsonl([...fixture(), { ...fixture()[2], title: "changed" }]),
  );
  if (
    !findings.some((finding) =>
      finding.detail.includes("quartz dump --offline")
    )
  ) {
    throw new Error("conflicting snapshots must explain offline export");
  }
  if (
    !usage.includes("quartz dump --date YYYY-MM-DD --format json") ||
    !usage.includes("quartz dump --offline") ||
    !usage.includes("frozen pre-crash")
  ) {
    throw new Error("usage must distinguish running and frozen exports");
  }
});
