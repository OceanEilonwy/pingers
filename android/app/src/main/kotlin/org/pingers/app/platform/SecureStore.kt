package org.pingers.app.platform

import android.content.Context
import android.content.SharedPreferences
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.security.keystore.StrongBoxUnavailableException
import android.util.Base64
import java.security.GeneralSecurityException
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/** Supplies the AES key that wraps secure-store values. */
fun interface WrappingKey {
    @Throws(GeneralSecurityException::class)
    fun get(): SecretKey
}

/**
 * An AES-256-GCM key in the Android Keystore, made on first use: in StrongBox when the phone
 * has one, otherwise in the TEE. It never leaves the Keystore. It needs no user authentication
 * and no unlocked screen, because background wakes open the database with the phone locked.
 */
class KeystoreWrappingKey(private val alias: String = ALIAS) : WrappingKey {
    override fun get(): SecretKey {
        val store = KeyStore.getInstance(PROVIDER).apply { load(null) }
        (store.getKey(alias, null) as? SecretKey)?.let { return it }
        return try {
            generate(strongBox = true)
        } catch (_: StrongBoxUnavailableException) {
            generate(strongBox = false)
        }
    }

    private fun generate(strongBox: Boolean): SecretKey {
        val spec = KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setKeySize(256)
            .setRandomizedEncryptionRequired(true)
            .apply { if (strongBox) setIsStrongBoxBacked(true) }
            .build()
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, PROVIDER).run {
            init(spec)
            generateKey()
        }
    }

    companion object {
        const val PROVIDER = "AndroidKeyStore"
        const val ALIAS = "pingers.secure-store"
    }
}

/**
 * The core's secure store: each value (the 32-byte SQLCipher key among them) is kept only
 * wrapped, AES-GCM under [wrappingKey], in app-private preferences that are never backed up.
 * Stored as base64 of `iv (12 B) || ciphertext || tag`; the key's name is the GCM associated
 * data, so a wrapped value can't be moved to another name.
 */
class SecureStore(
    private val prefs: SharedPreferences,
    private val wrappingKey: WrappingKey,
) {
    constructor(context: Context) : this(
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE),
        KeystoreWrappingKey(),
    )

    @Throws(PlatformException::class)
    fun get(key: SecureStoreKey): ByteArray? {
        val stored = prefs.getString(key.name, null) ?: return null
        return guarded {
            val bytes = Base64.decode(stored, Base64.NO_WRAP)
            if (bytes.size < IV_BYTES + TAG_BITS / 8) throw GeneralSecurityException("truncated")
            val cipher = Cipher.getInstance(TRANSFORMATION)
            cipher.init(Cipher.DECRYPT_MODE, wrappingKey.get(), GCMParameterSpec(TAG_BITS, bytes, 0, IV_BYTES))
            cipher.updateAAD(key.name.toByteArray())
            cipher.doFinal(bytes, IV_BYTES, bytes.size - IV_BYTES)
        }
    }

    @Throws(PlatformException::class)
    fun put(key: SecureStoreKey, value: ByteArray) {
        val wrapped = guarded {
            val cipher = Cipher.getInstance(TRANSFORMATION)
            // The Keystore picks the IV (randomised encryption is required).
            cipher.init(Cipher.ENCRYPT_MODE, wrappingKey.get())
            cipher.updateAAD(key.name.toByteArray())
            val sealed = cipher.doFinal(value)
            val iv = cipher.iv
            check(iv.size == IV_BYTES)
            iv + sealed
        }
        // commit, not apply: the core may rely on the key being on disk before it creates the database.
        @Suppress("UseKtx") // The KTX edit hides commit's result.
        if (!prefs.edit().putString(key.name, Base64.encodeToString(wrapped, Base64.NO_WRAP)).commit()) {
            throw PlatformException(PlatformError.SECURE_STORE_UNAVAILABLE)
        }
    }

    private inline fun <T> guarded(block: () -> T): T = try {
        block()
    } catch (e: GeneralSecurityException) {
        throw PlatformException(PlatformError.SECURE_STORE_UNAVAILABLE, e)
    } catch (e: IllegalArgumentException) {
        throw PlatformException(PlatformError.SECURE_STORE_UNAVAILABLE, e)
    } catch (e: IllegalStateException) {
        throw PlatformException(PlatformError.SECURE_STORE_UNAVAILABLE, e)
    }

    companion object {
        const val PREFS = "pingers.secure-store"
        private const val TRANSFORMATION = "AES/GCM/NoPadding"
        private const val IV_BYTES = 12
        private const val TAG_BITS = 128
    }
}
