package computer.ethernet.tapbridge

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class UsbLinkTest {
    private val hostA = "a".repeat(64)
    private val hostB = "b".repeat(64)

    @Test fun pairedHostReconnectsWithoutAsking() {
        assertEquals(
            UsbLink.Decision.Reconnect(hostA),
            UsbLink.decide(listOf(hostB, hostA), hostA, "Desk", pairingOpen = true),
        )
        assertEquals(
            UsbLink.Decision.Reconnect(hostA),
            UsbLink.decide(listOf(hostA), hostA, "Desk", pairingOpen = false),
        )
    }

    @Test fun storedIdSpellingWins() {
        // Invitations accept uppercase hex; the handshake reports lowercase.
        val upper = hostA.uppercase()
        assertEquals(UsbLink.Decision.Reconnect(upper), UsbLink.decide(listOf(upper), hostA, "Desk", false))
    }

    @Test fun unknownHostWithPairingOpenAsks() {
        assertEquals(
            UsbLink.Decision.AskToPair(hostB, "Office PC"),
            UsbLink.decide(listOf(hostA), hostB, "Office PC", pairingOpen = true),
        )
        assertEquals(
            UsbLink.Decision.AskToPair(hostB, "Office PC"),
            UsbLink.decide(emptyList(), hostB, "Office PC", pairingOpen = true),
        )
    }

    @Test fun unknownHostWithoutPairingIsRefusedWithAName() {
        val d = UsbLink.decide(listOf(hostA), hostB, "Office PC", pairingOpen = false)
        assertEquals(UsbLink.Decision.OpenPairingFirst("Office PC"), d)
        assertTrue(UsbLink.openPairingFirstMessage("Office PC").startsWith("Open Pair phone on Office PC first"))
    }

    @Test fun bareHostIdHintOnlyWhenNothingCached() {
        assertEquals(hostA, UsbLink.hintToSave(null, hostA))
        assertEquals(hostA, UsbLink.hintToSave("", hostA))
        assertNull(UsbLink.hintToSave("$hostA;r=https://relay.example/", hostA))
    }

    @Test fun matchesOnlyOurAccessory() {
        assertTrue(UsbLink.isOurs("tapbridge", "tapbridge"))
        assertFalse(UsbLink.isOurs("Android NFC Bridge", "NFC Bridge"))
        assertFalse(UsbLink.isOurs(null, null))
    }
}
