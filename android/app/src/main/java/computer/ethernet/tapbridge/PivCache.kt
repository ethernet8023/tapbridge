package computer.ethernet.tapbridge

import android.content.Context

/**
 * Last PIV status read from each card (keyed by stable NFC UID), so the
 * app can show a card's smart-card setup without a tap. Only a cache: every
 * setup re-reads the card and compares (Piv.setUp's `expected`), so a stale
 * entry can never cause a write.
 */
object PivCache {
    private const val PREFS = "tapbridge_piv_cache"

    data class Entry(val summary: String, val certificates: Set<Piv.Slot>, val blocked: Boolean)

    fun save(ctx: Context, uid: ByteArray, status: Piv.Status) {
        ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit()
            .putString(RegisteredKeys.hex(uid), encode(status)).apply()
    }

    fun get(ctx: Context, uid: ByteArray): Entry? =
        ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .getString(RegisteredKeys.hex(uid), null)?.let(::decode)

    /** `summary|9A,9D|blocked` (labels of slots holding certificates). */
    fun encode(status: Piv.Status): String =
        listOf(
            status.summary(),
            status.certificates.keys.joinToString(",") { it.label },
            if (status.contactlessBlocked == true) "blocked" else "ok",
        ).joinToString("|")

    fun decode(text: String): Entry? {
        val parts = text.split("|")
        if (parts.size != 3) return null
        val slots = parts[1].split(",").filter { it.isNotEmpty() }
            .map { label -> Piv.Slot.entries.firstOrNull { it.label == label } ?: return null }
        return Entry(parts[0], slots.toSet(), parts[2] == "blocked")
    }
}
