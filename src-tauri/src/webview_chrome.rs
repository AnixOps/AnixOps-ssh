//! The web view should behave like an app, not a browser. The frontend
//! (`src/lib/webviewChrome.ts`) replaces the right-click menu and swallows
//! browser keys on every platform; on Windows, release builds also switch the
//! same things off in WebView2 itself, so nothing leaks through before the
//! page has loaded. Debug builds keep both for the inspector.

use tauri::App;

pub fn lock_down(app: &App) {
    #[cfg(all(windows, not(debug_assertions)))]
    for window in tauri::Manager::webview_windows(app).into_values() {
        let _ = window.with_webview(|webview| unsafe {
            use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings3;
            use windows_core::Interface;

            let Ok(core) = webview.controller().CoreWebView2() else {
                return;
            };
            let Ok(settings) = core.Settings() else {
                return;
            };
            // The page still gets `contextmenu` and shows its own menu.
            let _ = settings.SetAreDefaultContextMenusEnabled(false);
            // Reload, print, find, save, zoom and the like; editing keys
            // (copy, paste, undo, select all) keep working.
            if let Ok(settings) = settings.cast::<ICoreWebView2Settings3>() {
                let _ = settings.SetAreBrowserAcceleratorKeysEnabled(false);
            }
        });
    }
    #[cfg(not(all(windows, not(debug_assertions))))]
    let _ = app;
}
