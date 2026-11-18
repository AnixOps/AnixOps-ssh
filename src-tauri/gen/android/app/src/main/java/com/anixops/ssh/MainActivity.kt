package com.anixops.ssh

import android.os.Bundle
import android.view.View
import android.webkit.WebView
import androidx.activity.enableEdgeToEdge
import androidx.core.graphics.Insets
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)

    // With edge-to-edge the system no longer shrinks the window for the soft
    // keyboard, so the web view would sit under it. Lift the content by the
    // keyboard's height; the page then sees a shorter viewport and puts the
    // key bar directly above the keyboard (src/lib/keyboardViewport.ts).
    val content = findViewById<View>(android.R.id.content)
    ViewCompat.setOnApplyWindowInsetsListener(content) { view, insets ->
      val ime = insets.getInsets(WindowInsetsCompat.Type.ime())
      view.setPadding(0, 0, 0, ime.bottom)
      // The keyboard is handled here; the web view must not shrink for it again.
      WindowInsetsCompat.Builder(insets).setInsets(WindowInsetsCompat.Type.ime(), Insets.NONE).build()
    }
  }

  override fun onWebViewCreate(webView: WebView) {
    super.onWebViewCreate(webView)
    // The page asks for the session notification through this object
    // (src/lib/androidBridge.ts); the page is local, never remote content.
    webView.addJavascriptInterface(AndroidBridge(this), "AnixOpsAndroid")
  }
}
