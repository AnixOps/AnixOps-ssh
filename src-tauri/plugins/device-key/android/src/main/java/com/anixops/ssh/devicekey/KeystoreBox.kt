package com.anixops.ssh.devicekey

import android.content.Context
import android.content.SharedPreferences
import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Secrets wrapped by AES-256-GCM keys that live in the Android Keystore.
 *
 * Two kinds of key, two kinds of item:
 *
 * - the wrapping key ([WRAP_ALIAS]) protects the vault's device key (or, with
 *   a vault passphrase, its passphrase-wrapped form). It is hardware-backed
 *   (StrongBox when the phone has it, else the TEE) and not exportable: the
 *   app can ask the Keystore to encrypt and decrypt, never to hand the key
 *   over. Only the ciphertext reaches app storage.
 * - the biometric key ([BIO_ALIAS]) is created with user authentication
 *   required on every use and invalidated by new biometric enrolment. The
 *   Keystore refuses to decrypt with it until a BiometricPrompt bound to that
 *   very operation (a CryptoObject) has matched, so the copy it protects
 *   cannot be released by code that skips the prompt.
 *
 * The plaintext does pass through this process while the vault is open; it
 * is overwritten with zeros as soon as it has been handed on.
 */
internal class KeystoreBox(context: Context) {
    private val prefs: SharedPreferences =
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    private fun keyStore(): KeyStore = KeyStore.getInstance(PROVIDER).apply { load(null) }

    // ---- the plain wrapping key -----------------------------------------

    private fun wrapKey(): SecretKey =
        (keyStore().getKey(WRAP_ALIAS, null) as? SecretKey) ?: generate(WRAP_ALIAS, false)

    fun get(service: String, account: String): ByteArray? {
        val stored = prefs.getString(itemName(service, account), null) ?: return null
        val blob = Base64.decode(stored, Base64.NO_WRAP)
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.DECRYPT_MODE, wrapKey(), GCMParameterSpec(TAG_BITS, blob.copyOfRange(0, IV_BYTES)))
        cipher.updateAAD(aad(service, account))
        return cipher.doFinal(blob, IV_BYTES, blob.size - IV_BYTES)
    }

    fun set(service: String, account: String, value: ByteArray) {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, wrapKey())
        cipher.updateAAD(aad(service, account))
        val blob = cipher.iv + cipher.doFinal(value)
        // commit(), not apply(): the caller is told "stored" only when it is.
        check(prefs.edit().putString(itemName(service, account), Base64.encodeToString(blob, Base64.NO_WRAP)).commit()) {
            "the secret could not be written"
        }
    }

    fun delete(service: String, account: String) {
        prefs.edit().remove(itemName(service, account)).commit()
    }

    // ---- the biometric-bound key ----------------------------------------

    fun hasBoundBlob(account: String): Boolean = prefs.contains(boundName(account))

    fun boundBlob(account: String): ByteArray? =
        prefs.getString(boundName(account), null)?.let { Base64.decode(it, Base64.NO_WRAP) }

    fun storeBoundBlob(account: String, blob: ByteArray) {
        check(prefs.edit().putString(boundName(account), Base64.encodeToString(blob, Base64.NO_WRAP)).commit()) {
            "the secret could not be written"
        }
    }

    /** The biometric key, or null when it is gone (new enrolment, removed lock screen). */
    fun boundKeyOrNull(): SecretKey? = try {
        keyStore().getKey(BIO_ALIAS, null) as? SecretKey
    } catch (e: java.security.UnrecoverableKeyException) {
        null
    }

    /** A fresh biometric key; any earlier one (and with it any earlier copy) is replaced. */
    fun newBoundKey(): SecretKey {
        keyStore().deleteEntry(BIO_ALIAS)
        return generate(BIO_ALIAS, true)
    }

    fun removeBound(account: String) {
        prefs.edit().remove(boundName(account)).commit()
        keyStore().deleteEntry(BIO_ALIAS)
    }

    // ---- key generation ---------------------------------------------------

    private fun generate(alias: String, authenticationRequired: Boolean): SecretKey {
        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, PROVIDER)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            try {
                generator.init(spec(alias, authenticationRequired, true))
                return generator.generateKey()
            } catch (e: Exception) {
                // No StrongBox on this phone (StrongBoxUnavailableException) or
                // not for these parameters: the TEE is the next best thing.
            }
        }
        generator.init(spec(alias, authenticationRequired, false))
        return generator.generateKey()
    }

    private fun spec(alias: String, authenticationRequired: Boolean, strongBox: Boolean): KeyGenParameterSpec {
        val builder = KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setKeySize(256)
        if (authenticationRequired) {
            builder.setUserAuthenticationRequired(true)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                // Timeout 0: every single use needs a fresh biometric match.
                builder.setUserAuthenticationParameters(0, KeyProperties.AUTH_BIOMETRIC_STRONG)
            } else {
                @Suppress("DEPRECATION")
                builder.setUserAuthenticationValidityDurationSeconds(-1)
            }
            // A new fingerprint or face destroys the key: someone who can add
            // their own biometrics to a stolen, unlocked phone gets nothing.
            builder.setInvalidatedByBiometricEnrollment(true)
        }
        if (strongBox && Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            builder.setIsStrongBoxBacked(true)
        }
        return builder.build()
    }

    companion object {
        const val PROVIDER = "AndroidKeyStore"
        const val WRAP_ALIAS = "anixops.devicekey.wrap.v1"
        const val BIO_ALIAS = "anixops.devicekey.bio.v1"
        const val TRANSFORMATION = "AES/GCM/NoPadding"
        const val TAG_BITS = 128
        const val IV_BYTES = 12
        private const val PREFS = "anixops_device_keys"

        private fun itemName(service: String, account: String) = "item/$service/$account"
        private fun boundName(account: String) = "bound/$account"

        /** Binds an item to its slot so a blob cannot be moved to another name. */
        fun aad(service: String, account: String): ByteArray = "$service\u0000$account".toByteArray(Charsets.UTF_8)
    }
}
