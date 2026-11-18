package com.anixops.ssh

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.webkit.JavascriptInterface
import androidx.core.content.ContextCompat

/** What the web page can ask of Android. Only the opt-in session notification
 *  (see docs/mobile-prototype.md, "Background execution"). */
class AndroidBridge(private val activity: MainActivity) {
  @JavascriptInterface
  fun setSessionService(enabled: Boolean, title: String, text: String) {
    activity.runOnUiThread {
      val intent = Intent(activity, SessionService::class.java)
        .putExtra("title", title)
        .putExtra("text", text)
      if (enabled) {
        val needsPermission = Build.VERSION.SDK_INT >= 33 &&
          ContextCompat.checkSelfPermission(activity, Manifest.permission.POST_NOTIFICATIONS) !=
          PackageManager.PERMISSION_GRANTED
        if (needsPermission) {
          activity.requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS)) { }
        }
        ContextCompat.startForegroundService(activity, intent)
      } else {
        activity.stopService(intent)
      }
    }
  }
}
