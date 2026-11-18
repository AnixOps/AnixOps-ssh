package com.anixops.ssh

import android.os.Bundle
import android.webkit.WebView
import androidx.activity.enableEdgeToEdge

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
  }

  // The page reaches the share sheet and Downloads through `window.AnixNative`
  // (src/lib/mobileShare.ts).
  override fun onWebViewCreate(webView: WebView) {
    webView.addJavascriptInterface(AnixNative(this), "AnixNative")
  }
}
