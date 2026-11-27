import {
  buildBrowserState,
  capTitle,
  extractDomain,
  INTERNAL_DOMAIN,
  LOCAL_FILE_DOMAIN,
  MAX_TITLE_UTF16_UNITS,
  statesEqual,
} from "./state.js";

function assertEqual(actual, expected, label) {
  if (actual !== expected) {
    const exp = JSON.stringify(expected);
    const got = JSON.stringify(actual);
    throw new Error(`${label}: expected ${exp}, got ${got}`);
  }
}

function assertDeepEqual(actual, expected, label) {
  const actualJson = JSON.stringify(actual);
  const expectedJson = JSON.stringify(expected);
  if (actualJson !== expectedJson) {
    throw new Error(`${label}: expected ${expectedJson}, got ${actualJson}`);
  }
}

Deno.test("extractDomain normalizes http(s), ports, IDN, internal, file and invalid URLs", () => {
  const cases = [
    ["https://github.com/4o3F/quartz?tab=readme#top", "github.com"],
    ["https://GitHub.COM/4o3F/quartz", "github.com"],
    ["http://localhost:3000/dashboard", "localhost"],
    ["http://127.0.0.1:8080/index.html", "127.0.0.1"],
    ["https://example.com:8443/path", "example.com"],
    ["https://github.com./path", "github.com"],
    ["https://münchen.de/path", "xn--mnchen-3ya.de"],
    ["https://例子.测试/路径", "xn--fsqu00a.xn--0zwm56d"],
    ["chrome://settings/extensions", INTERNAL_DOMAIN],
    ["about:blank", INTERNAL_DOMAIN],
    ["about:debugging#/runtime/this-firefox", INTERNAL_DOMAIN],
    ["edge://settings/", INTERNAL_DOMAIN],
    ["moz-extension://1234-5678/popup.html", INTERNAL_DOMAIN],
    [
      "chrome-extension://hmhnlfjafbcngnjgafhphcmljgldbakm/x.html",
      INTERNAL_DOMAIN,
    ],
    ["file:///C:/Users/me/notes.html", LOCAL_FILE_DOMAIN],
    ["file:///tmp/index.html", LOCAL_FILE_DOMAIN],
    ["not a url", INTERNAL_DOMAIN],
    ["", INTERNAL_DOMAIN],
    ["http://[::1]:8080/", INTERNAL_DOMAIN],
    [undefined, INTERNAL_DOMAIN],
    [null, INTERNAL_DOMAIN],
  ];
  for (const [input, expected] of cases) {
    assertEqual(extractDomain(input), expected, `extractDomain(${input})`);
  }
});

Deno.test("capTitle handles empty, exact 4096 UTF-16 boundary, CJK and emoji surrogate pairs", () => {
  assertDeepEqual(
    capTitle(""),
    { title: "", title_status: "empty" },
    "empty string",
  );
  assertDeepEqual(
    capTitle(undefined),
    { title: "", title_status: "empty" },
    "undefined title",
  );
  assertDeepEqual(
    capTitle("4o3F/quartz"),
    { title: "4o3F/quartz", title_status: "ok" },
    "short title",
  );

  const exactAscii = "a".repeat(MAX_TITLE_UTF16_UNITS);
  assertDeepEqual(
    capTitle(exactAscii),
    { title: exactAscii, title_status: "ok" },
    "exact 4096 ascii",
  );

  const exactCjk = "中".repeat(MAX_TITLE_UTF16_UNITS);
  assertDeepEqual(
    capTitle(exactCjk),
    { title: exactCjk, title_status: "ok" },
    "exact 4096 CJK",
  );

  const exactEmoji = "😀".repeat(MAX_TITLE_UTF16_UNITS / 2);
  assertDeepEqual(
    capTitle(exactEmoji),
    { title: exactEmoji, title_status: "ok" },
    "exact 4096 emoji",
  );

  const overCjk = "中".repeat(MAX_TITLE_UTF16_UNITS + 1);
  const cappedCjk = capTitle(overCjk);
  assertEqual(cappedCjk.title_status, "truncated", "over CJK status");
  assertEqual(cappedCjk.title.length, MAX_TITLE_UTF16_UNITS, "over CJK length");

  const splitEmoji = "a".repeat(MAX_TITLE_UTF16_UNITS - 1) + "😀";
  const cappedSplit = capTitle(splitEmoji);
  assertEqual(cappedSplit.title_status, "truncated", "split emoji status");
  assertEqual(
    cappedSplit.title.length,
    MAX_TITLE_UTF16_UNITS - 1,
    "split emoji drops trailing high surrogate",
  );
  const lastCode = cappedSplit.title.charCodeAt(cappedSplit.title.length - 1);
  assertEqual(
    lastCode >= 0xd800 && lastCode <= 0xdbff,
    false,
    "no trailing unpaired high surrogate",
  );

  const cjkThenEmoji = "中".repeat(MAX_TITLE_UTF16_UNITS - 1) + "😀";
  const cappedCjkEmoji = capTitle(cjkThenEmoji);
  assertEqual(cappedCjkEmoji.title_status, "truncated", "CJK+emoji status");
  assertEqual(
    cappedCjkEmoji.title.length,
    MAX_TITLE_UTF16_UNITS - 1,
    "CJK+emoji does not split surrogate pair",
  );

  const overEmoji = "😀".repeat(MAX_TITLE_UTF16_UNITS / 2 + 1);
  const cappedEmoji = capTitle(overEmoji);
  assertEqual(cappedEmoji.title_status, "truncated", "over emoji status");
  assertEqual(
    cappedEmoji.title.length,
    MAX_TITLE_UTF16_UNITS,
    "over emoji length",
  );
});

Deno.test("buildBrowserState matches focused, unfocused, minimized, internal and local-file shapes", () => {
  assertDeepEqual(
    buildBrowserState(
      { id: 7, focused: true, state: "normal" },
      {
        id: 91,
        url: "https://github.com/4o3F/quartz",
        title: "4o3F/quartz",
        audible: false,
      },
    ),
    {
      focused: true,
      window: { id: 7, state: "normal" },
      tab: {
        id: 91,
        domain: "github.com",
        title: "4o3F/quartz",
        title_status: "ok",
        audible: false,
      },
    },
    "state_focused",
  );

  assertDeepEqual(
    buildBrowserState(null, null),
    { focused: false, window: null, tab: null },
    "state_unfocused null",
  );
  assertDeepEqual(
    buildBrowserState({ id: -1, focused: false, state: "normal" }, null),
    { focused: false, window: null, tab: null },
    "state_unfocused WINDOW_ID_NONE",
  );
  assertDeepEqual(
    buildBrowserState({ id: 7, focused: false, state: "normal" }, null),
    { focused: false, window: null, tab: null },
    "state_unfocused unfocused window",
  );

  assertDeepEqual(
    buildBrowserState({ id: 7, focused: false, state: "minimized" }, null),
    { focused: false, window: { id: 7, state: "minimized" }, tab: null },
    "state_minimized",
  );
  assertDeepEqual(
    buildBrowserState(
      { id: 7, focused: true, state: "minimized" },
      { id: 91, url: "https://github.com", title: "x", audible: false },
    ),
    { focused: false, window: { id: 7, state: "minimized" }, tab: null },
    "state_minimized ignores tab",
  );

  assertDeepEqual(
    buildBrowserState(
      { id: 8, focused: true, state: "maximized" },
      { id: 92, url: "chrome://newtab/", title: "", audible: false },
    ),
    {
      focused: true,
      window: { id: 8, state: "maximized" },
      tab: {
        id: 92,
        domain: "(internal)",
        title: "",
        title_status: "empty",
        audible: false,
      },
    },
    "state_internal",
  );

  assertDeepEqual(
    buildBrowserState(
      { id: 9, focused: true, state: "fullscreen" },
      {
        id: 93,
        url: "file:///C:/notes.html",
        title: "notes.html",
        audible: true,
      },
    ),
    {
      focused: true,
      window: { id: 9, state: "fullscreen" },
      tab: {
        id: 93,
        domain: "(local-file)",
        title: "notes.html",
        title_status: "ok",
        audible: true,
      },
    },
    "state_local_file",
  );

  assertDeepEqual(
    buildBrowserState(
      { id: 10, focused: true, state: "normal", incognito: true },
      { id: 94, url: "https://example.com", title: "Secret", audible: false },
    ),
    { focused: false, window: null, tab: null },
    "incognito window suppressed",
  );
});

Deno.test("statesEqual detects unchanged and changed browser states", () => {
  const base = buildBrowserState(
    { id: 7, focused: true, state: "normal" },
    {
      id: 91,
      url: "https://github.com/4o3F/quartz",
      title: "4o3F/quartz",
      audible: false,
    },
  );
  const copy = buildBrowserState(
    { id: 7, focused: true, state: "normal" },
    {
      id: 91,
      url: "https://github.com/4o3F/quartz/issues",
      title: "4o3F/quartz",
      audible: false,
    },
  );
  assertEqual(statesEqual(base, copy), true, "same domain and title equal");

  const differentTitle = {
    ...base,
    tab: { ...base.tab, title: "Changed" },
  };
  assertEqual(statesEqual(base, differentTitle), false, "title change");

  const differentAudible = {
    ...base,
    tab: { ...base.tab, audible: true },
  };
  assertEqual(statesEqual(base, differentAudible), false, "audible change");

  const unfocused = buildBrowserState(null, null);
  assertEqual(statesEqual(base, unfocused), false, "focus change");
  assertEqual(
    statesEqual(unfocused, buildBrowserState(null, null)),
    true,
    "both unfocused equal",
  );
});
