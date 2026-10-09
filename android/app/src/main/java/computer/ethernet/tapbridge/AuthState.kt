package computer.ethernet.tapbridge

/**
 * The PC's sign-in state, pushed by the host as `Frame::Auth` on every
 * transition (crates/core/src/wire.rs, `wire::auth`). It is the only way
 * the phone learns whether a sign-in finished, so the tap art can say so.
 */
enum class AuthState {
    /** Nothing in progress. */
    IDLE,
    /** The card is in use: keep holding it. */
    WORKING,
    /** The sign-in or registration succeeded. */
    SUCCESS,
    /** The card left mid-auth; the PC waits for a re-tap to finish. */
    REMOVED,
    /** The PC shows a PIN prompt; the PIN is typed on the PC. */
    PIN,
    /** The sign-in failed (wrong PIN, timeout, card error). */
    FAILED;

    companion object {
        /** Wire value to state. Unknown values (a newer host) read as IDLE,
         *  so the phone falls back to presence-only art. */
        fun fromWire(value: Int): AuthState = when (value) {
            1 -> WORKING
            2 -> SUCCESS
            3 -> REMOVED
            4 -> PIN
            5 -> FAILED
            else -> IDLE
        }
    }
}

/**
 * Which foreground notification the service shows. Decided by a pure
 * function so the "when do we buzz the user" rules are unit-tested.
 */
enum class TapNotice {
    /** The quiet session notification. */
    QUIET,
    /** High-importance "Tap your security key" (vibration channel). */
    TAP,
    /** High-importance "Card removed — tap again to finish signing in". */
    TAP_AGAIN;

    companion object {
        /**
         * Alert only when a PC is actively asking for the card, no card is
         * on the phone and the app is not on screen (there the tap art and
         * a short haptic are enough). A PIN prompt points the user at the
         * PC, not the phone, so it stays quiet. [tapRequested] carries the
         * 120 s cap: once the request window ends the alert does too.
         */
        fun decide(visible: Boolean, tapRequested: Boolean, auth: AuthState, cardPresent: Boolean): TapNotice = when {
            visible || cardPresent || !tapRequested -> QUIET
            auth == AuthState.PIN -> QUIET
            auth == AuthState.REMOVED -> TAP_AGAIN
            else -> TAP
        }
    }
}
