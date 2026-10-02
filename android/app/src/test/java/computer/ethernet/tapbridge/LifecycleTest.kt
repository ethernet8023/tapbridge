package computer.ethernet.tapbridge
import org.junit.Assert.*
import org.junit.Test
class LifecycleTest {
    @Test fun parsesInvitationAndRejectsInvalid() {
        val host="a".repeat(64); val cap="b".repeat(64)
        assertEquals(host,Invitation.parse("tapbridge:1:$host:$cap")?.host)
        assertNull(Invitation.parse("tapbridge:2:$host:$cap"))
        assertNull(Invitation.parse("tapbridge:1:bad:$cap"))
    }
    @Test fun consumesInvitationOnce() {
        PairingState.setPendingInvitation("secret")
        assertEquals("secret",PairingState.consumePendingInvitation())
        assertEquals("",PairingState.consumePendingInvitation())
    }
    @Test fun pointerReuseCannotAuthorizeOldConnectionEpoch() {
        val gate=NativeGate()
        val first=gate.install(123)
        assertEquals(123L,gate.withHandle(first){it})
        assertNull(gate.withHandle(first+1){it})
    }
}
