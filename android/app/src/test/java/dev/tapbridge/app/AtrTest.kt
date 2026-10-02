package dev.tapbridge.app
import org.junit.Assert.*
import org.junit.Test
class AtrTest {
    @Test fun emptyHistoricalHasCorrectCheckByte() {
        assertArrayEquals(byteArrayOf(0x3b,0x80.toByte(),0x80.toByte(),1,1), Atr.fromHistorical(byteArrayOf()))
    }
    @Test fun preservesHistoricalAndExcludesTsFromChecksum() {
        val historical = byteArrayOf(0x80.toByte(),0x11,0x22)
        val atr = Atr.fromHistorical(historical)
        assertArrayEquals(historical, atr.copyOfRange(4,7))
        assertTrue(atr.drop(1).fold(0) { result, byte -> result xor (byte.toInt() and 255) } == 0)
    }
    @Test(expected=IllegalArgumentException::class) fun rejectsOversizedHistorical() {
        Atr.fromHistorical(ByteArray(16))
    }
}
