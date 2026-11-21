import { type Fact, parseDump, uniqueRows } from "./acceptance.ts";
import { run } from "./common.ts";

interface Range {
  start: number;
  end: number;
}
const minuteMs = 60_000;
const minutesPerHour = 60;

/** Independent no-annotation active-time check against query/pipeline.rs. */
export class ActiveTimeSummary {
  constructor(private rows: readonly Fact[], private range: Range) {
    if (
      !Number.isSafeInteger(range.start) || !Number.isSafeInteger(range.end) ||
      range.end <= range.start
    ) throw new Error("Invalid summary range");
  }

  private endpoint(row: Fact): number {
    const end = row.end_utc_ms ?? row.confirmed_utc_ms;
    if (end === null || end < row.start_utc_ms) {
      throw new Error(`Invalid UTC span: ${row.kind} ${row.id}`);
    }
    return end;
  }

  private duration(row: Fact, slice: Range): number {
    const end = this.endpoint(row);
    const monoEnd = row.end_mono_ms ?? row.confirmed_mono_ms;
    if (
      row.start_mono_ms === null || monoEnd === null ||
      monoEnd < row.start_mono_ms || end <= row.start_utc_ms
    ) throw new Error(`Invalid monotonic span: ${row.kind} ${row.id}`);
    // Match SpanDuration::within: integer endpoint interpolation, then subtraction.
    const duration = BigInt(monoEnd) - BigInt(row.start_mono_ms);
    const width = BigInt(end) - BigInt(row.start_utc_ms);
    const at = (utc: number) =>
      (BigInt(utc) - BigInt(row.start_utc_ms)) * duration / width;
    return Number(at(slice.end) - at(slice.start));
  }

  activeMs(): number {
    const rows = uniqueRows(this.rows);
    const clocks = rows.filter((row) => row.kind === "clock_segment");
    if (!clocks.length) throw new Error("Dump has no clock segments");
    for (const row of rows) {
      if (
        ["foreground", "no_foreground", "availability", "capture_gap"].includes(
          row.kind,
        ) &&
        !clocks.some((clock) =>
          clock.id === row.segment_id && clock.run_id === row.run_id
        )
      ) throw new Error(`Missing clock segment: ${row.kind} ${row.id}`);
    }
    let total = 0;
    for (const clock of clocks) {
      const start = Math.max(this.range.start, clock.start_utc_ms);
      const end = Math.min(this.range.end, this.endpoint(clock));
      if (end <= start) continue;
      const members = rows.filter((row) =>
        row.run_id === clock.run_id && row.segment_id === clock.id &&
        ["foreground", "no_foreground", "availability", "capture_gap"].includes(
          row.kind,
        ) && row.start_utc_ms < end && this.endpoint(row) > start
      );
      const boundaries = new Set([start, end]);
      for (const row of members) {
        boundaries.add(Math.max(start, row.start_utc_ms));
        boundaries.add(Math.min(end, this.endpoint(row)));
      }
      const ordered = [...boundaries].sort((left, right) => left - right);
      for (let index = 1; index < ordered.length; index++) {
        const slice = { start: ordered[index - 1], end: ordered[index] };
        const covering = members.filter((row) =>
          row.start_utc_ms <= slice.start && this.endpoint(row) >= slice.end
        );
        if (
          covering.some((row) =>
            row.kind === "availability" || row.kind === "capture_gap"
          )
        ) continue;
        // The pipeline selects the latest fact ID when foreground rows overlap.
        const foreground = covering.filter((row) =>
          row.kind === "foreground" || row.kind === "no_foreground"
        ).sort((left, right) =>
          right.id - left.id
        )[0];
        if (
          foreground?.kind === "foreground" &&
          ["full", "title_hidden"].includes(String(foreground.capture_level))
        ) total += this.duration(foreground, slice);
      }
    }
    if (!Number.isSafeInteger(total)) {
      throw new Error("Active time is too large");
    }
    return total;
  }

  text(): string {
    const activeMs = this.activeMs();
    const minutes = Math.floor(activeMs / minuteMs);
    return `active_ms=${activeMs}; 今日 ${
      Math.floor(minutes / minutesPerHour)
    }h ${minutes % minutesPerHour}m (floor total milliseconds to minutes)`;
  }

  static localDay(date: string): Range {
    if (!/^\d{4}-\d{2}-\d{2}$/.test(date)) {
      throw new Error("Date must use YYYY-MM-DD");
    }
    const start = new Date(`${date}T00:00:00`);
    const canonical = `${start.getFullYear()}-${
      String(start.getMonth() + 1).padStart(2, "0")
    }-${String(start.getDate()).padStart(2, "0")}`;
    if (!Number.isFinite(start.getTime()) || canonical !== date) {
      throw new Error("Invalid local date");
    }
    const end = new Date(start);
    end.setDate(end.getDate() + 1);
    return { start: start.getTime(), end: end.getTime() };
  }
}

async function main(): Promise<void> {
  const [path, dateFlag, date, annotationFlag, ...extra] = Deno.args;
  if (
    !path || dateFlag !== "--date" || !date ||
    annotationFlag !== "--no-annotations" || extra.length
  ) {
    throw new Error(
      "Usage: deno task acceptance-summary dump.jsonl --date YYYY-MM-DD --no-annotations\nRequires a fresh acceptance day with no annotations and day.start_offset_min=0; dump does not contain annotations.",
    );
  }
  const summary = new ActiveTimeSummary(
    parseDump(await Deno.readTextFile(path)),
    ActiveTimeSummary.localDay(date),
  );
  console.log(summary.text());
}
if (import.meta.main) await run(main);
