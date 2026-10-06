package computer.ethernet.tapbridge

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Multi-PC pairing logic (Track C). Pure JVM tests: the store codec, the
 * legacy migration, wake routing and the per-host merges are all pure
 * functions in [PairedHosts] with no Android dependencies.
 */
class PairedHostsTest {
    private val hostA = "a".repeat(64)
    private val hostB = "b".repeat(64)
    private val hostC = "c".repeat(64)

    // ------------------------------------------------------------ migration

    @Test fun migrationCarriesTheLegacySinglePairing() {
        val records = PairedHosts.migrate(hostA, "Office PC", "hint-a")
        assertEquals(listOf(PairedHosts.Record(hostA, "Office PC", "hint-a", 0L, renamed = true)), records)
    }

    @Test fun migrationWithoutLegacyNameFallsBackToGeneric() {
        val records = PairedHosts.migrate(hostA, null, null)
        assertEquals("PC", records.single().name)
        assertNull(records.single().cachedHint)
    }

    @Test fun unpairedLegacyStateMigratesToEmpty() {
        assertTrue(PairedHosts.migrate(null, "ignored", "ignored").isEmpty())
    }

    @Test(expected = IllegalStateException::class)
    fun corruptLegacyHostIdFailsLoudlyInsteadOfSilentRekey() {
        PairedHosts.migrate("not-hex", "Office PC", null)
    }

    // ---------------------------------------------------------- wake routing

    @Test fun wakeRoutesToTheNamedHostAmongMany() {
        val records = listOf(
            PairedHosts.Record(hostA, "Office PC", "cached-a", 1L),
            PairedHosts.Record(hostB, "MacBook", "cached-b", 2L),
        )
        val wake = PairedHosts.routeForWake(records, hostB, "fresh-b")
        assertEquals(PairedHosts.Wake(hostB, "fresh-b"), wake)
    }

    @Test fun unknownHostIsDropped() {
        val records = listOf(PairedHosts.Record(hostA, "Office PC", null, 1L))
        assertNull(PairedHosts.routeForWake(records, hostC, "whatever"))
    }

    @Test fun nullHostIsDropped() {
        val records = listOf(PairedHosts.Record(hostA, "Office PC", null, 1L))
        assertNull(PairedHosts.routeForWake(records, null, "hint"))
    }

    @Test fun missingHintFallsBackToTheBareHostId() {
        val records = listOf(PairedHosts.Record(hostA, "Office PC", "cached-a", 1L))
        assertEquals(PairedHosts.Wake(hostA, hostA), PairedHosts.routeForWake(records, hostA, null))
        assertEquals(PairedHosts.Wake(hostA, hostA), PairedHosts.routeForWake(records, hostA, "  "))
    }

    @Test fun emptyHintUsesCachedPerHostHint() {
        val records = listOf(PairedHosts.Record(hostA, "Office PC", "cached-a", 1L))
        assertEquals(PairedHosts.Wake(hostA, hostA), PairedHosts.routeForWake(records, hostA, null))
    }

    // ------------------------------------------------------------- upsert/rename

    @Test fun acceptedUpsertAddsASecondHostWithoutTouchingTheFirst() {
        var records = listOf(PairedHosts.Record(hostA, "Office PC", "cached-a", 1L))
        records = PairedHosts.upsertAfterAccept(records, hostB, "MacBook", 2L)
        assertEquals(2, records.size)
        assertEquals("Office PC", records.first { it.hostId == hostA }.name)
        assertEquals("MacBook", records.first { it.hostId == hostB }.name)
        assertEquals(2L, records.first { it.hostId == hostB }.pairedAt)
    }

    @Test fun acceptedRefreshesTheNameButKeepsPairingTime() {
        val records = listOf(PairedHosts.Record(hostA, "Old name", null, 1L))
        val updated = PairedHosts.upsertAfterAccept(records, hostA, "New name", 99L)
        assertEquals("New name", updated.single().name)
        assertEquals(1L, updated.single().pairedAt)
    }

    @Test fun userRenameWinsOverLaterAccepted() {
        var records = listOf(PairedHosts.Record(hostA, "Office PC", null, 1L))
        records = PairedHosts.withRenamed(records, hostA, "My desktop")
        records = PairedHosts.upsertAfterAccept(records, hostA, "Re-reported", 5L)
        assertEquals("My desktop", records.single().name)
        assertTrue(records.single().renamed)
    }

    @Test(expected = IllegalArgumentException::class)
    fun renameOfUnknownHostIsRejected() {
        PairedHosts.withRenamed(emptyList(), hostA, "x")
    }

    // -------------------------------------------------------------- forget

    @Test fun forgetRemovesOnlyTheNamedHost() {
        val records = listOf(
            PairedHosts.Record(hostA, "Office PC", null, 1L),
            PairedHosts.Record(hostB, "MacBook", null, 2L),
        )
        val remaining = PairedHosts.withoutHost(records, hostA)
        assertEquals(listOf(hostB), remaining.map { it.hostId })
    }

    @Test fun forgettingTheLastHostLeavesNone() {
        val records = listOf(PairedHosts.Record(hostA, "Office PC", null, 1L))
        assertTrue(PairedHosts.withoutHost(records, hostA).isEmpty())
    }

    @Test fun lastUsedSurvivesForgettingADifferentHost() {
        val records = listOf(
            PairedHosts.Record(hostA, "Office PC", null, 1L),
            PairedHosts.Record(hostB, "MacBook", null, 2L),
        )
        assertEquals(hostB, PairedHosts.fallbackLastUsed(records, hostA, hostB))
    }

    @Test fun forgettingTheLastUsedHostFallsBackToTheMostRecentRemaining() {
        val records = listOf(
            PairedHosts.Record(hostA, "Office PC", null, 10L),
            PairedHosts.Record(hostB, "MacBook", null, 20L),
            PairedHosts.Record(hostC, "Studio", null, 30L),
        )
        assertEquals(hostC, PairedHosts.fallbackLastUsed(records, hostB, hostB))
    }

    @Test fun forgettingTheOnlyHostClearsLastUsed() {
        val records = listOf(PairedHosts.Record(hostA, "Office PC", null, 1L))
        assertNull(PairedHosts.fallbackLastUsed(records, hostA, hostA))
    }

    // ---------------------------------------------------------------- codec

    @Test fun encodeDecodeRoundTripsManyHosts() {
        val records = listOf(
            PairedHosts.Record(hostA, "Office \"PC\"", "hint-a", 123L, renamed = true),
            PairedHosts.Record(hostB, "Mac\nBook", null, 456L),
            PairedHosts.Record(hostC, "Unicode ✓ ☃", "h3", 7L),
        )
        assertEquals(records, PairedHosts.decode(PairedHosts.encode(records)))
    }

    @Test fun emptyStoreRoundTrips() {
        assertTrue(PairedHosts.decode(PairedHosts.encode(emptyList())).isEmpty())
    }

    @Test fun decodeIsDeterministicInOrder() {
        val records = listOf(
            PairedHosts.Record(hostB, "B", null, 2L),
            PairedHosts.Record(hostA, "A", null, 1L),
        )
        assertEquals(listOf(hostB, hostA), PairedHosts.decode(PairedHosts.encode(records)).map { it.hostId })
    }

    @Test(expected = IllegalStateException::class)
    fun trailingBytesAreRejected() {
        PairedHosts.decode(PairedHosts.encode(emptyList()) + "x")
    }

    @Test
    fun trailingWhitespaceIsAccepted() {
        assertTrue(PairedHosts.decode(PairedHosts.encode(emptyList()) + " \n").isEmpty())
    }

    @Test(expected = IllegalStateException::class)
    fun unknownVersionIsRejected() {
        PairedHosts.decode("{\"v\":2,\"hosts\":{}}")
    }

    @Test(expected = IllegalStateException::class)
    fun unknownFieldsAreRejected() {
        PairedHosts.decode(
            "{\"v\":1,\"hosts\":{\"" + hostA + "\":{\"name\":\"PC\",\"hint\":null,\"at\":1,\"ren\":false,\"extra\":1}}}",
        )
    }

    @Test(expected = IllegalStateException::class)
    fun badHostIdIsRejected() {
        PairedHosts.decode(
            "{\"v\":1,\"hosts\":{\"short\":{\"name\":\"PC\",\"hint\":null,\"at\":1,\"ren\":false}}}",
        )
    }

    @Test(expected = IllegalStateException::class)
    fun overlengthNameIsRejected() {
        PairedHosts.decode(
            "{\"v\":1,\"hosts\":{\"" + hostA + "\":{\"name\":\"" + "x".repeat(65) +
                "\",\"hint\":null,\"at\":1,\"ren\":false}}}",
        )
    }

    @Test(expected = IllegalStateException::class)
    fun fractionalNumbersAreRejected() {
        PairedHosts.decode(
            "{\"v\":1,\"hosts\":{\"" + hostA + "\":{\"name\":\"PC\",\"hint\":null,\"at\":1.5,\"ren\":false}}}",
        )
    }

    // ----------------------------------------------------------- validation

    @Test fun hostIdValidation() {
        assertTrue(PairedHosts.validHostId("0".repeat(64)))
        assertTrue(PairedHosts.validHostId(("0123456789abcdef" + "ABCDEF0123456789").repeat(2)))
        assertTrue(!PairedHosts.validHostId("g".repeat(64)))
        assertTrue(!PairedHosts.validHostId("a".repeat(63)))
        assertTrue(!PairedHosts.validHostId("a".repeat(65)))
    }

    @Test fun nameAndHintBounds() {
        assertTrue(PairedHosts.validName("x"))
        assertTrue(PairedHosts.validName("x".repeat(64)))
        assertTrue(!PairedHosts.validName(""))
        assertTrue(!PairedHosts.validName("x".repeat(65)))
        assertTrue(PairedHosts.validHint("host"))
        assertTrue(!PairedHosts.validHint("  "))
        assertTrue(!PairedHosts.validHint(""))
        assertTrue(PairedHosts.validHint("h".repeat(1024)))
        assertTrue(!PairedHosts.validHint("h".repeat(1025)))
    }
}
