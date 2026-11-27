import { detectBrowser, HostConnection } from "./connection.js";
import { buildBrowserState } from "./state.js";

const api = globalThis.browser ?? globalThis.chrome;
const PROFILE_ID_KEY = "profile_instance_id";
const RECONNECT_ALARM_NAME = "quartz-reconnect";
const RECONNECT_ALARM_PERIOD_MINUTES = 1;
const WINDOW_ID_NONE = api.windows?.WINDOW_ID_NONE ?? -1;

let profileIdPromise = null;
let syncRunning = false;
let syncPending = false;
let pendingForceUnfocused = false;

function currentBrowser() {
  const manifest = api.runtime.getManifest?.();
  const isFirefox = Boolean(manifest?.browser_specific_settings?.gecko);
  const brands = globalThis.navigator?.userAgentData?.brands;
  const isEdgeBrand = Array.isArray(brands) &&
    brands.some((entry) =>
      typeof entry?.brand === "string" && /edge/i.test(entry.brand)
    );
  return detectBrowser(
    globalThis.navigator?.userAgent ?? "",
    isFirefox,
    isEdgeBrand,
  );
}

function setBadgeText(text) {
  try {
    const result = api.action?.setBadgeText?.({ text });
    if (result && typeof result.catch === "function") {
      result.catch(() => {
        // Ignore badge errors during browser shutdown.
      });
    }
  } catch {
    // Ignore badge errors when action API is unavailable.
  }
}

function getProfileInstanceId() {
  if (!profileIdPromise) {
    profileIdPromise = (async () => {
      const stored = await api.storage.local.get(PROFILE_ID_KEY);
      const existing = stored?.[PROFILE_ID_KEY];
      if (typeof existing === "string" && existing.length > 0) {
        return existing;
      }
      const generated = crypto.randomUUID();
      await api.storage.local.set({ [PROFILE_ID_KEY]: generated });
      return generated;
    })();
    profileIdPromise.catch(() => {
      profileIdPromise = null;
    });
  }
  return profileIdPromise;
}

async function ensureReconnectAlarm() {
  try {
    const existing = await api.alarms.get(RECONNECT_ALARM_NAME);
    if (!existing) {
      await api.alarms.create(RECONNECT_ALARM_NAME, {
        periodInMinutes: RECONNECT_ALARM_PERIOD_MINUTES,
      });
    }
  } catch {
    // Ignore alarm errors during browser shutdown.
  }
}

async function readCurrentState(forceUnfocused) {
  let win = null;
  try {
    win = await api.windows.getLastFocused({ populate: false });
  } catch {
    return buildBrowserState(null, null);
  }
  if (!win || win.id === WINDOW_ID_NONE || win.incognito) {
    return buildBrowserState(null, null);
  }
  if (forceUnfocused) {
    if (win.state === "minimized") {
      return buildBrowserState(win, null);
    }
    return buildBrowserState(null, null);
  }
  if (win.state === "minimized" || !win.focused) {
    return buildBrowserState(win, null);
  }
  let tabs = [];
  try {
    tabs = await api.tabs.query({ active: true, windowId: win.id });
  } catch {
    return buildBrowserState(win, null);
  }
  return buildBrowserState(win, tabs[0] ?? null);
}

const connection = new HostConnection({
  connectNative: (hostName) => api.runtime.connectNative(hostName),
  onBadgeChange: setBadgeText,
  onReconnectDue: () => {
    if (!connection.isConnected) {
      scheduleSync();
    }
  },
  getLastError: () => api.runtime.lastError?.message ?? null,
});

function scheduleSync(forceUnfocused = false) {
  if (syncRunning) {
    syncPending = true;
    pendingForceUnfocused = forceUnfocused;
    return;
  }
  void runSyncLoop(forceUnfocused);
}

async function runSyncLoop(initialForceUnfocused) {
  syncRunning = true;
  let forceUnfocused = initialForceUnfocused;
  try {
    do {
      syncPending = false;
      const [profileId, state] = await Promise.all([
        getProfileInstanceId(),
        readCurrentState(forceUnfocused),
      ]);
      if (!connection.isConnected) {
        connection.connect(profileId, currentBrowser(), state);
      } else {
        connection.sendState(state);
      }
      forceUnfocused = pendingForceUnfocused;
    } while (syncPending);
  } catch {
    // Ignore transient browser API errors during startup or shutdown.
  } finally {
    syncRunning = false;
  }
}

api.tabs.onActivated.addListener(() => {
  scheduleSync();
});

api.tabs.onUpdated.addListener((_tabId, changeInfo) => {
  if ("url" in changeInfo || "title" in changeInfo || "audible" in changeInfo) {
    scheduleSync();
  }
});

api.tabs.onRemoved.addListener(() => {
  scheduleSync();
});

api.windows.onFocusChanged.addListener((windowId) => {
  scheduleSync(windowId === WINDOW_ID_NONE);
});

api.windows.onRemoved.addListener(() => {
  scheduleSync();
});

api.alarms.onAlarm.addListener((alarm) => {
  if (alarm?.name === RECONNECT_ALARM_NAME && !connection.isConnected) {
    scheduleSync();
  }
});

api.runtime.onInstalled?.addListener(() => {
  void ensureReconnectAlarm();
  scheduleSync();
});

api.runtime.onStartup?.addListener(() => {
  void ensureReconnectAlarm();
  scheduleSync();
});

void ensureReconnectAlarm();
scheduleSync();
