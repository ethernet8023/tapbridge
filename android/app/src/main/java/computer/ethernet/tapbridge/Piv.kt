package computer.ethernet.tapbridge

import java.io.ByteArrayOutputStream
import java.io.IOException
import java.security.MessageDigest
import java.security.SecureRandom
import java.time.Instant
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter
import java.time.temporal.ChronoUnit
import javax.crypto.Cipher
import javax.crypto.spec.SecretKeySpec

/**
 * PIV (NIST SP 800-73-4, YubiKey flavour) setup done on the phone: find a
 * PIV applet with empty cert slots, then generate a P-256 key in each, have
 * the card self-sign a certificate, and store it. The same steps as
 *
 *     yubico-piv-tool -a generate -s 9a -A ECCP256
 *     yubico-piv-tool -a verify-pin -a selfsign-certificate -s 9a
 *     yubico-piv-tool -a import-certificate -s 9a
 *
 * for 9A (authentication) and 9D (key management): the pair macOS smart-card
 * login asks for. Local to the phone, never part of the bridge path.
 */
object Piv {
    fun interface Transport {
        fun transceive(apdu: ByteArray): ByteArray
    }

    enum class Slot(val id: Int, val objectId: Int, val label: String) {
        AUTHENTICATION(0x9A, 0x5FC105, "9A"),
        KEY_MANAGEMENT(0x9D, 0x5FC10B, "9D"),
    }

    /** The two slots macOS pairs for smart-card login. */
    val SETUP_SLOTS = listOf(Slot.AUTHENTICATION, Slot.KEY_MANAGEMENT)

    class PivException(message: String) : IOException(message)

    /** PIN refused; [retries] left, 0 when blocked. */
    class WrongPin(val retries: Int) : IOException(
        if (retries == 0) "PIN is blocked" else "Wrong PIN ($retries tries left)",
    )

    /** What a tap found: null from [probe] means no usable PIV applet. */
    data class Status(val missing: List<Slot>) {
        val needsSetup get() = missing.isNotEmpty()
    }

    private val AID = byteArrayOf(0xA0.toByte(), 0x00, 0x00, 0x03, 0x08)
    private const val OBJECT_CHUID = 0x5FC102
    private const val ALG_3DES = 0x03
    private const val ALG_AES128 = 0x08
    private const val ALG_AES192 = 0x0A
    private const val ALG_AES256 = 0x0C
    private const val ALG_ECCP256 = 0x11
    private const val SLOT_MANAGEMENT = 0x9B

    /** Factory management key of every YubiKey (3DES before 5.7, AES-192 after). */
    val DEFAULT_MANAGEMENT_KEY = byteArrayOf(
        1, 2, 3, 4, 5, 6, 7, 8, 1, 2, 3, 4, 5, 6, 7, 8, 1, 2, 3, 4, 5, 6, 7, 8,
    )

    // --- Detection ---------------------------------------------------------

    /**
     * SELECT the applet and read the setup slots' certificates. Returns null
     * when there is no PIV applet. A SELECT reply must carry the application
     * property template (tag 0x61): some cards answer 9000 to any SELECT.
     */
    fun probe(card: Transport): Status? {
        if (!select(card)) return null
        val missing = SETUP_SLOTS.filter { readObject(card, it.objectId)?.let(::holdsCertificate) != true }
        return Status(missing)
    }

    fun select(card: Transport): Boolean {
        val (data, sw) = send(card, apdu(0x00, 0xA4, 0x04, 0x00, AID, le = true))
        return sw == 0x9000 && data.isNotEmpty() && data[0] == 0x61.toByte()
    }

    /** GET DATA; null when the object does not exist. */
    fun readObject(card: Transport, objectId: Int): ByteArray? {
        val (data, sw) = send(card, apdu(0x00, 0xCB, 0x3F, 0xFF, objectTag(objectId), le = true))
        return when (sw) {
            0x9000 -> data
            0x6A82 -> null
            else -> throw PivException("reading object ${"%06X".format(objectId)} failed (SW ${"%04X".format(sw)})")
        }
    }

    /** A cert object is 53 { 70 cert, 71 info, FE lrc }; empty or bare means none. */
    private fun holdsCertificate(obj: ByteArray): Boolean {
        val inner = Tlv.find(obj, 0x53) ?: return false
        val cert = Tlv.find(inner, 0x70) ?: return false
        return cert.isNotEmpty()
    }

    // --- Setup ---------------------------------------------------------------

    data class Result(val slots: List<Slot>, val certificates: Map<Slot, ByteArray>)

    /**
     * Fills [slots] (default: the ones with no certificate). Generates a fresh
     * P-256 key per slot, so an existing key without a cert is replaced.
     */
    fun setUp(
        card: Transport,
        pin: String,
        commonName: String,
        slots: List<Slot>? = null,
        managementKey: ByteArray = DEFAULT_MANAGEMENT_KEY,
        now: Instant = Instant.now(),
        random: SecureRandom = SecureRandom(),
    ): Result {
        val status = probe(card) ?: throw PivException("This key has no PIV applet")
        val targets = slots ?: status.missing
        if (targets.isEmpty()) return Result(emptyList(), emptyMap())
        authenticate(card, managementKey, random)
        verifyPin(card, pin)
        val certificates = LinkedHashMap<Slot, ByteArray>()
        for (slot in targets) {
            val point = generate(card, slot)
            val serial = ByteArray(16).also(random::nextBytes).also { it[0] = (it[0].toInt() and 0x7F).toByte() }
            val tbs = SelfSignedCert.tbs(
                point, commonName, serial,
                notBefore = now.truncatedTo(ChronoUnit.SECONDS),
                notAfter = now.truncatedTo(ChronoUnit.SECONDS).plus(3650, ChronoUnit.DAYS),
            )
            val digest = MessageDigest.getInstance("SHA-256").digest(tbs)
            val cert = SelfSignedCert.assemble(tbs, sign(card, slot, digest))
            putCertificate(card, slot, cert)
            certificates[slot] = cert
        }
        // macOS identifies a PIV token by its CHUID; factory keys ship without.
        if (readObject(card, OBJECT_CHUID) == null) putObject(card, OBJECT_CHUID, chuid(now, random))
        return Result(targets, certificates)
    }

    fun verifyPin(card: Transport, pin: String) {
        val bytes = pin.toByteArray(Charsets.US_ASCII)
        if (bytes.size !in 6..8) throw PivException("The PIV PIN is 6 to 8 characters")
        val padded = ByteArray(8) { 0xFF.toByte() }.also { bytes.copyInto(it) }
        val (_, sw) = send(card, apdu(0x00, 0x20, 0x00, 0x80, padded))
        when {
            sw == 0x9000 -> return
            sw and 0xFFF0 == 0x63C0 -> throw WrongPin(sw and 0x0F)
            sw == 0x6983 -> throw WrongPin(0)
            else -> throw PivException("PIN check failed (SW ${"%04X".format(sw)})")
        }
    }

    /** Mutual authentication with the management key (slot 9B). */
    fun authenticate(card: Transport, key: ByteArray, random: SecureRandom = SecureRandom()) {
        val algorithm = managementAlgorithm(card, key)
        val block = if (algorithm == ALG_3DES) 8 else 16
        val step1 = generalAuthenticate(card, algorithm, SLOT_MANAGEMENT, Der.tlv(0x80))
        val witness = Tlv.find(step1, 0x80)?.takeIf { it.size == block }
            ?: throw PivException("Unexpected reply to the management key challenge")
        val challenge = ByteArray(block).also(random::nextBytes)
        val step2 = try {
            generalAuthenticate(
                card, algorithm, SLOT_MANAGEMENT,
                Der.tlv(0x80, crypt(algorithm, key, Cipher.DECRYPT_MODE, witness)),
                Der.tlv(0x81, challenge),
            )
        } catch (_: PivException) {
            throw PivException("This key's management key isn't the default; set it up with yubico-piv-tool")
        }
        val proof = Tlv.find(step2, 0x82)
        if (proof == null || !MessageDigest.isEqual(proof, crypt(algorithm, key, Cipher.ENCRYPT_MODE, challenge))) {
            throw PivException("The key failed management key authentication")
        }
    }

    /**
     * YubiKey 5.3+ reports the management key algorithm (GET METADATA);
     * older firmware and other cards don't, so fall back by key length.
     */
    private fun managementAlgorithm(card: Transport, key: ByteArray): Int {
        val (data, sw) = send(card, apdu(0x00, 0xF7, 0x00, SLOT_MANAGEMENT, null, le = true))
        if (sw == 0x9000) Tlv.find(data, 0x01)?.firstOrNull()?.let { return it.toInt() and 0xff }
        return when (key.size) {
            16 -> ALG_AES128
            32 -> ALG_AES256
            else -> ALG_3DES
        }
    }

    private fun crypt(algorithm: Int, key: ByteArray, mode: Int, block: ByteArray): ByteArray {
        val (transform, spec) = when (algorithm) {
            ALG_3DES -> "DESede/ECB/NoPadding" to SecretKeySpec(key, "DESede")
            ALG_AES128, ALG_AES192, ALG_AES256 -> "AES/ECB/NoPadding" to SecretKeySpec(key, "AES")
            else -> throw PivException("Unsupported management key algorithm ${"%02X".format(algorithm)}")
        }
        return Cipher.getInstance(transform).apply { init(mode, spec) }.doFinal(block)
    }

    /** GENERATE ASYMMETRIC KEY PAIR (ECCP256); returns the 65-byte point. */
    fun generate(card: Transport, slot: Slot): ByteArray {
        val template = Der.tlv(0xAC, Der.tlv(0x80, byteArrayOf(ALG_ECCP256.toByte())))
        val (data, sw) = send(card, apdu(0x00, 0x47, 0x00, slot.id, template, le = true))
        check(sw, "generating a key in ${slot.label}")
        val publicKey = Tlv.find(data, 0x7F49) ?: throw PivException("No public key in the generate reply")
        return Tlv.find(publicKey, 0x86)?.takeIf { it.size == 65 }
            ?: throw PivException("Unexpected public key from ${slot.label}")
    }

    /** ECDSA over a SHA-256 digest with the slot's key; returns DER Ecdsa-Sig-Value. */
    fun sign(card: Transport, slot: Slot, digest: ByteArray): ByteArray {
        val reply = generalAuthenticate(card, ALG_ECCP256, slot.id, Der.tlv(0x82), Der.tlv(0x81, digest))
        return Tlv.find(reply, 0x82) ?: throw PivException("No signature from ${slot.label}")
    }

    fun putCertificate(card: Transport, slot: Slot, cert: ByteArray) =
        putObject(card, slot.objectId, Der.tlv(0x70, cert) + Der.tlv(0x71, byteArrayOf(0)) + Der.tlv(0xFE))

    fun putObject(card: Transport, objectId: Int, contents: ByteArray) {
        val body = objectTag(objectId) + Der.tlv(0x53, contents)
        val (_, sw) = send(card, apdu(0x00, 0xDB, 0x3F, 0xFF, body))
        check(sw, "writing object ${"%06X".format(objectId)}")
    }

    /** Yubico's CHUID: fixed FASC-N, random GUID, expiry, empty signature. */
    fun chuid(now: Instant, random: SecureRandom): ByteArray {
        val fascn = byteArrayOf(
            0xD4.toByte(), 0xE7.toByte(), 0x39, 0xDA.toByte(), 0x73, 0x9C.toByte(), 0xED.toByte(), 0x39,
            0xCE.toByte(), 0x73, 0x9D.toByte(), 0x83.toByte(), 0x68, 0x58, 0x21, 0x08, 0x42, 0x10,
            0x84.toByte(), 0x21, 0xC8.toByte(), 0x42, 0x10, 0xC3.toByte(), 0xEB.toByte(),
        )
        val expiry = DateTimeFormatter.ofPattern("yyyyMMdd")
            .format(now.plus(3650, ChronoUnit.DAYS).atZone(ZoneOffset.UTC)).toByteArray()
        return Der.tlv(0x30, fascn) + Der.tlv(0x34, ByteArray(16).also(random::nextBytes)) +
            Der.tlv(0x35, expiry) + Der.tlv(0x3E) + Der.tlv(0xFE)
    }

    private fun generalAuthenticate(card: Transport, algorithm: Int, slot: Int, vararg items: ByteArray): ByteArray {
        val (data, sw) = send(card, apdu(0x00, 0x87, algorithm, slot, Der.tlv(0x7C, *items), le = true))
        if (sw == 0x6982) throw PivException("The key wants its PIN first")
        check(sw, "GENERAL AUTHENTICATE")
        return Tlv.find(data, 0x7C) ?: throw PivException("Malformed GENERAL AUTHENTICATE reply")
    }

    private fun check(sw: Int, what: String) {
        if (sw == 0x9000) return
        val reason = when (sw) {
            0x6982 -> "not authorized"
            0x6A80 -> "rejected the data"
            0x6A81 -> "unsupported"
            0x6A86 -> "unsupported algorithm or slot"
            else -> "SW ${"%04X".format(sw)}"
        }
        throw PivException("$what failed: $reason")
    }

    private fun objectTag(objectId: Int) =
        Der.tlv(0x5C, byteArrayOf((objectId shr 16).toByte(), (objectId shr 8).toByte(), objectId.toByte()))

    // --- APDU plumbing -------------------------------------------------------

    /** One logical command: a short APDU, or a command chain past 255 bytes. */
    class Command(val cla: Int, val ins: Int, val p1: Int, val p2: Int, val data: ByteArray?, val le: Boolean)

    fun apdu(cla: Int, ins: Int, p1: Int, p2: Int, data: ByteArray?, le: Boolean = false) =
        Command(cla, ins, p1, p2, data, le)

    /**
     * Sends with command chaining (CLA |= 0x10) for long data and GET
     * RESPONSE on 61xx. Returns (response data, status word).
     */
    fun send(card: Transport, command: Command): Pair<ByteArray, Int> {
        val data = command.data ?: ByteArray(0)
        val chunks = if (data.isEmpty()) listOf(data) else data.toList().chunked(255) { it.toByteArray() }
        var reply = ByteArray(0)
        for ((index, chunk) in chunks.withIndex()) {
            val last = index == chunks.lastIndex
            val out = ByteArrayOutputStream()
            out.write(if (last) command.cla else command.cla or 0x10)
            out.write(command.ins); out.write(command.p1); out.write(command.p2)
            if (chunk.isNotEmpty()) { out.write(chunk.size); out.write(chunk) }
            if (last && command.le) out.write(0)
            reply = card.transceive(out.toByteArray())
            if (reply.size < 2) throw PivException("Short reply from the key")
            if (!last && sw(reply) != 0x9000) return ByteArray(0) to sw(reply)
        }
        val body = ByteArrayOutputStream()
        var hops = 0
        while (true) {
            body.write(reply, 0, reply.size - 2)
            val sw = sw(reply)
            if (sw shr 8 != 0x61) return body.toByteArray() to sw
            if (++hops > 64) throw PivException("Reply from the key never ended")
            reply = card.transceive(byteArrayOf(0x00, 0xC0.toByte(), 0x00, 0x00, (sw and 0xFF).toByte()))
            if (reply.size < 2) throw PivException("Short reply from the key")
        }
    }

    private fun sw(reply: ByteArray) =
        ((reply[reply.size - 2].toInt() and 0xff) shl 8) or (reply[reply.size - 1].toInt() and 0xff)
}

/** BER-TLV lookup over one level (one- or two-byte tags, PIV lengths). */
object Tlv {
    data class Item(val tag: Int, val value: ByteArray)

    fun parse(bytes: ByteArray): List<Item> {
        val items = ArrayList<Item>()
        var i = 0
        while (i < bytes.size) {
            var tag = bytes[i++].toInt() and 0xff
            if (tag and 0x1F == 0x1F) {
                if (i >= bytes.size) throw IOException("truncated TLV tag")
                tag = (tag shl 8) or (bytes[i++].toInt() and 0xff)
            }
            if (i >= bytes.size) throw IOException("truncated TLV length")
            var length = bytes[i++].toInt() and 0xff
            if (length > 0x80) {
                val count = length - 0x80
                if (count > 3 || i + count > bytes.size) throw IOException("bad TLV length")
                length = 0
                repeat(count) { length = (length shl 8) or (bytes[i++].toInt() and 0xff) }
            } else if (length == 0x80) {
                throw IOException("indefinite TLV length")
            }
            if (i + length > bytes.size) throw IOException("truncated TLV value")
            items.add(Item(tag, bytes.copyOfRange(i, i + length)))
            i += length
        }
        return items
    }

    fun find(bytes: ByteArray, tag: Int): ByteArray? =
        runCatching { parse(bytes) }.getOrNull()?.firstOrNull { it.tag == tag }?.value
}
