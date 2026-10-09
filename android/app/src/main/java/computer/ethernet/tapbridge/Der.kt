package computer.ethernet.tapbridge

import java.io.ByteArrayOutputStream
import java.time.Instant
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter

/** Minimal DER writer: just what a self-signed P-256 certificate needs. */
object Der {
    fun length(n: Int): ByteArray = when {
        n < 0x80 -> byteArrayOf(n.toByte())
        n < 0x100 -> byteArrayOf(0x81.toByte(), n.toByte())
        n < 0x10000 -> byteArrayOf(0x82.toByte(), (n shr 8).toByte(), n.toByte())
        else -> throw IllegalArgumentException("DER value too long: $n")
    }

    /** Tag (one or two bytes, e.g. 0x7F49) + length + concatenated parts. */
    fun tlv(tag: Int, vararg parts: ByteArray): ByteArray {
        val body = ByteArrayOutputStream().apply { parts.forEach { write(it) } }.toByteArray()
        val out = ByteArrayOutputStream()
        if (tag > 0xff) out.write(tag shr 8)
        out.write(tag and 0xff)
        out.write(length(body.size))
        out.write(body)
        return out.toByteArray()
    }

    fun sequence(vararg parts: ByteArray) = tlv(0x30, *parts)
    fun set(vararg parts: ByteArray) = tlv(0x31, *parts)

    /** Unsigned big-endian magnitude as a positive INTEGER. */
    fun positiveInteger(magnitude: ByteArray): ByteArray {
        var start = 0
        while (start < magnitude.size - 1 && magnitude[start] == 0.toByte()) start++
        val trimmed = magnitude.copyOfRange(start, magnitude.size)
        val value = if (trimmed[0].toInt() and 0x80 != 0) byteArrayOf(0) + trimmed else trimmed
        return tlv(0x02, value)
    }

    fun bitString(bytes: ByteArray) = tlv(0x03, byteArrayOf(0), bytes)

    /** RFC 5280: UTCTime through 2049, GeneralizedTime from 2050. */
    fun time(at: Instant): ByteArray {
        val utc = at.atZone(ZoneOffset.UTC)
        return if (utc.year < 2050) {
            tlv(0x17, DateTimeFormatter.ofPattern("yyMMddHHmmss'Z'").format(utc).toByteArray())
        } else {
            tlv(0x18, DateTimeFormatter.ofPattern("yyyyMMddHHmmss'Z'").format(utc).toByteArray())
        }
    }

    /** Name with a single CN (UTF8String). */
    fun commonName(cn: String): ByteArray =
        sequence(set(sequence(OID_COMMON_NAME, tlv(0x0C, cn.toByteArray(Charsets.UTF_8)))))

    val OID_COMMON_NAME = byteArrayOf(0x06, 0x03, 0x55, 0x04, 0x03)
    val OID_ECDSA_SHA256 = byteArrayOf(0x06, 0x08, 0x2A, 0x86.toByte(), 0x48, 0xCE.toByte(), 0x3D, 0x04, 0x03, 0x02)
    val OID_EC_PUBLIC_KEY = byteArrayOf(0x06, 0x07, 0x2A, 0x86.toByte(), 0x48, 0xCE.toByte(), 0x3D, 0x02, 0x01)
    val OID_P256 = byteArrayOf(0x06, 0x08, 0x2A, 0x86.toByte(), 0x48, 0xCE.toByte(), 0x3D, 0x03, 0x01, 0x07)
}

/**
 * Self-signed X.509 v3 certificate for a P-256 key that lives on a card:
 * build the TBSCertificate, let the card sign its SHA-256, then assemble.
 * The same shape as `yubico-piv-tool -a selfsign-certificate`.
 */
object SelfSignedCert {
    /** [point] is the uncompressed EC point 04||X||Y (65 bytes). */
    fun tbs(point: ByteArray, cn: String, serial: ByteArray, notBefore: Instant, notAfter: Instant): ByteArray {
        require(point.size == 65 && point[0] == 0x04.toByte()) { "expected an uncompressed P-256 point" }
        val name = Der.commonName(cn)
        return Der.sequence(
            Der.tlv(0xA0, Der.tlv(0x02, byteArrayOf(2))), // v3
            Der.positiveInteger(serial),
            Der.sequence(Der.OID_ECDSA_SHA256),
            name,
            Der.sequence(Der.time(notBefore), Der.time(notAfter)),
            name,
            Der.sequence(Der.sequence(Der.OID_EC_PUBLIC_KEY, Der.OID_P256), Der.bitString(point)),
        )
    }

    /** [signature] is the DER ECDSA-Sig-Value the card returned. */
    fun assemble(tbs: ByteArray, signature: ByteArray): ByteArray =
        Der.sequence(tbs, Der.sequence(Der.OID_ECDSA_SHA256), Der.bitString(signature))
}
