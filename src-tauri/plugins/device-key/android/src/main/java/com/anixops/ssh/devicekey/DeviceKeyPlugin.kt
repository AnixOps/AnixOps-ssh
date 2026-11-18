package com.anixops.ssh.devicekey

import android.app.Activity
import android.security.keystore.KeyPermanentlyInvalidatedException
import android.util.Base64
import androidx.biometric.BiometricManager
import androidx.biometric.BiometricPrompt
import androidx.core.content.ContextCompat
import androidx.fragment.app.FragmentActivity
import app.tauri.annotation.Command
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.util.Locale
import javax.crypto.Cipher
import javax.crypto.spec.GCMParameterSpec
import org.json.JSONObject

/**
 * The Kotlin side of `easyssh-mobile-keys`: the commands in
 * `bridge.rs` (`secureGet`, `boundOpen`, ...). Rust rejects nothing it did
 * not ask for; every failure here is a rejection with a code Rust maps:
 * `cancelled`, `invalidated`, `unavailable`, or `failed`.
 */
@TauriPlugin
class DeviceKeyPlugin(private val activity: Activity) : Plugin(activity) {
    private val box = KeystoreBox(activity)

    // ---- secure storage ------------------------------------------------------

    @Command
    fun secureGet(invoke: Invoke) {
        guarded(invoke, "unavailable") {
            val args = invoke.getArgs()
            val value = box.get(args.getString("service"), args.getString("account"))
            val reply = JSObject()
            if (value == null) {
                reply.put("value", JSONObject.NULL)
            } else {
                reply.put("value", Base64.encodeToString(value, Base64.NO_WRAP))
                value.fill(0)
            }
            invoke.resolve(reply)
        }
    }

    @Command
    fun secureSet(invoke: Invoke) {
        guarded(invoke, "unavailable") {
            val args = invoke.getArgs()
            val value = Base64.decode(args.getString("value"), Base64.DEFAULT)
            try {
                box.set(args.getString("service"), args.getString("account"), value)
            } finally {
                value.fill(0)
            }
            invoke.resolve()
        }
    }

    @Command
    fun secureDelete(invoke: Invoke) {
        guarded(invoke, "unavailable") {
            val args = invoke.getArgs()
            box.delete(args.getString("service"), args.getString("account"))
            invoke.resolve()
        }
    }

    // ---- biometric-bound storage -----------------------------------------------

    private fun biometricsReady(): Boolean =
        BiometricManager.from(activity).canAuthenticate(BiometricManager.Authenticators.BIOMETRIC_STRONG) ==
            BiometricManager.BIOMETRIC_SUCCESS

    @Command
    fun boundStatus(invoke: Invoke) {
        val reply = JSObject()
        reply.put("available", biometricsReady())
        invoke.resolve(reply)
    }

    @Command
    fun boundContains(invoke: Invoke) {
        val reply = JSObject()
        reply.put("present", box.hasBoundBlob(invoke.getArgs().getString("account")))
        invoke.resolve(reply)
    }

    @Command
    fun boundRemove(invoke: Invoke) {
        guarded(invoke, "unavailable") {
            box.removeBound(invoke.getArgs().getString("account"))
            invoke.resolve()
        }
    }

    /** Stores a value behind the sensor: one prompt, then the key encrypts. */
    @Command
    fun boundSeal(invoke: Invoke) {
        guarded(invoke, "unavailable") {
            val args = invoke.getArgs()
            val account = args.getString("account")
            val reason = args.getString("reason", "") ?: ""
            if (!biometricsReady()) {
                invoke.reject("no fingerprint or face is enrolled", "unavailable")
                return@guarded
            }
            val value = Base64.decode(args.getString("value"), Base64.DEFAULT)
            val cipher = Cipher.getInstance(KeystoreBox.TRANSFORMATION)
            cipher.init(Cipher.ENCRYPT_MODE, box.newBoundKey())
            prompt(invoke, cipher, reason) { authenticated ->
                try {
                    authenticated.updateAAD(KeystoreBox.aad("bound", account))
                    val sealed = authenticated.doFinal(value)
                    box.storeBoundBlob(account, authenticated.iv + sealed)
                    invoke.resolve()
                } finally {
                    value.fill(0)
                }
            }
        }
    }

    /** Releases the stored value: one prompt, and only a match lets the key decrypt. */
    @Command
    fun boundOpen(invoke: Invoke) {
        guarded(invoke, "unavailable") {
            val args = invoke.getArgs()
            val account = args.getString("account")
            val reason = args.getString("reason", "") ?: ""
            val blob = box.boundBlob(account)
            if (blob == null) {
                val none = JSObject()
                none.put("value", JSONObject.NULL)
                invoke.resolve(none)
                return@guarded
            }
            val key = box.boundKeyOrNull()
            if (key == null) {
                // The blob is still here but its key is gone: a fingerprint or
                // face was added, or the lock screen was removed.
                invoke.reject("the biometric key is gone", "invalidated")
                return@guarded
            }
            val cipher = Cipher.getInstance(KeystoreBox.TRANSFORMATION)
            try {
                cipher.init(
                    Cipher.DECRYPT_MODE,
                    key,
                    GCMParameterSpec(KeystoreBox.TAG_BITS, blob.copyOfRange(0, KeystoreBox.IV_BYTES)),
                )
            } catch (e: KeyPermanentlyInvalidatedException) {
                invoke.reject("the biometric key was invalidated", "invalidated")
                return@guarded
            }
            prompt(invoke, cipher, reason) { authenticated ->
                authenticated.updateAAD(KeystoreBox.aad("bound", account))
                val plain = authenticated.doFinal(blob, KeystoreBox.IV_BYTES, blob.size - KeystoreBox.IV_BYTES)
                try {
                    val reply = JSObject()
                    reply.put("value", Base64.encodeToString(plain, Base64.NO_WRAP))
                    invoke.resolve(reply)
                } finally {
                    plain.fill(0)
                }
            }
        }
    }

    /**
     * Shows the system prompt for an operation that needs the sensor. The
     * cipher is handed to [work] only after the match, inside the
     * CryptoObject the prompt returns; a rejected or cancelled prompt
     * rejects the invoke with the code Rust expects.
     */
    private fun prompt(invoke: Invoke, cipher: Cipher, reason: String, work: (Cipher) -> Unit) {
        activity.runOnUiThread {
            try {
                val callback = object : BiometricPrompt.AuthenticationCallback() {
                    override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
                        val authenticated = result.cryptoObject?.cipher
                        if (authenticated == null) {
                            invoke.reject("the prompt returned no key", "failed")
                            return
                        }
                        try {
                            work(authenticated)
                        } catch (e: Exception) {
                            invoke.reject(e.message ?: e.javaClass.simpleName, "failed")
                        }
                    }

                    override fun onAuthenticationError(errorCode: Int, errString: CharSequence) {
                        invoke.reject(errString.toString(), codeFor(errorCode))
                    }
                    // onAuthenticationFailed is a finger that did not match: the
                    // prompt stays up and the user tries again.
                }
                val info = BiometricPrompt.PromptInfo.Builder()
                    .setTitle("AnixOps SSH")
                    .setSubtitle(reason)
                    .setNegativeButtonText(if (Locale.getDefault().language == "zh") "使用口令" else "Use passphrase")
                    .setAllowedAuthenticators(BiometricManager.Authenticators.BIOMETRIC_STRONG)
                    .setConfirmationRequired(false)
                    .build()
                BiometricPrompt(activity as FragmentActivity, ContextCompat.getMainExecutor(activity), callback)
                    .authenticate(info, BiometricPrompt.CryptoObject(cipher))
            } catch (e: Exception) {
                invoke.reject(e.message ?: e.javaClass.simpleName, "failed")
            }
        }
    }

    private fun codeFor(errorCode: Int): String = when (errorCode) {
        BiometricPrompt.ERROR_USER_CANCELED,
        BiometricPrompt.ERROR_NEGATIVE_BUTTON,
        BiometricPrompt.ERROR_CANCELED -> "cancelled"
        BiometricPrompt.ERROR_LOCKOUT,
        BiometricPrompt.ERROR_LOCKOUT_PERMANENT,
        BiometricPrompt.ERROR_NO_BIOMETRICS,
        BiometricPrompt.ERROR_HW_NOT_PRESENT,
        BiometricPrompt.ERROR_HW_UNAVAILABLE,
        BiometricPrompt.ERROR_NO_DEVICE_CREDENTIAL -> "unavailable"
        else -> "failed"
    }

    /** Runs [body]; any exception becomes a rejection with [code]. */
    private fun guarded(invoke: Invoke, code: String, body: () -> Unit) {
        try {
            body()
        } catch (e: Exception) {
            invoke.reject(e.message ?: e.javaClass.simpleName, if (e is java.security.GeneralSecurityException) code else "failed")
        }
    }
}
