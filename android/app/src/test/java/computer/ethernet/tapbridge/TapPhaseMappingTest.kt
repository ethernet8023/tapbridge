package computer.ethernet.tapbridge

import computer.ethernet.tapbridge.TapBridgeService.Status
import computer.ethernet.tapbridge.ui.TapPhase
import computer.ethernet.tapbridge.ui.tapPhase
import org.junit.Assert.*
import org.junit.Test

class TapPhaseMappingTest {
    private fun phase(
        status: Status = Status.CONNECTED,
        card: Boolean = false,
        requested: Boolean = false,
        auth: AuthState = AuthState.IDLE,
    ) = tapPhase(status, card, requested, auth)

    @Test fun existingBehaviourWithoutAuth() {
        assertEquals(TapPhase.IDLE, phase())
        assertEquals(TapPhase.WAITING, phase(requested = true))
        assertEquals(TapPhase.ACTIVE, phase(card = true))
        assertEquals(TapPhase.DETECTED, phase(status = Status.CONNECTING, card = true))
        assertEquals(TapPhase.WAITING, phase(status = Status.CONNECTING, requested = true))
        assertEquals(TapPhase.IDLE, phase(status = Status.IDLE))
        assertEquals(TapPhase.ERROR, phase(status = Status.ERROR))
    }

    @Test fun connectionErrorBeatsAuthAndPresence() {
        for (auth in AuthState.entries) {
            for (card in listOf(true, false)) {
                assertEquals("$auth card=$card", TapPhase.ERROR, phase(Status.ERROR, card, true, auth))
            }
        }
    }

    @Test fun authStatesIgnoredWithoutSession() {
        // A stale auth state from a dropped session must not drive the art.
        assertEquals(TapPhase.IDLE, phase(status = Status.CONNECTING, auth = AuthState.SUCCESS))
        assertEquals(TapPhase.DETECTED, phase(status = Status.IDLE, card = true, auth = AuthState.PIN))
    }

    @Test fun workingFollowsPresence() {
        assertEquals(TapPhase.ACTIVE, phase(card = true, auth = AuthState.WORKING))
        assertEquals(TapPhase.WAITING, phase(card = false, auth = AuthState.WORKING))
    }

    @Test fun successAndFailureWinOverPresence() {
        for (card in listOf(true, false)) for (requested in listOf(true, false)) {
            assertEquals(TapPhase.SUCCESS, phase(card = card, requested = requested, auth = AuthState.SUCCESS))
            assertEquals(TapPhase.FAILED, phase(card = card, requested = requested, auth = AuthState.FAILED))
        }
    }

    @Test fun removedUntilTheCardReturns() {
        assertEquals(TapPhase.REMOVED, phase(auth = AuthState.REMOVED))
        assertEquals(TapPhase.REMOVED, phase(auth = AuthState.REMOVED, requested = true))
        assertEquals(TapPhase.ACTIVE, phase(card = true, auth = AuthState.REMOVED))
    }

    @Test fun pinWhateverThePresence() {
        assertEquals(TapPhase.PIN, phase(card = true, auth = AuthState.PIN))
        assertEquals(TapPhase.PIN, phase(card = false, auth = AuthState.PIN))
    }

    @Test fun docksOnlyOnRealPresence() {
        assertTrue(TapPhase.ACTIVE.docks(cardPresent = true))
        assertFalse(TapPhase.PIN.docks(cardPresent = false))
        assertTrue(TapPhase.PIN.docks(cardPresent = true))
        assertTrue(TapPhase.SUCCESS.docks(cardPresent = true))
        assertFalse(TapPhase.SUCCESS.docks(cardPresent = false))
        assertFalse(TapPhase.FAILED.docks(cardPresent = false))
        assertFalse(TapPhase.REMOVED.docks(cardPresent = false))
        assertFalse(TapPhase.WAITING.docks(cardPresent = false))
        assertFalse(TapPhase.IDLE.docks(cardPresent = false))
        assertFalse("connection error lifts the card", TapPhase.ERROR.docks(cardPresent = true))
    }

    @Test fun animatesOnlyDuringARequest() {
        assertFalse(TapPhase.IDLE.animates)
        for (p in TapPhase.entries.filter { it != TapPhase.IDLE }) assertTrue(p.name, p.animates)
    }
}
