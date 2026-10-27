package computer.ethernet.tapbridge

/**
 * Holds a pending invitation between the UI and the service. Parsed loosely
 * on the Kotlin side (fixed prefix layout) so we can persist the host
 * candidate; the Rust side validates authority and the full contents.
 *
 * Format: tapbridge:1:<64 hex host>:<64 hex capability>
 */
object Invitation {
    private const val PREFIX = "tapbridge:1:"
    private const val HEX = 64

    data class Parts(val host: String, val capability: String)

    fun parse(text: String): Parts? {
        val s = text.trim()
        if (!s.startsWith(PREFIX)) return null
        val parts = s.removePrefix(PREFIX).split(":")
        if (parts.size != 2) return null
        val (host, cap) = parts
        if (host.length != HEX || cap.length != HEX) return null
        if (!host.all { it.isDigit() || it in 'a'..'f' || it in 'A'..'F' }) return null
        if (!cap.all { it.isDigit() || it in 'a'..'f' || it in 'A'..'F' }) return null
        return Parts(host, cap)
    }
}

/**
 * Process-wide pending invitation. Set by the UI, consumed on connect. The
 * connect worker moves it into the service's [PairingFlow.Pairing], which
 * keeps it only while that pairing retries (or until "Try again"/Cancel
 * after a failure) and drops it once the PC accepts the phone.
 */
object PairingState {
    private val slot = arrayOfNulls<String>(1)

    fun setPendingInvitation(invitation: String) {
        synchronized(slot) { slot[0] = invitation }
    }

    fun consumePendingInvitation(): String = synchronized(slot) {
        val v = slot[0] ?: return ""
        slot[0] = null
        v
    }

    fun clear() {
        synchronized(slot) { slot[0] = null }
    }

    /** A scanned invitation is waiting for the connect worker. */
    fun hasPending(): Boolean = synchronized(slot) { slot[0] != null }
}
