package computer.ethernet.tapbridge

import android.nfc.tech.IsoDep
import java.io.IOException

/**
 * Minimal CTAP2 identification exchange, local to the phone (never part of
 * the bridge path): SELECT the FIDO AID, send authenticatorGetInfo, and walk
 * the CBOR for map key 0x03 (AAGUID, bstr(16)).
 *
 * SELECT A0000006472F0001, then CLA 0x80 INS 0x10 (authenticatorGetInfo) with
 * GET RESPONSE on 61xx (skill-validated short-chaining pattern).
 */
object Ctap2 {
    private val FIDO_AID = byteArrayOf(
        0xA0.toByte(), 0x00.toByte(), 0x00.toByte(), 0x06.toByte(), 0x47.toByte(),
        0x2F.toByte(), 0x00.toByte(), 0x01.toByte(),
    )

    /** SELECT the CTAP2 applet. Returns true when SW 9000. */
    fun selectFido(iso: IsoDep): Boolean {
        val select = byteArrayOf(0x00, 0xa4.toByte(), 0x04, 0x00, FIDO_AID.size.toByte()) + FIDO_AID
        val response = iso.transceive(select)
        return endsWith9000(response)
    }

    /**
     * authenticatorGetInfo: `80 10 00 00 01 04 00` — P1=0x04 (get full
     * response), Le=0x00. Follows GET RESPONSE on 61xx status words.
     */
    fun getInfoResponse(iso: IsoDep): ByteArray? {
        var data = iso.transceive(byteArrayOf(0x80.toByte(), 0x10, 0x00, 0x00, 0x01, 0x04, 0x00))
        var hops = 0
        while (true) {
            val last = data.size
            if (last < 2) return null
            val sw1 = data[last - 2].toInt() and 0xff
            val sw2 = data[last - 1].toInt() and 0xff
            if (sw1 == 0x90 && sw2 == 0x00) return data.copyOfRange(0, last - 2)
            if (sw1 == 0x61 && sw2 > 0) {
                if (++hops > 16) return null
                data = data.copyOfRange(0, last - 2) +
                    iso.transceive(byteArrayOf(0x00, 0xc0.toByte(), 0x00, 0x00, sw2.toByte()))
                continue
            }
            return null
        }
    }

    private fun endsWith9000(b: ByteArray): Boolean =
        b.size >= 2 && b[b.size - 2] == 0x90.toByte() && b[b.size - 1] == 0x00.toByte()

    /**
     * Extract AAGUID (map key 0x03, bstr of length 16) from
     * authenticatorGetInfo CBOR. Minimal walk over definite-length map/array
     * headers, which is all getInfo produces. Pure function; unit-tested
     * against real-shaped bytes.
     */
    fun aaguidFromInfo(cbor: ByteArray): ByteArray? {
        return try {
            var index = 0
            var hops = 0
            while (index < cbor.size) {
                if (++hops > 512) return null
                val byte = cbor[index].toInt() and 0xff
                val major = byte shr 5
                when (major) {
                    // bstr: a 16-byte bstr is the AAGUID
                    2 -> {
                        val (headerLen, len) = headerLength(byte, cbor, index)
                        val valueStart = index + headerLen
                        if (len == 16 && valueStart + len <= cbor.size) {
                            return cbor.copyOfRange(valueStart, valueStart + len)
                        }
                        index = valueStart + len
                    }
                    0, 1, 3, 4, 5, 6, 7 -> {
                        val (headerLen, _) = headerLength(byte, cbor, index)
                        index += headerLen
                    }
                    else -> return null
                }
            }
            null
        } catch (_: IOException) {
            null
        }
    }

    /**
     * Additional-data field encoding of CBOR headers: (total header length,
     * payload length for major types 0/1/2/3). Definite lengths only.
     */
    private fun headerLength(firstByte: Int, cbor: ByteArray, index: Int): Pair<Int, Int> {
        val info = firstByte and 0x1f
        return when {
            info < 24 -> 1 to info
            info == 24 -> 2 to (cbor[index + 1].toInt() and 0xff)
            info == 25 -> 3 to (((cbor[index + 1].toInt() and 0xff) shl 8) or (cbor[index + 2].toInt() and 0xff))
            else -> throw IOException("unsupported CBOR length")
        }
    }
}
