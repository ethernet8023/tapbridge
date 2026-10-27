package computer.ethernet.tapbridge

import computer.ethernet.tapbridge.PairingFlow.Phase
import computer.ethernet.tapbridge.PairingFlow.PcState
import computer.ethernet.tapbridge.TapBridgeService.Status
import org.junit.Assert.*
import org.junit.Test

class PairingFlowTest {
    private val host = "a".repeat(64)
    private val other = "b".repeat(64)
    private val invite = "tapbridge:1:$host:${"c".repeat(64)}"

    @Test fun pairsOnlyWhenTheScannedPcAccepts() {
        val started = PairingFlow.start(host, invite, now = 0)
        assertEquals(Phase.PAIRING, started.phase)
        assertEquals(invite, started.inviteToUse)
        // Another PC connecting (e.g. the old one) does not finish it.
        assertSame(started, PairingFlow.accepted(started, other, "Old PC"))
        val done = PairingFlow.accepted(started, host, "Desk")!!
        assertEquals(Phase.PAIRED, done.phase)
        assertEquals("Desk", done.hostName)
        assertNull("the secret is dropped once paired", done.invitation)
        assertNull(done.inviteToUse)
    }

    @Test fun retriesQuietlyThenFailsWithTheLastError() {
        val started = PairingFlow.start(host, invite, now = 0)
        val retrying = PairingFlow.failedAttempt(started, "timed out")!!
        assertEquals(Phase.PAIRING, retrying.phase)
        assertEquals("still dialing the scanned PC", invite, retrying.inviteToUse)
        val failed = PairingFlow.timedOut(retrying)!!
        assertEquals(Phase.FAILED, failed.phase)
        assertEquals("timed out", failed.error)
        assertNull("no dialing after failure", failed.inviteToUse)
        assertEquals("Couldn't reach the PC.", PairingFlow.timedOut(started)!!.error)
    }

    @Test fun tryAgainReusesTheCodeWithAFreshWindow() {
        val failed = PairingFlow.timedOut(PairingFlow.start(host, invite, now = 0))
        val again = PairingFlow.retry(failed, now = 50_000)!!
        assertEquals(Phase.PAIRING, again.phase)
        assertEquals(50_000, again.startedAt)
        assertNull(again.error)
        assertEquals(invite, again.inviteToUse)
        // Only a failure can be retried.
        val paired = PairingFlow.accepted(again, host, "Desk")
        assertSame(paired, PairingFlow.retry(paired, now = 60_000))
    }

    @Test fun successAfterTheWindowStillWins() {
        val failed = PairingFlow.timedOut(PairingFlow.start(host, invite, now = 0))
        assertEquals(Phase.PAIRED, PairingFlow.accepted(failed, host, "Desk")!!.phase)
    }

    @Test fun aPairingReplacesASessionWithAnotherPc() {
        val started = PairingFlow.start(host, invite, now = 0)
        assertTrue(PairingFlow.needsDial(started, connectedHost = other))
        assertTrue(PairingFlow.needsDial(started, connectedHost = null))
        assertFalse(PairingFlow.needsDial(started, connectedHost = host))
        assertFalse(PairingFlow.needsDial(PairingFlow.timedOut(started), connectedHost = other))
        assertFalse(PairingFlow.needsDial(null, connectedHost = other))
    }

    @Test fun viewNeverCarriesTheInvitation() {
        val view = PairingFlow.start(host, invite, now = 0).view
        assertFalse(view.toString().contains(invite))
        assertFalse(PairingFlow.start(host, invite, now = 0).toString().contains("c".repeat(64)))
    }

    @Test fun homeRowsShowTheOneSessionAndStandbyForTheRest() {
        assertEquals(PcState.CONNECTED, PairingFlow.pcState(host, Status.CONNECTED, host, host))
        assertEquals(PcState.STANDBY, PairingFlow.pcState(other, Status.CONNECTED, host, host))
        assertEquals(PcState.CONNECTING, PairingFlow.pcState(other, Status.CONNECTING, null, other))
        assertEquals(PcState.STANDBY, PairingFlow.pcState(host, Status.CONNECTING, null, other))
        assertEquals(PcState.ERROR, PairingFlow.pcState(other, Status.ERROR, null, other))
        assertEquals(PcState.STANDBY, PairingFlow.pcState(host, Status.IDLE, null, host))
    }
}
