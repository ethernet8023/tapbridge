package computer.ethernet.tapbridge

import org.junit.Assert.*
import org.junit.Test

class TapNoticeTest {
    private fun notice(
        visible: Boolean = false,
        tapRequested: Boolean = false,
        auth: AuthState = AuthState.IDLE,
        cardPresent: Boolean = false,
    ) = TapNotice.decide(visible, tapRequested, auth, cardPresent)

    @Test fun hiddenTapRequestAlerts() {
        assertEquals(TapNotice.TAP, notice(tapRequested = true))
    }

    @Test fun visibleAppStaysQuiet() {
        assertEquals(TapNotice.QUIET, notice(visible = true, tapRequested = true))
        assertEquals(TapNotice.QUIET, notice(visible = true, auth = AuthState.REMOVED))
    }

    @Test fun cardArrivingEndsTheAlert() {
        assertEquals(TapNotice.QUIET, notice(tapRequested = true, cardPresent = true))
        assertEquals(TapNotice.QUIET, notice(auth = AuthState.REMOVED, cardPresent = true))
    }

    @Test fun removedMidAuthAsksToTapAgain() {
        assertEquals(TapNotice.TAP_AGAIN, notice(auth = AuthState.REMOVED, tapRequested = true))
    }

    @Test fun requestWindowCapsTheAlert() {
        // The 120 s window ended: the UI keeps saying REMOVED, the
        // notification goes quiet.
        assertEquals(TapNotice.QUIET, notice(auth = AuthState.REMOVED, tapRequested = false))
    }

    @Test fun pinPromptPointsAtThePc() {
        assertEquals(TapNotice.QUIET, notice(auth = AuthState.PIN, tapRequested = true))
    }

    @Test fun noRequestIsQuiet() {
        for (auth in AuthState.entries) {
            assertEquals(auth.name, TapNotice.QUIET, notice(auth = auth))
        }
    }

    @Test fun authStateFromWire() {
        assertEquals(AuthState.IDLE, AuthState.fromWire(0))
        assertEquals(AuthState.WORKING, AuthState.fromWire(1))
        assertEquals(AuthState.SUCCESS, AuthState.fromWire(2))
        assertEquals(AuthState.REMOVED, AuthState.fromWire(3))
        assertEquals(AuthState.PIN, AuthState.fromWire(4))
        assertEquals(AuthState.FAILED, AuthState.fromWire(5))
        assertEquals("unknown states are ignored as idle", AuthState.IDLE, AuthState.fromWire(9))
        assertEquals(AuthState.IDLE, AuthState.fromWire(-1))
    }
}
