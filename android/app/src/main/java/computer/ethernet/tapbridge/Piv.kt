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

    open class PivException(message: String) : IOException(message)

    /** PIN refused; [retries] left, 0 when blocked. */
    class WrongPin(val retries: Int) : IOException(
        if (retries == 0) "PIN is blocked" else "Wrong PIN ($retries tries left)",
    )

    /**
     * The PIN was NOT sent: [retries] tries left (null = the card won't
     * say) is at or below [MIN_PIN_RETRIES]. PivApplet wipes every private
     * key once PIN and PUK are both blocked, so the app never spends those
     * last tries.
     */
    class PinRetriesTooLow(val retries: Int?) : IOException(
        if (retries == null) "The key won't report how many PIN tries are left, so tapbridge won't risk a PIN attempt"
        else "Only $retries PIN ${if (retries == 1) "try" else "tries"} left; tapbridge won't use them. Reset the PIN with the PUK first.",
    )

    /**
     * The management key was refused. Safe to retry: the 9B key has no
     * retry counter on PivApplet or YubiKey.
     */
    class WrongManagementKey : IOException("That management key didn't work")

    /** 6982 on a key operation: the card wants a (fresh) PIN VERIFY. */
    class PinRequired : PivException("The key wants its PIN first")

    /**
     * The PIN was just accepted and the slot still refuses to sign: the
     * applet blocks this slot over contactless. PivApplet's
     * PIV_STRICT_CONTACTLESS ("S" in release names like REePSAaD) does that
     * for 9A, 9C and 9D, and the phone is always contactless.
     */
    class ContactlessBlocked(slot: Slot) : PivException(
        "This key's PIV applet blocks slot ${slot.label} over NFC, so it can't be set up from a phone. " +
            "If it runs PivApplet, install a build without \"S\" in its name (for example REePAxaD).",
    )

    /**
     * The card's PIV setup is not what the user reviewed: certificates were
     * added, replaced or removed (or it is another card) since the tap that
     * showed the status. Nothing was changed on the card.
     */
    class SetupChanged(val status: Status) : PivException("This card's smart-card setup changed")

    /** PIN attempts are only made while more than this many tries remain. */
    const val MIN_PIN_RETRIES = 2

    /**
     * What a tap found: null from [probe] means no usable PIV applet.
     * [defaultManagementKey] is null when the card doesn't report it
     * (no GET METADATA); [pinRetries] null when it doesn't report that.
     */
    data class Status(
        val missing: List<Slot>,
        val defaultManagementKey: Boolean? = null,
        val pinRetries: Int? = null,
        /**
         * The applet advertises strict contactless rules (see
         * [ContactlessBlocked]); null when it doesn't say.
         */
        val contactlessBlocked: Boolean? = null,
        /** GET METADATA says the PIN is still the factory 123456; null when unknown. */
        val pinDefault: Boolean? = null,
        /** SHA-256 (hex) of each setup slot's certificate object, for the slots that hold one. */
        val certificates: Map<Slot, String> = emptyMap(),
    ) {
        val needsSetup get() = missing.isNotEmpty()

        /**
         * What setup would touch: which slots hold which certificate, and
         * whether NFC is blocked. Equal summaries mean the card is set up
         * the same way (PIN tries may differ).
         */
        fun summary(): String =
            SETUP_SLOTS.joinToString(";") { "${it.label}=${certificates[it] ?: "-"}" } +
                ";nfc=${if (contactlessBlocked == true) "blocked" else "ok"}"
    }

    private val AID = byteArrayOf(0xA0.toByte(), 0x00, 0x00, 0x03, 0x08)
    private const val OBJECT_CHUID = 0x5FC102
    private const val ALG_3DES = 0x03
    private const val ALG_AES128 = 0x08
    private const val ALG_AES192 = 0x0A
    private const val ALG_AES256 = 0x0C
    private const val ALG_ECCP256 = 0x11
    private const val SLOT_MANAGEMENT = 0x9B
    private const val PIN_REFERENCE = 0x80
    private const val PIN_POLICY_ONCE: Byte = 0x02

    /** Factory PIN of YubiKey and PivApplet. */
    const val DEFAULT_PIN = "123456"

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
        // Strict-contactless builds refuse to even read these certificates
        // over NFC (6982); some builds only refuse the signature, which
        // setUp reports as ContactlessBlocked instead.
        var blocked = false
        val certificates = LinkedHashMap<Slot, String>()
        val missing = SETUP_SLOTS.filter { slot ->
            val (data, sw) = send(card, apdu(0x00, 0xCB, 0x3F, 0xFF, objectTag(slot.objectId), le = true))
            when (sw) {
                0x9000 -> if (holdsCertificate(data)) {
                    certificates[slot] = Cards.hex(MessageDigest.getInstance("SHA-256").digest(data))
                    false
                } else {
                    true
                }
                0x6A82 -> true
                0x6982 -> { blocked = true; true }
                else -> throw PivException("reading object ${"%06X".format(slot.objectId)} failed (SW ${"%04X".format(sw)})")
            }
        }
        val management = metadata(card, SLOT_MANAGEMENT)
        return Status(
            missing,
            defaultManagementKey = management?.let { Tlv.find(it, 0x05) }?.firstOrNull()?.let { it.toInt() == 1 },
            pinRetries = pinRetries(card),
            contactlessBlocked = blocked.takeIf { it },
            pinDefault = metadata(card, PIN_REFERENCE)?.let { Tlv.find(it, 0x05) }?.firstOrNull()?.let { it.toInt() == 1 },
            certificates = certificates,
        )
    }

    /** GET METADATA (YubiKey 5.3+, PivApplet); null when unsupported. */
    private fun metadata(card: Transport, key: Int): ByteArray? {
        val (data, sw) = send(card, apdu(0x00, 0xF7, 0x00, key, null, le = true))
        return if (sw == 0x9000) data else null
    }

    /**
     * PIN tries left, read without spending one: GET METADATA tag 06
     * [total, remaining], else an empty VERIFY (SP 800-73-4), which answers
     * 63Cx. Null when the card says neither (or the PIN is already verified
     * this session, which hides the count).
     */
    fun pinRetries(card: Transport): Int? {
        metadata(card, PIN_REFERENCE)?.let { Tlv.find(it, 0x06) }?.takeIf { it.size == 2 }
            ?.let { return it[1].toInt() and 0xff }
        val (_, sw) = send(card, apdu(0x00, 0x20, 0x00, PIN_REFERENCE, null))
        return when {
            sw and 0xFFF0 == 0x63C0 -> sw and 0x0F
            sw == 0x6983 -> 0
            else -> null
        }
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
     * Fills [slots] (default: the setup slots). Slots that already hold a
     * certificate are left alone unless [replace]: replacing destroys the
     * old key, so only an explicit "replace certificates" choice sets it.
     * A slot with a key but no certificate counts as empty and gets a fresh
     * key.
     *
     * [expected] is the status the user reviewed; if the card on the phone
     * now differs (another card, or certificates changed since), nothing
     * is written and [SetupChanged] carries the new status.
     */
    fun setUp(
        card: Transport,
        pin: String,
        commonName: String,
        slots: List<Slot>? = null,
        managementKey: ByteArray = DEFAULT_MANAGEMENT_KEY,
        now: Instant = Instant.now(),
        random: SecureRandom = SecureRandom(),
        replace: Boolean = false,
        expected: Status? = null,
    ): Result {
        val status = probe(card) ?: throw PivException("This key has no PIV applet")
        if (expected != null && expected.summary() != status.summary()) throw SetupChanged(status)
        if (status.contactlessBlocked == true) throw ContactlessBlocked(status.missing.firstOrNull() ?: SETUP_SLOTS.first())
        val wanted = slots ?: SETUP_SLOTS
        val targets = if (replace) wanted else wanted.filter { it in status.missing }
        if (targets.isEmpty()) return Result(emptyList(), emptyMap())
        authenticate(card, managementKey, random)
        // Checked right before the attempt, on the key actually in the field.
        val retries = pinRetries(card)
        if (retries == null || retries <= MIN_PIN_RETRIES) throw PinRetriesTooLow(retries)
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
            val cert = SelfSignedCert.assemble(tbs, signVerified(card, slot, digest, pin))
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
        val (_, sw) = send(card, apdu(0x00, 0x20, 0x00, PIN_REFERENCE, padded))
        when {
            sw == 0x9000 -> return
            sw and 0xFFF0 == 0x63C0 -> throw WrongPin(sw and 0x0F)
            sw == 0x6983 -> throw WrongPin(0)
            else -> throw PivException("PIN check failed (SW ${"%04X".format(sw)})")
        }
    }

    /**
     * Mutual authentication with the management key (slot 9B). A wrong key
     * throws [WrongManagementKey]; 9B has no retry counter, so callers may
     * let the user try again.
     */
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
            throw WrongManagementKey()
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
        metadata(card, SLOT_MANAGEMENT)?.let { Tlv.find(it, 0x01) }?.firstOrNull()
            ?.let { return it.toInt() and 0xff }
        return when (key.size) {
            16 -> ALG_AES128
            32 -> ALG_AES256
            else -> ALG_3DES
        }
    }

    /** Hex management key (spaces/colons allowed); null unless 16, 24 or 32 bytes. */
    fun parseManagementKey(text: String): ByteArray? {
        val hex = text.filterNot { it.isWhitespace() || it == ':' }
        val bytes = Cards.hexBytes(hex) ?: return null
        return bytes.takeIf { it.size in setOf(16, 24, 32) }
    }

    val DEFAULT_MANAGEMENT_KEY_HEX: String = Cards.hex(DEFAULT_MANAGEMENT_KEY)

    private fun crypt(algorithm: Int, key: ByteArray, mode: Int, block: ByteArray): ByteArray {
        val expected = when (algorithm) {
            ALG_3DES, ALG_AES192 -> 24
            ALG_AES128 -> 16
            ALG_AES256 -> 32
            else -> key.size
        }
        if (key.size != expected) throw WrongManagementKey()
        val (transform, spec) = when (algorithm) {
            ALG_3DES -> "DESede/ECB/NoPadding" to SecretKeySpec(key, "DESede")
            ALG_AES128, ALG_AES192, ALG_AES256 -> "AES/ECB/NoPadding" to SecretKeySpec(key, "AES")
            else -> throw PivException("Unsupported management key algorithm ${"%02X".format(algorithm)}")
        }
        return Cipher.getInstance(transform).apply { init(mode, spec) }.doFinal(block)
    }

    /**
     * GENERATE ASYMMETRIC KEY PAIR (ECCP256); returns the 65-byte point.
     * Sets PIN policy "once" (AA 02, the 9A/9D default) explicitly: PivApplet
     * keeps a slot's old policy across regeneration, and a leftover "always"
     * makes the certificate signature below fail with 6982.
     */
    fun generate(card: Transport, slot: Slot): ByteArray {
        val template = Der.tlv(
            0xAC,
            Der.tlv(0x80, byteArrayOf(ALG_ECCP256.toByte())) + Der.tlv(0xAA, byteArrayOf(PIN_POLICY_ONCE)),
        )
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

    /**
     * [sign] during setup, where [pin] was verified by this card moments
     * ago. A slot with PIN policy "always" refuses (6982) unless VERIFY is
     * the command right before the signature; PivApplet keeps such a policy
     * when the slot is regenerated. Re-sending the PIN the card just accepted
     * cannot cost a try, and it happens at most once per slot.
     */
    private fun signVerified(card: Transport, slot: Slot, digest: ByteArray, pin: String): ByteArray =
        try {
            sign(card, slot, digest)
        } catch (_: PinRequired) {
            verifyPin(card, pin)
            try {
                sign(card, slot, digest)
            } catch (_: PinRequired) {
                throw ContactlessBlocked(slot)
            }
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
        if (sw == 0x6982) throw PinRequired()
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
