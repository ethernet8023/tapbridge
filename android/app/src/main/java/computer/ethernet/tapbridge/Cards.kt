package computer.ethernet.tapbridge

import android.content.Context

/**
 * The user's cards, kept in sync with every paired PC (wire `CardInfo`, see
 * crates/core/src/cards.rs; the merge here must match it exactly).
 *
 * Cards are keyed by stable NFC UID. Every synced field carries the Unix
 * time (ms) of its last change; merging takes the newer value field by
 * field, and equal stamps fall back to comparing values, so both sides
 * converge whatever order edits arrive in. Forgetting stamps `deletedAt`;
 * a card stays gone until it is named again.
 */
data class CardInfo(
    val uid: ByteArray,
    val label: String,
    val labelAt: Long,
    /** Open tapbridge when this card is tapped, even with the app closed. */
    val wake: Boolean,
    val wakeAt: Long,
    /** [Caps] bits. */
    val caps: Int,
    val capsAt: Long,
    /** The card's PIV identity is paired with a Mac's login. */
    val macLogin: Boolean,
    val macLoginAt: Long,
    /** When the card was forgotten (0: never). */
    val deletedAt: Long,
) {
    object Caps {
        const val FIDO = 1
        const val PIV = 2
        const val CERTS = 4
        const val BLOCKED = 8
        const val ALL = FIDO or PIV or CERTS or BLOCKED
    }

    fun has(bits: Int) = caps and bits == bits

    fun newestEdit(): Long = maxOf(labelAt, wakeAt, capsAt, macLoginAt)

    /** Forgotten, and not named again since. */
    val deleted get() = deletedAt > newestEdit()

    /** Fits the wire: UID length, label length, known caps bits. */
    val valid get() =
        uid.size in 1..Cards.MAX_UID && label.isNotEmpty() &&
            label.toByteArray(Charsets.UTF_8).size <= Cards.MAX_LABEL && caps and Caps.ALL.inv() == 0

    /** Both copies must describe the same card. Matches `CardInfo::merge`. */
    fun merge(other: CardInfo): CardInfo {
        val (label, labelAt) = newer(label to labelAt, other.label to other.labelAt, ::compareUtf8)
        val (wake, wakeAt) = newer(wake to wakeAt, other.wake to other.wakeAt) { a, b -> a.compareTo(b) }
        val (caps, capsAt) = newer(caps to capsAt, other.caps to other.capsAt) { a, b -> a.compareTo(b) }
        val (mac, macAt) = newer(macLogin to macLoginAt, other.macLogin to other.macLoginAt) { a, b -> a.compareTo(b) }
        return CardInfo(uid, label, labelAt, wake, wakeAt, caps, capsAt, mac, macAt, maxOf(deletedAt, other.deletedAt))
    }

    override fun equals(other: Any?): Boolean =
        other is CardInfo && uid.contentEquals(other.uid) && label == other.label &&
            labelAt == other.labelAt && wake == other.wake && wakeAt == other.wakeAt &&
            caps == other.caps && capsAt == other.capsAt && macLogin == other.macLogin &&
            macLoginAt == other.macLoginAt && deletedAt == other.deletedAt

    override fun hashCode(): Int = uid.contentHashCode() * 31 + labelAt.hashCode()

    companion object {
        fun new(uid: ByteArray, label: String, caps: Int, now: Long) =
            CardInfo(uid, label, now, true, now, caps, now, false, 0, 0)

        /** The value with the newer stamp; on a tie, the larger value (Rust's `Ord`). */
        private fun <T> newer(a: Pair<T, Long>, b: Pair<T, Long>, compare: (T, T) -> Int): Pair<T, Long> =
            if (a.second > b.second || (a.second == b.second && compare(a.first, b.first) >= 0)) a else b

        /** Rust compares `String`s by UTF-8 bytes, not UTF-16 units. */
        private fun compareUtf8(a: String, b: String): Int {
            val x = a.toByteArray(Charsets.UTF_8)
            val y = b.toByteArray(Charsets.UTF_8)
            for (i in 0 until minOf(x.size, y.size)) {
                val d = (x[i].toInt() and 0xFF) - (y[i].toInt() and 0xFF)
                if (d != 0) return d
            }
            return x.size - y.size
        }
    }
}

object Cards {
    /** Longest UID the wire carries (ISO 14443: 4, 7 or 10 bytes). */
    const val MAX_UID = 10
    /** Longest label, UTF-8 bytes (`tapbridge_core::cards::MAX_LABEL`). */
    const val MAX_LABEL = 64

    private const val PREFS = "tapbridge_cards"
    private const val K_CARDS = "cards"
    private const val HEADER = "tapbridge-cards 1"

    /** Told about every change: the record, and whether it came from a PC. */
    fun interface Listener {
        fun onCardChanged(card: CardInfo, fromPc: Boolean)
    }

    private val listeners = java.util.concurrent.CopyOnWriteArrayList<Listener>()
    private var cache: MutableList<CardInfo>? = null

    fun addListener(listener: Listener) { listeners += listener }
    fun removeListener(listener: Listener) { listeners -= listener }

    /** A 4-byte UID starting 0x08 is random (ISO 14443-3) and never stored. */
    fun uidStable(uid: ByteArray?): Boolean =
        uid != null && uid.isNotEmpty() && !(uid.size == 4 && uid[0] == 0x08.toByte())

    /** The UID the card frame reports: the stable one, or empty for none. */
    fun wireUid(uid: ByteArray?): ByteArray =
        uid?.takeIf { uidStable(it) && it.size <= MAX_UID }?.copyOf() ?: byteArrayOf()

    fun now(): Long = System.currentTimeMillis()

    /** Cards the user has (forgotten ones left out), oldest first. */
    @Synchronized
    fun list(ctx: Context): List<CardInfo> = load(ctx).filterNot { it.deleted }

    /** Every record, forgotten ones included: what goes to a PC. */
    @Synchronized
    fun records(ctx: Context): List<CardInfo> = load(ctx).toList()

    @Synchronized
    fun get(ctx: Context, uid: ByteArray?): CardInfo? =
        uid?.let { id -> load(ctx).firstOrNull { it.uid.contentEquals(id) && !it.deleted } }

    /** Adds (or names again) a card with "open when tapped" on. Null when unusable. */
    fun add(ctx: Context, uid: ByteArray, label: String, caps: Int): CardInfo? {
        val name = cleanLabel(label) ?: return null
        if (!uidStable(uid) || uid.size > MAX_UID) return null
        return change(ctx, uid, create = true) { old ->
            if (old == null) {
                CardInfo.new(uid.copyOf(), name, caps and CardInfo.Caps.ALL, now())
            } else {
                // Outranks the tombstone even when this clock is behind.
                val at = maxOf(now(), old.deletedAt + 1)
                val revived = old.copy(label = name, labelAt = at, wake = true, wakeAt = at)
                if (caps == 0) revived else noteCaps(revived, caps, at)
            }
        }
    }

    fun rename(ctx: Context, uid: ByteArray, label: String): CardInfo? {
        val name = cleanLabel(label) ?: return null
        return change(ctx, uid) { it?.takeIf { c -> c.label != name }?.copy(label = name, labelAt = now()) }
    }

    fun setWake(ctx: Context, uid: ByteArray, wake: Boolean): CardInfo? =
        change(ctx, uid) { it?.takeIf { c -> c.wake != wake }?.copy(wake = wake, wakeAt = now()) }

    /** Merges what a read of the card showed into its caps. */
    fun noteCaps(ctx: Context, uid: ByteArray, seen: Int): CardInfo? =
        change(ctx, uid) { old -> old?.let { noteCaps(it, seen, now()) }?.takeIf { it != old } }

    /** Replaces the caps with a full read of the card (FIDO and PIV both read). */
    fun setCaps(ctx: Context, uid: ByteArray, caps: Int): CardInfo? =
        change(ctx, uid) { it?.takeIf { c -> c.caps != caps }?.copy(caps = caps, capsAt = now()) }

    fun forget(ctx: Context, uid: ByteArray): CardInfo? =
        change(ctx, uid) { it?.copy(deletedAt = maxOf(now(), it.newestEdit() + 1)) }

    /**
     * Merges a PC's copy. Returns the merged record when it differs from
     * the PC's (send it back), else null.
     */
    fun merge(ctx: Context, theirs: CardInfo): CardInfo? {
        if (!theirs.valid || !uidStable(theirs.uid)) return null
        var merged: CardInfo? = null
        change(ctx, theirs.uid, create = true, fromPc = true, includeDeleted = true) { old ->
            (old?.merge(theirs) ?: theirs).also { merged = it }.takeIf { it != old }
        }
        return merged?.takeIf { it != theirs }
    }

    private fun noteCaps(card: CardInfo, seen: Int, at: Long): CardInfo {
        var caps = card.caps or (seen and CardInfo.Caps.ALL)
        // Readable now: the applet was reinstalled without the block.
        if (seen and CardInfo.Caps.CERTS != 0) caps = caps and CardInfo.Caps.BLOCKED.inv()
        if (caps and (CardInfo.Caps.CERTS or CardInfo.Caps.BLOCKED) != 0) caps = caps or CardInfo.Caps.PIV
        return if (caps == card.caps) card else card.copy(caps = caps, capsAt = at)
    }

    /**
     * Applies [edit] to the card's record (null when absent; forgotten ones
     * count as absent unless [includeDeleted]); a non-null result is saved
     * and announced.
     */
    private fun change(
        ctx: Context,
        uid: ByteArray,
        create: Boolean = false,
        fromPc: Boolean = false,
        includeDeleted: Boolean = false,
        edit: (CardInfo?) -> CardInfo?,
    ): CardInfo? {
        val updated = synchronized(this) {
            val cards = load(ctx)
            val index = cards.indexOfFirst { it.uid.contentEquals(uid) }
            val old = cards.getOrNull(index)
            val visible = old?.takeIf { includeDeleted || create || !it.deleted }
            if (old != null && visible == null) return null
            if (old == null && !create) return null
            val next = edit(visible) ?: return null
            if (index >= 0) cards[index] = next else cards += next
            save(ctx, cards)
            next
        }
        listeners.forEach { it.onCardChanged(updated, fromPc) }
        return updated
    }

    /** One line, trimmed, at most [MAX_LABEL] UTF-8 bytes. Null when empty. */
    fun cleanLabel(label: String): String? {
        val line = label.map { if (it.isISOControl()) ' ' else it }.joinToString("").trim()
        val out = StringBuilder()
        var bytes = 0
        var i = 0
        while (i < line.length) {
            val cp = line.codePointAt(i)
            val size = String(Character.toChars(cp)).toByteArray(Charsets.UTF_8).size
            if (bytes + size > MAX_LABEL) break
            out.appendCodePoint(cp)
            bytes += size
            i += Character.charCount(cp)
        }
        return out.toString().trimEnd().takeIf { it.isNotEmpty() }
    }

    private const val K_SEEN_MAC = "seen_mac"

    /** This phone has connected to a Mac: Mac login is worth showing. */
    fun seenMac(ctx: Context): Boolean =
        ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getBoolean(K_SEEN_MAC, false)

    fun markSeenMac(ctx: Context) {
        if (!seenMac(ctx)) ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit().putBoolean(K_SEEN_MAC, true).apply()
    }

    /** The first free "Card N". */
    fun defaultLabel(ctx: Context): String {
        val taken = list(ctx).map { it.label }.toSet()
        return generateSequence(1) { it + 1 }.map { "Card $it" }.first { it !in taken }
    }

    private fun load(ctx: Context): MutableList<CardInfo> =
        cache ?: decode(
            ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(K_CARDS, null),
        ).toMutableList().also { cache = it }

    private fun save(ctx: Context, cards: List<CardInfo>) {
        check(
            ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
                .edit().putString(K_CARDS, encode(cards)).commit(),
        ) { "cards could not be saved" }
    }

    /** Header line, then `UID|LABEL_AT|WAKE|WAKE_AT|CAPS|CAPS_AT|MAC|MAC_AT|DELETED_AT|LABEL`. */
    fun encode(cards: List<CardInfo>): String = buildString {
        append(HEADER)
        for (c in cards) {
            append('\n')
            append(
                listOf(
                    hex(c.uid), c.labelAt, if (c.wake) 1 else 0, c.wakeAt, c.caps, c.capsAt,
                    if (c.macLogin) 1 else 0, c.macLoginAt, c.deletedAt, c.label,
                ).joinToString("|"),
            )
        }
    }

    /** Anything but the current format reads as no cards. */
    fun decode(text: String?): List<CardInfo> {
        val lines = text?.split('\n') ?: return emptyList()
        if (lines.firstOrNull() != HEADER) return emptyList()
        val out = ArrayList<CardInfo>()
        for (line in lines.drop(1)) {
            val p = line.split('|', limit = 10)
            if (p.size != 10) continue
            val card = runCatching {
                CardInfo(
                    uid = hexBytes(p[0]) ?: return@runCatching null,
                    labelAt = p[1].toLong(),
                    wake = flag(p[2]) ?: return@runCatching null,
                    wakeAt = p[3].toLong(),
                    caps = p[4].toInt(),
                    capsAt = p[5].toLong(),
                    macLogin = flag(p[6]) ?: return@runCatching null,
                    macLoginAt = p[7].toLong(),
                    deletedAt = p[8].toLong(),
                    label = p[9],
                )
            }.getOrNull() ?: continue
            if (card.valid && uidStable(card.uid) && out.none { it.uid.contentEquals(card.uid) }) out += card
        }
        return out
    }

    private fun flag(text: String) = when (text) { "0" -> false; "1" -> true; else -> null }

    fun hex(bytes: ByteArray): String = bytes.joinToString("") { "%02x".format(it) }

    fun hexBytes(text: String): ByteArray? {
        if (text.length % 2 != 0) return null
        return try {
            ByteArray(text.length / 2) { i -> text.substring(i * 2, i * 2 + 2).toInt(16).toByte() }
        } catch (_: NumberFormatException) {
            null
        }
    }

    /** Tests only: forget the in-memory copy. */
    @Synchronized
    internal fun resetCache() { cache = null }
}
