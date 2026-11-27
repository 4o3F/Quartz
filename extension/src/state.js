export const MAX_TITLE_UTF16_UNITS = 4_096;
export const INTERNAL_DOMAIN = "(internal)";
export const LOCAL_FILE_DOMAIN = "(local-file)";

const MAX_DOMAIN_LEN = 253;
const MAX_DOMAIN_LABEL_LEN = 63;
const DOMAIN_LABEL_PATTERN = /^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/;
const VALID_WINDOW_STATES = new Set([
  "normal",
  "maximized",
  "minimized",
  "fullscreen",
]);

function isValidHostname(host) {
  if (host.length === 0 || host.length > MAX_DOMAIN_LEN) {
    return false;
  }
  const labels = host.split(".");
  for (const label of labels) {
    if (
      label.length === 0 ||
      label.length > MAX_DOMAIN_LABEL_LEN ||
      !DOMAIN_LABEL_PATTERN.test(label) ||
      label === "xn--"
    ) {
      return false;
    }
  }
  return true;
}

export function extractDomain(rawUrl) {
  if (typeof rawUrl !== "string" || rawUrl.trim() === "") {
    return INTERNAL_DOMAIN;
  }
  let parsed;
  try {
    parsed = new URL(rawUrl);
  } catch {
    return INTERNAL_DOMAIN;
  }
  if (parsed.protocol === "file:") {
    return LOCAL_FILE_DOMAIN;
  }
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
    return INTERNAL_DOMAIN;
  }
  let host = parsed.hostname.toLowerCase();
  if (host.endsWith(".")) {
    host = host.slice(0, -1);
  }
  return isValidHostname(host) ? host : INTERNAL_DOMAIN;
}

export function capTitle(rawTitle) {
  if (typeof rawTitle !== "string" || rawTitle.length === 0) {
    return { title: "", title_status: "empty" };
  }
  if (rawTitle.length <= MAX_TITLE_UTF16_UNITS) {
    return { title: rawTitle, title_status: "ok" };
  }
  let end = MAX_TITLE_UTF16_UNITS;
  const lastUnit = rawTitle.charCodeAt(end - 1);
  if (lastUnit >= 0xd800 && lastUnit <= 0xdbff) {
    end -= 1;
  }
  return {
    title: rawTitle.slice(0, end),
    title_status: "truncated",
  };
}

export function normalizeWindowState(state) {
  if (typeof state === "string" && VALID_WINDOW_STATES.has(state)) {
    return state;
  }
  if (state === "locked-fullscreen") {
    return "fullscreen";
  }
  return "normal";
}

export function buildBrowserState(windowInfo, tabInfo) {
  if (
    !windowInfo ||
    typeof windowInfo.id !== "number" ||
    windowInfo.id < 0 ||
    windowInfo.incognito
  ) {
    return { focused: false, window: null, tab: null };
  }

  const windowState = normalizeWindowState(windowInfo.state);
  if (windowState === "minimized") {
    return {
      focused: false,
      window: { id: windowInfo.id, state: "minimized" },
      tab: null,
    };
  }

  if (!windowInfo.focused) {
    return { focused: false, window: null, tab: null };
  }

  const windowBody = { id: windowInfo.id, state: windowState };
  if (!tabInfo || typeof tabInfo.id !== "number" || tabInfo.incognito) {
    return { focused: true, window: windowBody, tab: null };
  }

  const { title, title_status } = capTitle(tabInfo.title);
  const rawUrl = typeof tabInfo.url === "string" && tabInfo.url.length > 0
    ? tabInfo.url
    : tabInfo.pendingUrl;

  return {
    focused: true,
    window: windowBody,
    tab: {
      id: tabInfo.id,
      domain: extractDomain(rawUrl),
      title,
      title_status,
      audible: Boolean(tabInfo.audible),
    },
  };
}

export function statesEqual(left, right) {
  if (left === right) {
    return true;
  }
  if (!left || !right || left.focused !== right.focused) {
    return false;
  }
  if (left.window === null || right.window === null) {
    if (left.window !== right.window) {
      return false;
    }
  } else if (
    left.window.id !== right.window.id ||
    left.window.state !== right.window.state
  ) {
    return false;
  }
  if (left.tab === null || right.tab === null) {
    return left.tab === right.tab;
  }
  return (
    left.tab.id === right.tab.id &&
    left.tab.domain === right.tab.domain &&
    left.tab.title === right.tab.title &&
    left.tab.title_status === right.tab.title_status &&
    left.tab.audible === right.tab.audible
  );
}
