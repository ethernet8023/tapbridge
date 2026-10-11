package computer.ethernet.tapbridge

import org.junit.Assert.*
import org.junit.Test

class CardsTest {
    private val uid = byteArrayOf(4, 1, 2, 3)
    private fun card(label: String, at: Long) = CardInfo.new(uid, label, CardInfo.Caps.FIDO, at)

    @Test fun eachFieldTakesTheNewerEditInAnyOrder() {
        val phone = card("Ring", 10).copy(label = "Phone", labelAt = 30)
        val pc = card("Ring", 10).copy(login = true, loginAt = 20, caps = 7, capsAt = 25)
        val a = phone.merge(pc)
        val b = pc.merge(phone)
        assertEquals(a, b)
        assertEquals("Phone", a.label)
        assertTrue(a.login)
        assertEquals(7, a.caps)
        assertEquals(a, a.merge(phone))
    }

    @Test fun tiesPickTheLargerValueLikeRust() {
        // Rust orders Strings by UTF-8 bytes: U+1F600 (F0 ..) beats U+FF5E (EF ..),
        // though in UTF-16 units U+FF5E is larger.
        val a = card("\uFF5E", 5)
        val b = card("\uD83D\uDE00", 5)
        assertEquals("\uD83D\uDE00", a.merge(b).label)
        assertEquals("\uD83D\uDE00", b.merge(a).label)
        val on = card("x", 5)
        val off = on.copy(wake = false)
        assertTrue(on.merge(off).wake)
        assertTrue(off.merge(on).wake)
    }

    @Test fun forgottenUntilNamedAgain() {
        val c = card("Ring", 10)
        val gone = c.copy(deletedAt = 11)
        assertTrue(gone.deleted)
        assertTrue(c.merge(gone).deleted)
        val back = gone.copy(label = "Again", labelAt = 12)
        assertFalse(back.deleted)
        assertFalse(gone.merge(back).deleted)
    }

    @Test fun validityMatchesTheWire() {
        assertTrue(card("Ring", 1).valid)
        assertFalse(card("", 1).valid)
        assertFalse(card("x".repeat(65), 1).valid)
        assertTrue(card("\u270B".repeat(21), 1).valid)
        assertFalse(card("\u270B".repeat(22), 1).valid)
        assertFalse(card("Ring", 1).copy(caps = 16).valid)
        assertFalse(card("Ring", 1).copy(uid = ByteArray(11)).valid)
    }

    @Test fun encodingRoundTripsAndRejectsOtherFormats() {
        val cards = listOf(
            card("Hand | ring", 10).copy(login = true, loginAt = 12, caps = 7),
            CardInfo.new(byteArrayOf(4, 9, 9, 9), "Gone", 0, 1).copy(deletedAt = 2),
        )
        assertEquals(cards, Cards.decode(Cards.encode(cards)))
        // The old registered-keys format is not migrated.
        assertEquals(emptyList<CardInfo>(), Cards.decode("id|Blue|04112233|"))
        assertEquals(emptyList<CardInfo>(), Cards.decode(null))
        // Random UIDs and duplicates are dropped.
        val text = Cards.encode(cards) + "\n08010203|1|1|1|0|1|0|0|0|Random\n04010203|1|1|1|0|1|0|0|0|Dup"
        assertEquals(cards, Cards.decode(text))
    }

    @Test fun labelsAreCleanedToTheWireLimit() {
        assertEquals("Hand ring", Cards.cleanLabel("  Hand\tring "))
        assertNull(Cards.cleanLabel(" \n "))
        val long = Cards.cleanLabel("\u270B".repeat(30))!!
        assertEquals(21, long.codePointCount(0, long.length))
        val emoji = Cards.cleanLabel("\uD83D\uDE00".repeat(20))!!
        assertEquals(16, emoji.codePointCount(0, emoji.length))
    }

    @Test fun wireUidIsStableOnly() {
        assertArrayEquals(byteArrayOf(), Cards.wireUid(byteArrayOf(8, 1, 2, 3)))
        assertArrayEquals(byteArrayOf(), Cards.wireUid(null))
        assertArrayEquals(byteArrayOf(), Cards.wireUid(ByteArray(11) { 4 }))
        assertArrayEquals(uid, Cards.wireUid(uid))
    }

    @Test fun pivCapsFromAProbe() {
        assertEquals(0, CardRead.pivCaps(null))
        val empty = Piv.Status(missing = Piv.SETUP_SLOTS)
        assertEquals(CardInfo.Caps.PIV, CardRead.pivCaps(empty))
        val set = Piv.Status(missing = emptyList(), certificates = mapOf(Piv.Slot.AUTHENTICATION to "ab"))
        assertEquals(CardInfo.Caps.PIV or CardInfo.Caps.CERTS, CardRead.pivCaps(set))
        val blocked = empty.copy(contactlessBlocked = true)
        assertEquals(CardInfo.Caps.PIV or CardInfo.Caps.BLOCKED, CardRead.pivCaps(blocked))
        assertEquals(CardInfo.Caps.FIDO or CardInfo.Caps.PIV, CardRead.withPiv(CardInfo.Caps.ALL, empty))
    }
}
