/** Whether this is a Tauri mobile shell (Android, iOS, iPadOS), where the app
 *  shows the touch UI of `components/mobile` instead of the desktop window.
 *  `?mobile=1` forces it in a browser (preview and screenshots), `?mobile=0`
 *  forces it off. */
export function detectMobileShell(userAgent: string, touchPoints: number, search: string): boolean {
  const forced = new URLSearchParams(search).get("mobile");
  if (forced === "1") return true;
  if (forced === "0") return false;
  if (/Android|iPhone|iPad|iPod/i.test(userAgent)) return true;
  // iPadOS reports a Mac; a Mac with a touch screen is an iPad.
  return /Macintosh/.test(userAgent) && touchPoints > 1;
}

export function isMobileShell(): boolean {
  return detectMobileShell(navigator.userAgent, navigator.maxTouchPoints, window.location.search);
}
