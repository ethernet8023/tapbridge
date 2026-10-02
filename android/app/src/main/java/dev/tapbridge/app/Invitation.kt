package dev.tapbridge.app

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
 * secret-bearing text is cleared as soon as the connect worker takes it
 * (single consume); it never lingers past one connection attempt.
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
}
