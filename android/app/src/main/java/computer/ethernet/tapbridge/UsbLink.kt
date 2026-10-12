package computer.ethernet.tapbridge

/**
 * Pure decisions for the USB (AOA) link; see docs/USB-PLAN.md. The PC's
 * identity comes from the signed USB handshake (NativeBridge.usbOpen), so
 * a host id here is verified, unlike a wake hint.
 */
object UsbLink {
    /** tapbridge_core::usb::aoa; res/xml/accessory_filter.xml matches these. */
    const val MANUFACTURER = "tapbridge"
    const val MODEL = "tapbridge"
    const val VERSION = "1"

    /** Our accessory (the PC running tapbridge), not some other AOA host. */
    fun isOurs(manufacturer: String?, model: String?): Boolean =
        manufacturer == MANUFACTURER && model == MODEL

    sealed interface Decision {
        /** A paired PC: Hello without capability (pinned reconnect). [hostId] is the stored spelling. */
        data class Reconnect(val hostId: String) : Decision
        /** Unknown PC with its pairing window open: ask the user first. */
        data class AskToPair(val hostId: String, val hostName: String) : Decision
        /** Unknown PC, pairing window closed: nothing to do but tell the user. */
        data class OpenPairingFirst(val hostName: String) : Decision
    }

    /** What to do after the handshake with [hostId] (64 hex, verified). */
    fun decide(pairedHostIds: Collection<String>, hostId: String, hostName: String, pairingOpen: Boolean): Decision {
        val stored = pairedHostIds.firstOrNull { it.equals(hostId, ignoreCase = true) }
        return when {
            stored != null -> Decision.Reconnect(stored)
            pairingOpen -> Decision.AskToPair(hostId, hostName)
            else -> Decision.OpenPairingFirst(hostName)
        }
    }

    /**
     * Hint to cache after a USB accept: the bare host id, so network wakes
     * and reconnects can find the PC later through discovery. A richer hint
     * a network session already cached is kept (null = leave it).
     */
    fun hintToSave(existing: String?, hostId: String): String? =
        if (existing.isNullOrBlank()) hostId else null

    fun openPairingFirstMessage(hostName: String) =
        "Open Pair phone on $hostName first, then plug the cable in again."
}
