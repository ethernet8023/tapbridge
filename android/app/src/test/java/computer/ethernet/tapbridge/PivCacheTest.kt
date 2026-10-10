package computer.ethernet.tapbridge

import org.junit.Assert.*
import org.junit.Test

class PivCacheTest {
    @Test fun roundTripsWhatTheReviewShows() {
        val status = Piv.Status(
            missing = listOf(Piv.Slot.KEY_MANAGEMENT),
            certificates = mapOf(Piv.Slot.AUTHENTICATION to "ab12"),
        )
        val entry = PivCache.decode(PivCache.encode(status))!!
        assertEquals(status.summary(), entry.summary)
        assertEquals(setOf(Piv.Slot.AUTHENTICATION), entry.certificates)
        assertFalse(entry.blocked)
    }

    @Test fun blockedCardsAreRemembered() {
        val status = Piv.Status(missing = Piv.SETUP_SLOTS, contactlessBlocked = true)
        assertTrue(PivCache.decode(PivCache.encode(status))!!.blocked)
    }

    @Test fun garbageIsIgnored() {
        assertNull(PivCache.decode("nope"))
        assertNull(PivCache.decode("a|ZZ|ok"))
    }
}
