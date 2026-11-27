import {
  backoffDelayMs,
  BADGE_CONNECTED,
  BADGE_OFFLINE,
  BADGE_UNREGISTERED,
  detectBrowser,
  HostConnection,
  NATIVE_HOST_NAME,
} from "./connection.js";
import { buildBrowserState } from "./state.js";

const TEST_PROFILE_ID = "a286737e-3855-43cb-98ee-c366344372d0";

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

function createFakePort() {
  const messageListeners = [];
  const disconnectListeners = [];
  const posted = [];
  let disconnected = false;
  return {
    posted,
    error: null,
    get disconnected() {
      return disconnected;
    },
    postMessage(message) {
      if (disconnected) {
        throw new Error("port disconnected");
      }
      posted.push(message);
    },
    disconnect() {
      disconnected = true;
    },
    emitMessage(message) {
      for (const listener of messageListeners) {
        listener(message);
      }
    },
    emitDisconnect() {
      disconnected = true;
      for (const listener of disconnectListeners) {
        listener();
      }
    },
    onMessage: {
      addListener(listener) {
        messageListeners.push(listener);
      },
    },
    onDisconnect: {
      addListener(listener) {
        disconnectListeners.push(listener);
      },
    },
  };
}

function createFakeTimers() {
  let nextTimerId = 1;
  const scheduled = [];
  return {
    scheduled,
    setTimer(callback, delayMs) {
      const id = nextTimerId;
      nextTimerId += 1;
      scheduled.push({ id, delayMs, callback, cleared: false });
      return id;
    },
    clearTimer(id) {
      const entry = scheduled.find((item) => item.id === id);
      if (entry) {
        entry.cleared = true;
      }
    },
  };
}

Deno.test("backoffDelayMs follows 1, 2, 4, 8, 16, 32, 60, 60 second sequence", () => {
  const seconds = [0, 1, 2, 3, 4, 5, 6, 7, 8].map((attempt) =>
    backoffDelayMs(attempt) / 1_000
  );
  assertDeepEqual(
    seconds,
    [1, 2, 4, 8, 16, 32, 60, 60, 60],
    "backoff seconds",
  );
});

Deno.test("detectBrowser identifies chrome, edge and firefox", () => {
  assertEqual(
    detectBrowser(
      "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/120.0.0.0 Safari/537.36",
    ),
    "chrome",
    "chrome UA",
  );
  assertEqual(
    detectBrowser(
      "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/120.0.0.0 Safari/537.36 Edg/120.0.0.0",
    ),
    "edge",
    "edge UA",
  );
  assertEqual(
    detectBrowser(
      "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:128.0) Gecko/20100101 Firefox/128.0",
    ),
    "firefox",
    "firefox UA",
  );
  assertEqual(detectBrowser("", true, false), "firefox", "gecko manifest flag");
});

Deno.test("HostConnection increments request ids and suppresses unchanged state", () => {
  const ports = [];
  const hostNames = [];
  const badges = [];
  const timers = createFakeTimers();

  const connection = new HostConnection({
    connectNative(hostName) {
      hostNames.push(hostName);
      const port = createFakePort();
      ports.push(port);
      return port;
    },
    onBadgeChange(text) {
      badges.push(text);
    },
    onReconnectDue() {},
    setTimer: timers.setTimer,
    clearTimer: timers.clearTimer,
  });

  const state1 = buildBrowserState(
    { id: 7, focused: true, state: "normal" },
    {
      id: 91,
      url: "https://github.com/4o3F/quartz",
      title: "4o3F/quartz",
      audible: false,
    },
  );
  const state1Copy = buildBrowserState(
    { id: 7, focused: true, state: "normal" },
    {
      id: 91,
      url: "https://github.com/4o3F/quartz/pulls",
      title: "4o3F/quartz",
      audible: false,
    },
  );
  const state2 = buildBrowserState(null, null);

  assertEqual(
    connection.connect(TEST_PROFILE_ID, "chrome", state1),
    true,
    "connect succeeds",
  );
  assertDeepEqual(hostNames, [NATIVE_HOST_NAME], "native host name");
  assertEqual(ports.length, 1, "one port created");

  const port = ports[0];
  assertDeepEqual(
    port.posted,
    [
      {
        t: "req",
        id: 1,
        op: "browser.hello",
        body: {
          profile_instance_id: TEST_PROFILE_ID,
          browser: "chrome",
          proto: { min: 1, max: 1 },
        },
      },
      {
        t: "req",
        id: 2,
        op: "browser.state",
        body: state1,
      },
    ],
    "hello then initial state",
  );

  port.emitMessage({
    t: "res",
    id: 1,
    ok: true,
    body: { proto: 1, core_version: "0.1.0", instance_id: "inst" },
  });
  assertEqual(connection.badge, BADGE_CONNECTED, "connected badge");
  assertEqual(connection.isHandshakeConfirmed, true, "handshake confirmed");

  assertEqual(connection.sendState(state1Copy), false, "unchanged suppressed");
  assertEqual(port.posted.length, 2, "no frame sent for unchanged state");

  assertEqual(connection.sendState(state2), true, "changed state sent");
  assertDeepEqual(
    port.posted[2],
    { t: "req", id: 3, op: "browser.state", body: state2 },
    "next state increments id to 3",
  );
  assertDeepEqual(badges, [BADGE_CONNECTED], "badge updated to empty");
});

Deno.test("HostConnection handles core_offline with ! badge, backoff and full resend on reconnect", () => {
  const ports = [];
  const badges = [];
  const timers = createFakeTimers();

  const state = buildBrowserState(
    { id: 7, focused: true, state: "normal" },
    { id: 91, url: "https://github.com", title: "GitHub", audible: false },
  );

  const connection = new HostConnection({
    connectNative() {
      const port = createFakePort();
      ports.push(port);
      return port;
    },
    onBadgeChange(text) {
      badges.push(text);
    },
    onReconnectDue() {
      connection.connect(TEST_PROFILE_ID, "edge", state);
    },
    setTimer: timers.setTimer,
    clearTimer: timers.clearTimer,
  });

  const expectedDelays = [
    1_000,
    2_000,
    4_000,
    8_000,
    16_000,
    32_000,
    60_000,
    60_000,
  ];
  connection.connect(TEST_PROFILE_ID, "edge", state);

  for (let i = 0; i < expectedDelays.length; i += 1) {
    const port = ports[i];
    port.emitMessage({
      t: "evt",
      topic: "control",
      seq: 1,
      body: { op: "core_offline" },
    });
    // Browser also fires onDisconnect after host exits; must not double-schedule.
    port.emitDisconnect();
    assertEqual(connection.isConnected, false, `disconnected attempt ${i}`);
    assertEqual(connection.badge, BADGE_OFFLINE, `offline badge attempt ${i}`);
    assertEqual(timers.scheduled.length, i + 1, `single timer attempt ${i}`);
    assertEqual(
      timers.scheduled[i].delayMs,
      expectedDelays[i],
      `delay attempt ${i}`,
    );
    timers.scheduled[i].callback();
  }

  // Reconnect after core_offline must re-send hello + full state with fresh ids.
  const latestPort = ports[ports.length - 1];
  assertEqual(latestPort.posted.length, 2, "hello and full state resent");
  assertEqual(latestPort.posted[0].op, "browser.hello", "hello op");
  assertEqual(latestPort.posted[1].op, "browser.state", "state op");
  assertEqual(
    latestPort.posted[1].id,
    latestPort.posted[0].id + 1,
    "state id follows hello id",
  );

  // Confirming handshake clears badge and resets backoff back to 1s.
  latestPort.emitMessage({
    t: "res",
    id: latestPort.posted[0].id,
    ok: true,
    body: { proto: 1 },
  });
  assertEqual(connection.badge, BADGE_CONNECTED, "badge cleared on connect");
  assertEqual(connection.backoffAttempt, 0, "backoff reset after handshake");
  assertDeepEqual(badges, [BADGE_OFFLINE, BADGE_CONNECTED], "badge sequence");
});

Deno.test("HostConnection sets ? badge when native host is not registered", () => {
  const timers = createFakeTimers();
  const badges = [];
  let lastError = null;

  const syncThrowConn = new HostConnection({
    connectNative() {
      throw new Error("No such native application dev.4o3f.quartz");
    },
    onBadgeChange(text) {
      badges.push(text);
    },
    onReconnectDue() {},
    setTimer: timers.setTimer,
    clearTimer: timers.clearTimer,
  });
  assertEqual(
    syncThrowConn.connect(TEST_PROFILE_ID, "firefox", null),
    false,
    "sync throw returns false",
  );
  assertEqual(syncThrowConn.badge, BADGE_UNREGISTERED, "sync throw badge ?");

  let port;
  const asyncDisconnectConn = new HostConnection({
    connectNative() {
      port = createFakePort();
      return port;
    },
    onBadgeChange(text) {
      badges.push(text);
    },
    onReconnectDue() {},
    getLastError: () => lastError,
    setTimer: timers.setTimer,
    clearTimer: timers.clearTimer,
  });
  asyncDisconnectConn.connect(TEST_PROFILE_ID, "chrome", null);
  lastError = "Specified native messaging host not found.";
  port.emitDisconnect();
  assertEqual(
    asyncDisconnectConn.badge,
    BADGE_UNREGISTERED,
    "chrome missing host badge ?",
  );
});
