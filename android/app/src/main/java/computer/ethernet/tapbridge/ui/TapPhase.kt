package computer.ethernet.tapbridge.ui

import computer.ethernet.tapbridge.AuthState
import computer.ethernet.tapbridge.TapBridgeService.Status

/** Phases of the tap hero. Each is something the phone actually knows:
 *  card presence from the NFC controller, sign-in state from the PC's
 *  Auth frames, connection state from the session. */
enum class TapPhase {
    /** Paired, nothing asked of the user: art at rest, no motion. */
    IDLE,
    /** A PC asked for a tap and no card is on the phone yet: the key drifts
     *  toward the phone, inviting a tap. Only during an active request. */
    WAITING,
    /** Card on the phone but the session is not up yet. */
    DETECTED,
    /** Card on the phone and the PC is talking to it. */
    ACTIVE,
    /** The PC reports the sign-in or registration succeeded. */
    SUCCESS,
    /** The card left before the sign-in finished; the PC waits for a re-tap. */
    REMOVED,
    /** The PC shows a PIN prompt; the PIN is typed on the PC. */
    PIN,
    /** The PC reports the sign-in failed. */
    FAILED,
    /** Connection error (not a sign-in failure). */
    ERROR;

    /** The card art sits on the phone only while a card really is there.
     *  A connection error lifts it whatever the presence. */
    fun docks(cardPresent: Boolean): Boolean = when (this) {
        IDLE, WAITING, REMOVED, ERROR -> false
        DETECTED, ACTIVE, SUCCESS, PIN, FAILED -> cardPresent
    }

    /** Motion plays only while something is being asked or reported;
     *  a merely connected phone rests. */
    val animates: Boolean get() = this != IDLE
}

/**
 * UiState to TapPhase. Precedence, highest first:
 *  1. A connection error: nothing else is trustworthy.
 *  2. No live session: auth states are stale, so presence and the tap
 *     request alone decide (the pre-auth behaviour).
 *  3. Results the PC reported (SUCCESS, FAILED) and its PIN prompt, which
 *     hold whatever the card does meanwhile.
 *  4. Card presence: a card on the phone is ACTIVE, even if the PC still
 *     says REMOVED (the re-tap landed; WORKING follows).
 *  5. REMOVED, then a pending request (WORKING without a card is a request
 *     too: the PC wants the card back), then IDLE.
 */
fun tapPhase(status: Status, cardPresent: Boolean, tapRequested: Boolean, auth: AuthState): TapPhase = when {
    status == Status.ERROR -> TapPhase.ERROR
    status != Status.CONNECTED -> when {
        cardPresent -> TapPhase.DETECTED
        tapRequested -> TapPhase.WAITING
        else -> TapPhase.IDLE
    }
    auth == AuthState.SUCCESS -> TapPhase.SUCCESS
    auth == AuthState.FAILED -> TapPhase.FAILED
    auth == AuthState.PIN -> TapPhase.PIN
    cardPresent -> TapPhase.ACTIVE
    auth == AuthState.REMOVED -> TapPhase.REMOVED
    tapRequested || auth == AuthState.WORKING -> TapPhase.WAITING
    else -> TapPhase.IDLE
}
