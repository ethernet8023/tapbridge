package computer.ethernet.tapbridge

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Identity persistence. The 32-byte pairing key never exists in plaintext on
 * disk: it is wrapped with an AndroidKeyStore AES/GCM key. The wrapping key is
 * generated on first use and is hardware-backed on supported devices.
 *
 * Keystore failures propagate (fail loud) — we never silently rekey, because
 * that would orphan the pairing on the host side.
 */
object IdentityStore {
    private const val KS = "AndroidKeyStore"
    private const val WRAP_ALIAS = "tapbridge_wrap_key"
    private const val PREFS = "tapbridge_identity"
    private const val K_KEY = "wrapped_key"
    private const val K_IV = "wrap_iv"
    private const val K_HOST = "paired_host"
    private const val K_HINT = "cached_hint"
    private const val K_HOST_NAME = "host_name"
    private const val K_PUSH_TOKEN = "push_token"
    private const val K_PHONE_NAME = "phone_name"
    private const val KEY_LEN = 32
    private const val GCM_TAG_BITS = 128

    @Synchronized
    fun getOrCreateKey(ctx: Context): ByteArray {
        val prefs = ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        val stored = prefs.getString(K_KEY, null)
        val iv = prefs.getString(K_IV, null)
        check((stored == null) == (iv == null)) { "incomplete saved identity; restore storage, do not re-key" }
        if (stored != null && iv != null) {
            return unwrap(decode(stored), decode(iv))
        }

        // Fresh identity: generate once, wrap, persist. Only called before pairing.
        val key = NativeBridge.generateKey()
        require(key.size == KEY_LEN) { "native key generator returned ${key.size} bytes" }
        val (ct, newIv) = wrap(key)
        check(prefs.edit()
            .putString(K_KEY, encode(ct))
            .putString(K_IV, encode(newIv))
            .commit()) { "identity could not be saved" }
        return key
    }

    fun pairedHost(ctx: Context): String? =
        ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(K_HOST, null)

    /** Called only after the host has accepted the connection. */
    fun saveHost(ctx: Context, host: String) {
        check(ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .edit().putString(K_HOST, host).commit()) { "host pairing could not be saved" }
    }

    fun forgetHost(ctx: Context) {
        check(ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .edit()
            .remove(K_HOST)
            .remove(K_HINT)
            .remove(K_HOST_NAME)
            // The FCM token belongs to this install, not to the pairing: keep
            // it, or the next PC could never wake this phone.
            .commit()) { "could not forget the paired PC" }
    }

    /**
     * Last wake hint known for the paired host (HOSTID[;r=..][;d=..]).
     * Cached from every FCM wake and every successful dial so later
     * reconnects skip discovery. A bare host id is still a valid hint.
     */
    fun cachedHint(ctx: Context): String? =
        ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(K_HINT, null)

    /** Update the cached hint; best-effort (non-connection-critical). */
    fun saveHint(ctx: Context, hint: String) {
        if (hint.isBlank()) return
        ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .edit().putString(K_HINT, hint).apply()
    }

    /** Host display name as reported in Accepted { name }. */
    fun hostName(ctx: Context): String? =
        ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(K_HOST_NAME, null)

    fun saveHostName(ctx: Context, name: String) {
        ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .edit().putString(K_HOST_NAME, name).apply()
    }

    /** Current FCM token, sent as Hello.push on the next connect. */
    fun pushToken(ctx: Context): String? =
        ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(K_PUSH_TOKEN, null)

    fun savePushToken(ctx: Context, token: String) {
        ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .edit().putString(K_PUSH_TOKEN, token).apply()
    }

    /** Phone display name shown on the PC. Default derived, editable in settings. */
    fun phoneName(ctx: Context): String =
        ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(K_PHONE_NAME, null)
            ?: defaultPhoneName(ctx)

    fun setPhoneName(ctx: Context, name: String) {
        val trimmed = name.trim()
        require(trimmed.isNotEmpty() && trimmed.toByteArray(Charsets.UTF_8).size <= 64) {
            "phone name must be 1..=64 UTF-8 bytes"
        }
        check(ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .edit().putString(K_PHONE_NAME, trimmed).commit()) { "could not save phone name" }
    }

    fun defaultPhoneName(ctx: Context): String {
        val settings = android.provider.Settings.Global.getString(
            ctx.contentResolver,
            android.provider.Settings.Global.DEVICE_NAME,
        )
        val name = (settings ?: android.os.Build.MODEL).trim()
        return if (name.isEmpty()) "Android phone" else name
    }

    private fun wrapKey(): SecretKey {
        val ks = KeyStore.getInstance(KS).apply { load(null) }
        (ks.getKey(WRAP_ALIAS, null) as? SecretKey)?.let { return it }
        val gen = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, KS)
        gen.init(
            KeyGenParameterSpec.Builder(
                WRAP_ALIAS,
                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
            )
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build()
        )
        return gen.generateKey()
    }

    private fun wrap(plain: ByteArray): Pair<ByteArray, ByteArray> {
        val c = Cipher.getInstance("AES/GCM/NoPadding")
        c.init(Cipher.ENCRYPT_MODE, wrapKey())
        return Pair(c.doFinal(plain), c.iv)
    }

    private fun unwrap(ct: ByteArray, iv: ByteArray): ByteArray {
        val c = Cipher.getInstance("AES/GCM/NoPadding")
        c.init(Cipher.DECRYPT_MODE, wrapKey(), GCMParameterSpec(GCM_TAG_BITS, iv))
        return c.doFinal(ct)
    }

    private fun encode(b: ByteArray): String =
        android.util.Base64.encodeToString(b, android.util.Base64.NO_WRAP)

    private fun decode(s: String): ByteArray =
        android.util.Base64.decode(s, android.util.Base64.NO_WRAP)
}
