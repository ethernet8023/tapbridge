package computer.ethernet.tapbridge

import computer.ethernet.tapbridge.TapBridgeService.Status

/**
 * A QR pairing from scan to result, shown on the pairing screen until the
 * PC really accepts the phone (or the attempt gives up).
 *
 * The service keeps the invitation while [Phase.PAIRING] so quiet retries
 * can redial the scanned PC for [WINDOW_MS]; after that the attempt is
 * [Phase.FAILED] and the user decides (Try again / Cancel). The invitation
 * is dropped once the pairing succeeds or is cancelled, and never leaves the
 * service: the UI only sees [View]. Pure functions, no Android.
 */
object PairingFlow {
    /** How long a pairing retries quietly before it shows the error. */
    const val WINDOW_MS = 30_000L

    enum class Phase { PAIRING, PAIRED, FAILED }

    data class Pairing(
        /** The scanned PC (the invitation's host id). */
        val hostId: String,
        /** The secret-bearing invitation; null once it's no longer needed. */
        val invitation: String?,
        val startedAt: Long,
        val phase: Phase = Phase.PAIRING,
        /** The latest connect error (shown on failure, or as "still trying"). */
        val error: String? = null,
        /** The PC's name from Accepted, once paired. */
        val hostName: String? = null,
    ) {
        /** What the UI may see: everything but the invitation. */
        val view get() = View(hostId, phase, error, hostName)

        /** The invitation to dial with: only while still pairing. */
        val inviteToUse get() = invitation.takeIf { phase == Phase.PAIRING }

        override fun toString() = "Pairing(host=$hostId, phase=$phase, error=$error)"
    }

    data class View(val hostId: String, val phase: Phase, val error: String?, val hostName: String?)

    fun start(hostId: String, invitation: String, now: Long) = Pairing(hostId, invitation, now)

    /** A connect finished with [acceptedHost]: done if that's the scanned PC. */
    fun accepted(pairing: Pairing?, acceptedHost: String, hostName: String): Pairing? = when {
        pairing == null -> null
        pairing.hostId != acceptedHost || pairing.phase == Phase.PAIRED -> pairing
        // Success wins even after the window ran out mid-attempt.
        else -> pairing.copy(phase = Phase.PAIRED, invitation = null, error = null, hostName = hostName)
    }

    /** A connect attempt failed; the window decides when to give up. */
    fun failedAttempt(pairing: Pairing?, error: String): Pairing? =
        if (pairing?.phase == Phase.PAIRING) pairing.copy(error = error) else pairing

    /** The quiet-retry window ran out without success. */
    fun timedOut(pairing: Pairing?): Pairing? =
        if (pairing?.phase == Phase.PAIRING) {
            pairing.copy(phase = Phase.FAILED, error = pairing.error ?: "Couldn't reach the PC.")
        } else pairing

    /** "Try again" after a failure: a fresh window with the same invitation. */
    fun retry(pairing: Pairing?, now: Long): Pairing? =
        if (pairing?.phase == Phase.FAILED && pairing.invitation != null) {
            pairing.copy(phase = Phase.PAIRING, startedAt = now, error = null)
        } else pairing

    /** The connect is for the scanned PC and must replace a live session elsewhere. */
    fun needsDial(pairing: Pairing?, connectedHost: String?): Boolean =
        pairing?.phase == Phase.PAIRING && pairing.hostId != connectedHost

    // ----------------------------------------------------------- home list

    /** One paired PC's state on the home screen. */
    enum class PcState { CONNECTED, CONNECTING, ERROR, STANDBY }

    /**
     * The phone holds one session at a time: the PC it is connected to (or
     * dialing) shows that, every other paired PC is on standby until it asks
     * for a key (wake) or the user picks it.
     */
    fun pcState(hostId: String, status: Status, connectedHost: String?, dialingHost: String?): PcState = when {
        status == Status.CONNECTED && hostId == connectedHost -> PcState.CONNECTED
        hostId == dialingHost && status == Status.CONNECTING -> PcState.CONNECTING
        hostId == dialingHost && status == Status.ERROR -> PcState.ERROR
        else -> PcState.STANDBY
    }
}
