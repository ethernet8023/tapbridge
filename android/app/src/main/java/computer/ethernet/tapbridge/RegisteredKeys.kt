package computer.ethernet.tapbridge

import android.content.Context

/**
 * Registered keys (DESIGN "Registered keys and the tap gate"). A record stores:
 * label, NFC UID if stable (a 4-byte UID starting 0x08 is random and is NOT
 * stored), and the CTAP2 authenticatorGetInfo AAGUID when obtainable.
 *
 * Match = stable UID equal, otherwise AAGUID equal. Stored in app-private
 * SharedPreferences (no backup, see dataExtractionRules).
 */
object RegisteredKeys {
    data class KeyRecord(
        val id: String,
        val label: String,
        /** Stable NFC UID, or null when the card uses a random UID. */
        val uid: ByteArray?,
        /** CTAP2 getInfo AAGUID, or null when unknown. */
        val aaguid: ByteArray?,
    ) {
        override fun equals(other: Any?): Boolean =
            other is KeyRecord && other.id == id && other.label == label &&
                other.uid.contentEquals(uid) && other.aaguid.contentEquals(aaguid)

        override fun hashCode(): Int = id.hashCode() * 31 + label.hashCode()
    }

    private const val PREFS = "tapbridge_identity"
    private const val K_KEYS = "registered_keys"

    fun list(ctx: Context): List<KeyRecord> = decode(
        ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(K_KEYS, null)
    )

    fun upsert(ctx: Context, record: KeyRecord) {
        val current = list(ctx).filterNot { it.id == record.id } + record
        save(ctx, current)
    }

    fun remove(ctx: Context, id: String) {
        save(ctx, list(ctx).filterNot { it.id == id })
    }

    private fun save(ctx: Context, records: List<KeyRecord>) {
        // Plain-text line format (no JSON): id|label|uidHex|aaguidHex.
        // Hex fields are empty when absent; label never contains '|'.
        val text = records.joinToString("\n") { record ->
            listOf(
                record.id,
                record.label.replace("|", " "),
                record.uid?.let { hex(it) } ?: "",
                record.aaguid?.let { hex(it) } ?: "",
            ).joinToString("|")
        }
        check(ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .edit().putString(K_KEYS, text).commit()) {
            "registered keys could not be saved"
        }
    }

    fun decode(text: String?): List<KeyRecord> {
        if (text.isNullOrEmpty()) return emptyList()
        return text.split("\n").mapNotNull { line ->
            if (line.isBlank()) return@mapNotNull null
            val parts = line.split("|")
            if (parts.size != 4 || parts[0].isEmpty()) return@mapNotNull null
            KeyRecord(
                id = parts[0],
                label = parts[1],
                uid = if (parts[2].isEmpty()) null else hexBytes(parts[2]),
                aaguid = if (parts[3].isEmpty()) null else hexBytes(parts[3]),
            )
        }
    }

    /** A 4-byte UID starting 0x08 is random (ISO 14443-3 cascade) and never stored. */
    fun uidStable(uid: ByteArray): Boolean =
        uid.isNotEmpty() && !(uid.size == 4 && uid[0] == 0x08.toByte())

    /** Longest UID the card frame carries (ISO 14443: 4, 7 or 10 bytes). */
    const val MAX_WIRE_UID = 10

    /** The UID the card frame reports: the stable one, or empty for none. */
    fun wireUid(uid: ByteArray?): ByteArray =
        uid?.takeIf { uidStable(it) && it.size <= MAX_WIRE_UID }?.copyOf() ?: byteArrayOf()

    /**
     * Gate matching: a discovered (uid, aaguid) pair matches a registered
     * record when the stable UID is equal; otherwise when the AAGUID is equal.
     */
    fun matches(record: KeyRecord, uid: ByteArray?, aaguid: ByteArray?): Boolean {
        if (uid != null && record.uid != null && record.uid.contentEquals(uid)) return true
        if (aaguid != null && record.aaguid != null && record.aaguid.contentEquals(aaguid)) return true
        return false
    }

    fun anyMatch(records: List<KeyRecord>, uid: ByteArray?, aaguid: ByteArray?): KeyRecord? =
        records.firstOrNull { matches(it, uid, aaguid) }

    fun hex(bytes: ByteArray): String =
        bytes.joinToString("") { "%02x".format(it) }

    fun hexBytes(text: String): ByteArray? {
        if (text.length % 2 != 0) return null
        return try {
            ByteArray(text.length / 2) { i ->
                text.substring(i * 2, i * 2 + 2).toInt(16).toByte()
            }
        } catch (_: NumberFormatException) {
            null
        }
    }
}
