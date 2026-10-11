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
 *
 * One phone pairs MANY hosts (Track C): per-host records live in one JSON
 * pref key ([PairedHosts]).
 * Each host is an independent peer with its own identity key and pairing
 * capability and still pins exactly one phone. The phone's own identity key
 * and FCM token belong to this install and are shared by all hosts.
 */
object IdentityStore {
    private const val KS = "AndroidKeyStore"
    private const val WRAP_ALIAS = "tapbridge_wrap_key"
    private const val PREFS = "tapbridge_identity"
    private const val K_KEY = "wrapped_key"
    private const val K_IV = "wrap_iv"
    private const val K_HOSTS = "paired_hosts"
    private const val K_LAST_USED = "last_used_host"
    // Legacy single-host keys (pre multi-PC pairing); read-only after migration.
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

    // ------------------------------------------------------------- paired hosts

    /** All paired hosts, oldest-paired first (list order = display order). */
    @Synchronized
    fun pairedHosts(ctx: Context): List<PairedHosts.Record> {
        val stored = ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(K_HOSTS, null)
            ?: return emptyList()
        return PairedHosts.decode(stored)
    }

    private fun saveHosts(ctx: Context, records: List<PairedHosts.Record>) {
        check(ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .edit().putString(K_HOSTS, PairedHosts.encode(records)).commit()) {
            "paired hosts could not be saved"
        }
    }

    fun hostById(ctx: Context, hostId: String): PairedHosts.Record? =
        pairedHosts(ctx).firstOrNull { it.hostId == hostId }

    /** Record after a successful connect/Accepted; keeps pairedAt and renames. */
    fun saveAcceptedHost(ctx: Context, hostId: String, name: String, platform: Int, pivLogin: Boolean) {
        val hosts = pairedHosts(ctx)
        saveHosts(
            ctx,
            PairedHosts.upsertAfterAccept(hosts, hostId, name, System.currentTimeMillis(), platform, pivLogin),
        )
        setLastUsedHost(ctx, hostId)
    }

    /** Update the cached hint for one host; best-effort (non-connection-critical). */
    fun saveHint(ctx: Context, hostId: String, hint: String) {
        if (hint.isBlank() || !PairedHosts.validHint(hint)) return
        val hosts = pairedHosts(ctx)
        val record = hosts.firstOrNull { it.hostId == hostId } ?: return
        if (record.cachedHint == hint) return
        saveHosts(ctx, hosts.map { if (it.hostId == hostId) it.copy(cachedHint = hint) else it })
    }

    /** Cached hint for one host (HOSTID[;r=..][;d=..]); null when unknown. */
    fun cachedHint(ctx: Context, hostId: String): String? =
        hostById(ctx, hostId)?.cachedHint

    /** Per-host rename; Accepted no longer overwrites a user-chosen name. */
    fun renameHost(ctx: Context, hostId: String, name: String) {
        saveHosts(ctx, PairedHosts.withRenamed(pairedHosts(ctx), hostId, name.trim()))
    }

    /**
     * Forget ONE host ("forget this PC"). The FCM token and the phone identity
     * belong to this install, not the pairing, and stay — the next PC (or the
     * remaining ones) still reach this phone.
     */
    fun forgetHost(ctx: Context, hostId: String) {
        val prefs = ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        val hosts = pairedHosts(ctx)
        val remaining = PairedHosts.withoutHost(hosts, hostId)
        val last = lastUsedHost(ctx)
        val fallback = PairedHosts.fallbackLastUsed(hosts, hostId, last)
        check(prefs.edit()
            .putString(K_HOSTS, PairedHosts.encode(remaining))
            .putString(K_LAST_USED, fallback)
            .commit()) { "could not forget the paired PC" }
    }

    /**
     * The host the app last connected to: UI default and app-open auto-connect.
     * Null when nothing is paired. Resilient: an unknown value (state restored
     * from a partial backup) falls back to the most recently paired host.
     */
    fun lastUsedHost(ctx: Context): String? {
        val hosts = pairedHosts(ctx)
        if (hosts.isEmpty()) return null
        val saved = ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .getString(K_LAST_USED, null)
        return if (saved != null && hosts.any { it.hostId == saved }) saved
        else hosts.maxByOrNull { it.pairedAt }?.hostId
    }

    fun setLastUsedHost(ctx: Context, hostId: String) {
        ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .edit().putString(K_LAST_USED, hostId).apply()
    }

    /** Current FCM token, sent as Hello.push on the next connect. */
    fun pushToken(ctx: Context): String? =
        ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(K_PUSH_TOKEN, null)

    fun savePushToken(ctx: Context, token: String) {
        ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .edit().putString(K_PUSH_TOKEN, token).apply()
    }

    /** Phone display name shown on the PC. Default derived, editable in settings. */
    fun phoneName(ctx: Context): String = ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        .getString(K_PHONE_NAME, null) ?: defaultPhoneName(ctx)

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
