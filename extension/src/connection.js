import { statesEqual } from "./state.js";

export const NATIVE_HOST_NAME = "dev.4o3f.quartz";
export const PROTO_VERSION = 1;
export const INITIAL_BACKOFF_MS = 1_000;
export const MAX_BACKOFF_MS = 60_000;

export const BADGE_CONNECTED = "";
export const BADGE_OFFLINE = "!";
export const BADGE_UNREGISTERED = "?";

export function backoffDelayMs(attempt) {
  const safeAttempt = Math.max(0, Math.floor(attempt));
  if (safeAttempt >= 6) {
    return MAX_BACKOFF_MS;
  }
  return Math.min(INITIAL_BACKOFF_MS * (2 ** safeAttempt), MAX_BACKOFF_MS);
}

export function detectBrowser(
  userAgent = "",
  isFirefox = false,
  isEdgeBrand = false,
) {
  if (isFirefox || /Firefox\//i.test(userAgent)) {
    return "firefox";
  }
  if (isEdgeBrand || /Edg\//i.test(userAgent)) {
    return "edge";
  }
  return "chrome";
}

export function buildHelloRequest(id, profileInstanceId, browser) {
  return {
    t: "req",
    id,
    op: "browser.hello",
    body: {
      profile_instance_id: profileInstanceId,
      browser,
      proto: { min: PROTO_VERSION, max: PROTO_VERSION },
    },
  };
}

export function buildStateRequest(id, state) {
  return {
    t: "req",
    id,
    op: "browser.state",
    body: state,
  };
}

export function isCoreOfflineEvent(message) {
  return (
    message !== null &&
    typeof message === "object" &&
    message.t === "evt" &&
    message.topic === "control" &&
    message.body !== null &&
    typeof message.body === "object" &&
    message.body.op === "core_offline"
  );
}

export function isHostNotRegisteredError(message) {
  if (typeof message !== "string") {
    return false;
  }
  const lower = message.toLowerCase();
  return (
    lower.includes("native messaging host not found") ||
    lower.includes("no such native application") ||
    lower.includes("native messaging host is forbidden")
  );
}

export class HostConnection {
  #connectNative;
  #onBadgeChange;
  #onReconnectDue;
  #getLastError;
  #setTimer;
  #clearTimer;

  #port = null;
  #nextId = 1;
  #helloRequestId = null;
  #handshakeConfirmed = false;
  #lastSentState = null;
  #backoffAttempt = 0;
  #reconnectTimer = null;
  #badge = null;

  constructor({
    connectNative,
    onBadgeChange,
    onReconnectDue,
    getLastError = () => null,
    setTimer = (callback, delayMs) => globalThis.setTimeout(callback, delayMs),
    clearTimer = (timerId) => globalThis.clearTimeout(timerId),
  }) {
    this.#connectNative = connectNative;
    this.#onBadgeChange = onBadgeChange;
    this.#onReconnectDue = onReconnectDue;
    this.#getLastError = getLastError;
    this.#setTimer = setTimer;
    this.#clearTimer = clearTimer;
  }

  get isConnected() {
    return this.#port !== null;
  }

  get isHandshakeConfirmed() {
    return this.#port !== null && this.#handshakeConfirmed;
  }

  get badge() {
    return this.#badge ?? BADGE_CONNECTED;
  }

  get backoffAttempt() {
    return this.#backoffAttempt;
  }

  connect(profileInstanceId, browser, initialState) {
    if (this.#port !== null) {
      return true;
    }
    this.#clearReconnectTimer();

    let port;
    try {
      port = this.#connectNative(NATIVE_HOST_NAME);
    } catch {
      this.#setBadge(BADGE_UNREGISTERED);
      this.#scheduleReconnect();
      return false;
    }

    this.#port = port;
    this.#handshakeConfirmed = false;
    this.#lastSentState = null;

    port.onMessage.addListener((message) => this.#handleMessage(port, message));
    port.onDisconnect.addListener(() => this.#handleDisconnect(port));

    const helloId = this.#allocateId();
    this.#helloRequestId = helloId;
    const helloFrame = buildHelloRequest(helloId, profileInstanceId, browser);
    if (!this.#post(port, helloFrame)) {
      return false;
    }
    if (initialState !== undefined && initialState !== null) {
      this.sendState(initialState);
    }
    return true;
  }

  sendState(state) {
    if (this.#port === null) {
      return false;
    }
    if (
      this.#lastSentState !== null &&
      statesEqual(this.#lastSentState, state)
    ) {
      return false;
    }
    const id = this.#allocateId();
    if (!this.#post(this.#port, buildStateRequest(id, state))) {
      return false;
    }
    this.#lastSentState = state;
    return true;
  }

  #allocateId() {
    const id = this.#nextId;
    this.#nextId += 1;
    return id;
  }

  #post(port, frame) {
    try {
      port.postMessage(frame);
      return true;
    } catch {
      this.#dropPort(port, BADGE_OFFLINE);
      return false;
    }
  }

  #handleMessage(port, message) {
    if (port !== this.#port) {
      return;
    }
    if (isCoreOfflineEvent(message)) {
      this.#dropPort(port, BADGE_OFFLINE);
      return;
    }
    if (
      message !== null &&
      typeof message === "object" &&
      message.t === "res" &&
      message.id === this.#helloRequestId
    ) {
      if (message.ok === true) {
        this.#handshakeConfirmed = true;
        this.#backoffAttempt = 0;
        this.#setBadge(BADGE_CONNECTED);
      } else {
        this.#dropPort(port, BADGE_OFFLINE);
      }
    }
  }

  #handleDisconnect(port) {
    if (port !== this.#port) {
      return;
    }
    const errorMessage = port.error?.message ?? this.#getLastError() ?? "";
    const isUnregistered = !this.#handshakeConfirmed &&
      isHostNotRegisteredError(errorMessage);
    const nextBadge = isUnregistered ? BADGE_UNREGISTERED : BADGE_OFFLINE;
    this.#dropPort(port, nextBadge);
  }

  #dropPort(port, badge) {
    if (this.#port !== port) {
      return;
    }
    this.#port = null;
    this.#helloRequestId = null;
    this.#handshakeConfirmed = false;
    this.#lastSentState = null;
    this.#setBadge(badge);
    try {
      port.disconnect?.();
    } catch {
      // Port is already closed.
    }
    this.#scheduleReconnect();
  }

  #scheduleReconnect() {
    if (this.#reconnectTimer !== null) {
      return;
    }
    const delayMs = backoffDelayMs(this.#backoffAttempt);
    this.#backoffAttempt += 1;
    this.#reconnectTimer = this.#setTimer(() => {
      this.#reconnectTimer = null;
      this.#onReconnectDue?.();
    }, delayMs);
  }

  #clearReconnectTimer() {
    if (this.#reconnectTimer !== null) {
      this.#clearTimer(this.#reconnectTimer);
      this.#reconnectTimer = null;
    }
  }

  #setBadge(text) {
    if (this.#badge === text) {
      return;
    }
    this.#badge = text;
    this.#onBadgeChange?.(text);
  }
}
