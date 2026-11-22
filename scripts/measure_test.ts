import {
  firstFrameSamples,
  percentile,
  scenarioSequence,
  segmentSummary,
} from "./measure.ts";
function equal(actual: unknown, expected: unknown) {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    throw new Error(`${JSON.stringify(actual)} != ${JSON.stringify(expected)}`);
  }
}
Deno.test("first frame parser accepts tracing styles and ignores partial or unrelated lines", () => {
  equal(
    firstFrameSamples(
      'UI first frame acknowledged click_to_first_frame_ms=106\n\x1b[32mUI first frame acknowledged\x1b[0m click_to_first_frame_ms="81"\nother click_to_first_frame_ms=8\nUI first frame acknowledged click_to_first_frame_ms=-2\nUI first frame acknowledged click_to_first_frame_ms=17junk\nUI first frame acknowledged click_to_first_frame_ms=9',
    ),
    [106, 81],
  );
});
Deno.test("nearest-rank percentiles cover odd, even and tail samples without mutating input", () => {
  const values = [5, 1, 3, 2];
  equal(percentile(values, 0.5), 2);
  equal(percentile(values, 0.95), 5);
  equal(percentile([7], 0.95), 7);
  equal(percentile([1, 4, 9], 0.5), 4);
  equal(values, [5, 1, 3, 2]);
  for (const fraction of [0, -1, 1.1, NaN]) {
    let rejected = false;
    try {
      percentile([1], fraction);
    } catch {
      rejected = true;
    }
    if (!rejected) throw new Error("invalid percentile accepted");
  }
  let rejected = false;
  try {
    percentile([], 0.5);
  } catch {
    rejected = true;
  }
  if (!rejected) throw new Error("empty sample accepted");
});
Deno.test("resident scenarios sample Hidden before and after opening; on-demand has no Hidden", () => {
  equal(scenarioSequence("resident"), [
    "core-only",
    "hidden-never-opened",
    "hidden-after-close",
    "visible-static",
  ]);
  equal(scenarioSequence("on-demand"), ["core-only", "visible-static"]);
  equal(scenarioSequence("core-only"), ["core-only"]);
});
Deno.test("segment summary reports memory, threads and handles median and maximum", () => {
  const summary = segmentSummary(
    [1, 3, 2].map((value) => ({
      Scenario: "test",
      PrivateBytes: value * 100,
      PrivateWS: value * 50,
      Threads: value,
      Handles: value * 10,
      CpuMs: 0,
      IdleMs: null,
    })),
  );
  equal(summary.PrivateBytes, { median: 200, max: 300 });
  equal(summary.Handles, { median: 20, max: 30 });
});
