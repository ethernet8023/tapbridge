package computer.ethernet.tapbridge

import org.junit.Assert.*
import org.junit.Test
import java.io.IOException

/** Ctap2 AAGUID extraction against real-shaped authenticatorGetInfo bytes. */
class Ctap2Test {
    /**
     * Real-shaped getInfo CBOR: a map { 1: [1,5], 3: bstr(16) AAGUID, ... }.
     * bytes: a3 (map 3), 01 82 01 05, 03 50 <16 bytes>, 04 68 <tstr>.
     */
    private val aaguid = byteArrayOf(
        0x2f, 0xc0.toByte(), 0x57, 0x93.toByte(), 0x0d, 0x62, 0x4b, 0x1f,
        0x89.toByte(), 0xb6.toByte(), 0x30, 0x93.toByte(), 0x37, 0x7a, 0x34, 0x51,
    )

    private fun buildCbor(): ByteArray {
        val out = ArrayList<Byte>()
        out.add(0xa3.toByte()) // map(3)
        out.add(0x01.toByte()) // key 1 -> [1,5]
        out.add(0x82.toByte()); out.add(0x01.toByte()); out.add(0x05.toByte())
        out.add(0x03.toByte()) // key 3 (aaguid) -> bstr(16)
        out.add(0x50.toByte())
        aaguid.forEach { out.add(it) }
        out.add(0x04.toByte()) // key 4 -> tstr "tapbridge"
        out.add(0x69.toByte())
        "tapbridge".forEach { out.add(it.code.toByte()) }
        return out.toByteArray()
    }

    @Test fun extractsAaguidFromRealShapedInfo() {
        assertEquals(aaguid.contentToString(), Ctap2.aaguidFromInfo(buildCbor())?.contentToString())
    }

    @Test fun returnsNullWithoutSixteenByteString() {
        val cbor = byteArrayOf(0xa1.toByte(), 0x01, 0x82.toByte(), 0x01, 0x05)
        assertNull(Ctap2.aaguidFromInfo(cbor))
    }

    @Test fun handlesMapAfterBstrEntries() {
        // 16-byte bstr first, then a nested map: still finds the AAGUID.
        val cbor = byteArrayOf(0xa2.toByte(), 0x02, 0x50) + aaguid +
            byteArrayOf(0x05, 0xa0.toByte())
        assertEquals(aaguid.contentToString(), Ctap2.aaguidFromInfo(cbor)?.contentToString())
    }

    @Test fun getStatusWordHelpersDoNotCrashOnShortData() {
        assertNull(Ctap2.aaguidFromInfo(byteArrayOf()))
    }
}

/** UID stability rule and record matching (DESIGN "Registered keys"). */
class RegisteredKeysTest {
    @Test fun fourByteUidStarting08IsRandom() {
        assertFalse(RegisteredKeys.uidStable(byteArrayOf(0x08.toByte(), 1, 2, 3)))
        assertTrue(RegisteredKeys.uidStable(byteArrayOf(0x08.toByte(), 1, 2, 3, 4)))
        assertTrue(RegisteredKeys.uidStable(byteArrayOf(0x04.toByte(), 1, 2, 3)))
        assertFalse(RegisteredKeys.uidStable(byteArrayOf()))
    }

    @Test fun stableUidMatchAndAaguidFallback() {
        val uid = byteArrayOf(0x04, 1, 2, 3, 4)
        val aaguid = ByteArray(16) { 7 }
        val record = RegisteredKeys.KeyRecord("id1", "Yubikey", uid, aaguid)
        assertTrue(RegisteredKeys.matches(record, uid, null))
        assertTrue(RegisteredKeys.matches(record, null, aaguid))
        // UID equal wins; a different UID with the right AAGUID still matches
        // per DESIGN ("stable UID equal, otherwise AAGUID equal").
        assertTrue(RegisteredKeys.matches(record, byteArrayOf(9), aaguid))
        assertFalse(RegisteredKeys.matches(record, byteArrayOf(9), ByteArray(16) { 9 }))
        // Random UID (not stored) never blocks an AAGUID match.
        val random = RegisteredKeys.KeyRecord("id1", "Yubikey", null, aaguid)
        assertTrue(RegisteredKeys.matches(random, byteArrayOf(0x08.toByte(), 1, 2, 3), aaguid))
        assertFalse(RegisteredKeys.matches(random, null, ByteArray(16) { 9 }))
    }

    @Test fun storeFormatRoundTripKeepsRecords() {
        val uid = byteArrayOf(0x04, 1, 2, 3, 4)
        val aaguid = ByteArray(16) { 3 }
        val record = RegisteredKeys.KeyRecord("k1", "Backup key", uid, aaguid)
        val encoded = listOf("k1", "Backup key", RegisteredKeys.hex(uid), RegisteredKeys.hex(aaguid))
            .joinToString("|")
        assertEquals(listOf(record), RegisteredKeys.decode(encoded))
        // Absent identifiers decode to null.
        assertEquals(
            listOf(RegisteredKeys.KeyRecord("k2", "Tag-only", null, null)),
            RegisteredKeys.decode("k2|Tag-only||"),
        )
        // Corrupt store behaves as empty; registration can be redone.
        assertEquals(emptyList<RegisteredKeys.KeyRecord>(), RegisteredKeys.decode("not|valid"))
        assertEquals(emptyList<RegisteredKeys.KeyRecord>(), RegisteredKeys.decode(""))
    }
}
