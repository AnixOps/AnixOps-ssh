package com.anixops.ssh

import android.app.Activity
import android.content.ContentValues
import android.content.Intent
import android.os.Build
import android.os.Environment
import android.provider.MediaStore
import android.webkit.JavascriptInterface
import android.webkit.MimeTypeMap
import androidx.core.content.FileProvider
import java.io.File

/**
 * What the web view cannot do itself: hand a downloaded file to the share
 * sheet or put it in the Downloads folder. Both take the path of a file the
 * engine downloaded (it lives in the app's cache folder); anything else is
 * refused. Each returns an empty string when it worked and the reason when it
 * did not.
 */
class AnixNative(private val activity: Activity) {
    private val downloads: File = File(activity.cacheDir, "downloads")

    private fun downloaded(path: String): File? {
        val file = File(path).canonicalFile
        val root = downloads.canonicalFile
        return if (file.isFile && file.path.startsWith(root.path + File.separator)) file else null
    }

    private fun mimeOf(file: File): String {
        val extension = file.extension.lowercase()
        return MimeTypeMap.getSingleton().getMimeTypeFromExtension(extension) ?: "application/octet-stream"
    }

    @JavascriptInterface
    fun shareFile(path: String): String {
        return try {
            val file = downloaded(path) ?: return "not a downloaded file"
            val uri = FileProvider.getUriForFile(activity, activity.packageName + ".fileprovider", file)
            val send = Intent(Intent.ACTION_SEND).apply {
                type = mimeOf(file)
                putExtra(Intent.EXTRA_STREAM, uri)
                addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            }
            activity.runOnUiThread {
                activity.startActivity(Intent.createChooser(send, file.name))
            }
            ""
        } catch (error: Exception) {
            error.message ?: "cannot share the file"
        }
    }

    @JavascriptInterface
    fun saveToDownloads(path: String): String {
        return try {
            val file = downloaded(path) ?: return "not a downloaded file"
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                val values = ContentValues().apply {
                    put(MediaStore.Downloads.DISPLAY_NAME, file.name)
                    put(MediaStore.Downloads.MIME_TYPE, mimeOf(file))
                    put(MediaStore.Downloads.IS_PENDING, 1)
                }
                val resolver = activity.contentResolver
                val target = resolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values)
                    ?: return "cannot create the file in Downloads"
                resolver.openOutputStream(target)?.use { out ->
                    file.inputStream().use { it.copyTo(out) }
                } ?: return "cannot write the file in Downloads"
                values.clear()
                values.put(MediaStore.Downloads.IS_PENDING, 0)
                resolver.update(target, values, null, null)
                ""
            } else {
                // Before Android 10 writing to Downloads needs a storage
                // permission the app does not ask for; the share sheet can
                // still save it.
                "unsupported"
            }
        } catch (error: Exception) {
            error.message ?: "cannot save the file"
        }
    }
}
