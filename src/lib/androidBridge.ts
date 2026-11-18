/* What the Android shell offers the page (see MainActivity.kt and
   AndroidBridge.kt). Everywhere else (iOS, a browser) there is no bridge and
   every call is a no-op. */

interface SessionServiceBridge {
  setSessionService(enabled: boolean, title: string, text: string): void;
}

function bridge(): SessionServiceBridge | null {
  const candidate = (window as unknown as { AnixOpsAndroid?: Partial<SessionServiceBridge> }).AnixOpsAndroid;
  return candidate && typeof candidate.setSessionService === "function" ? (candidate as SessionServiceBridge) : null;
}

/** Whether this is the Android app, where the foreground notification exists. */
export function hasSessionNotification(): boolean {
  return bridge() !== null;
}

/** Shows or hides the ongoing "sessions are open" notification. */
export function setSessionNotification(enabled: boolean, title: string, text: string): void {
  try {
    bridge()?.setSessionService(enabled, title, text);
  } catch {
    // The service could not start (permission, system limits); the app works without it.
  }
}
