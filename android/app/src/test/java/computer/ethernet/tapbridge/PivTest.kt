package computer.ethernet.tapbridge

import org.junit.Assert.*
import org.junit.Test
import java.io.ByteArrayOutputStream
import java.security.KeyPair
import java.security.KeyPairGenerator
import java.security.SecureRandom
import java.security.Signature
import java.security.cert.CertificateFactory
import java.security.cert.X509Certificate
import java.security.interfaces.ECPublicKey
import java.security.spec.ECGenParameterSpec
import java.time.Instant
import javax.crypto.Cipher
import javax.crypto.spec.SecretKeySpec

/** Piv setup against a simulated YubiKey PIV applet. */
class PivTest {
    /** Just enough of a PIV applet: SELECT, GET/PUT DATA, VERIFY, mgmt auth, GENERATE, sign. */
    class FakeCard(
        val aes: Boolean = false,
        val chunkReplies: Boolean = true,
        val selectsEverything: Boolean = false,
        val hasPiv: Boolean = true,
    ) : Piv.Transport {
        val objects = HashMap<Int, ByteArray>()
        val keys = HashMap<Int, KeyPair>()
        var pinOk = false
        var mgmtOk = false
        var retries = 3
        private var witness: ByteArray? = null
        private var pending = ByteArray(0)
        private var chain = ByteArrayOutputStream()
        val log = ArrayList<ByteArray>()

        private fun reply(data: ByteArray, sw: Int = 0x9000): ByteArray {
            if (!chunkReplies || data.size <= 200) return data + byteArrayOf((sw shr 8).toByte(), sw.toByte())
            pending = data.copyOfRange(200, data.size)
            return data.copyOfRange(0, 200) + byteArrayOf(0x61, minOf(pending.size, 255).toByte())
        }

        private fun mgmtCipher(mode: Int, block: ByteArray): ByteArray {
            val key = Piv.DEFAULT_MANAGEMENT_KEY
            val c = if (aes) Cipher.getInstance("AES/ECB/NoPadding").apply { init(mode, SecretKeySpec(key, "AES")) }
            else Cipher.getInstance("DESede/ECB/NoPadding").apply { init(mode, SecretKeySpec(key, "DESede")) }
            return c.doFinal(block)
        }

        override fun transceive(apdu: ByteArray): ByteArray {
            log.add(apdu)
            val cla = apdu[0].toInt() and 0xff
            val ins = apdu[1].toInt() and 0xff
            val p1 = apdu[2].toInt() and 0xff
            val p2 = apdu[3].toInt() and 0xff
            val lc = if (apdu.size > 5) apdu[4].toInt() and 0xff else 0
            val part = if (lc > 0) apdu.copyOfRange(5, 5 + lc) else ByteArray(0)
            if (ins == 0xC0) {
                val n = minOf(pending.size, 255)
                val out = pending.copyOfRange(0, n)
                pending = pending.copyOfRange(n, pending.size)
                val sw = if (pending.isEmpty()) byteArrayOf(0x90.toByte(), 0) else byteArrayOf(0x61, minOf(pending.size, 255).toByte())
                return out + sw
            }
            chain.write(part)
            if (cla and 0x10 != 0) return byteArrayOf(0x90.toByte(), 0)
            val data = chain.toByteArray()
            chain = ByteArrayOutputStream()
            return when (ins) {
                0xA4 -> when {
                    selectsEverything -> reply(ByteArray(0))
                    hasPiv -> reply(Der.tlv(0x61, Der.tlv(0x4F, byteArrayOf(0, 0, 0x10, 0, 1, 0))))
                    else -> reply(ByteArray(0), 0x6A82)
                }
                0xCB -> {
                    val id = Tlv.find(data, 0x5C)!!.fold(0) { a, b -> (a shl 8) or (b.toInt() and 0xff) }
                    objects[id]?.let { reply(it) } ?: reply(ByteArray(0), 0x6A82)
                }
                0xDB -> {
                    if (!mgmtOk) return reply(ByteArray(0), 0x6982)
                    val items = Tlv.parse(data)
                    val id = items.first { it.tag == 0x5C }.value.fold(0) { a, b -> (a shl 8) or (b.toInt() and 0xff) }
                    objects[id] = Der.tlv(0x53, items.first { it.tag == 0x53 }.value)
                    reply(ByteArray(0))
                }
                0x20 -> {
                    if (retries == 0) return reply(ByteArray(0), 0x6983)
                    val pin = data.takeWhile { it != 0xFF.toByte() }.toByteArray().toString(Charsets.US_ASCII)
                    if (pin == "123456") { pinOk = true; retries = 3; reply(ByteArray(0)) }
                    else { retries--; reply(ByteArray(0), 0x63C0 or retries) }
                }
                0xF7 -> if (aes) reply(Der.tlv(0x01, byteArrayOf(0x0A))) else reply(ByteArray(0), 0x6D00)
                0x47 -> {
                    if (!mgmtOk) return reply(ByteArray(0), 0x6982)
                    val kp = KeyPairGenerator.getInstance("EC").apply { initialize(ECGenParameterSpec("secp256r1")) }.generateKeyPair()
                    keys[p2] = kp
                    val pub = kp.public as ECPublicKey
                    fun fixed(v: java.math.BigInteger) = v.toByteArray().let { b ->
                        if (b.size > 32) b.copyOfRange(b.size - 32, b.size) else ByteArray(32 - b.size) + b
                    }
                    val point = byteArrayOf(4) + fixed(pub.w.affineX) + fixed(pub.w.affineY)
                    reply(Der.tlv(0x7F49, Der.tlv(0x86, point)))
                }
                0x87 -> {
                    val inner = Tlv.parse(Tlv.find(data, 0x7C)!!)
                    if (p2 == 0x9B) {
                        val first = inner.firstOrNull { it.tag == 0x80 }
                        if (first != null && first.value.isEmpty()) {
                            val w = ByteArray(if (aes) 16 else 8).also { SecureRandom().nextBytes(it) }
                            witness = w
                            return reply(Der.tlv(0x7C, Der.tlv(0x80, mgmtCipher(Cipher.ENCRYPT_MODE, w))))
                        }
                        if (first == null || !first.value.contentEquals(witness)) return reply(ByteArray(0), 0x6982)
                        mgmtOk = true
                        val challenge = inner.first { it.tag == 0x81 }.value
                        reply(Der.tlv(0x7C, Der.tlv(0x82, mgmtCipher(Cipher.ENCRYPT_MODE, challenge))))
                    } else {
                        if (!pinOk) return reply(ByteArray(0), 0x6982)
                        val digest = inner.first { it.tag == 0x81 }.value
                        val sig = Signature.getInstance("NONEwithECDSA").apply { initSign(keys[p2]!!.private); update(digest) }.sign()
                        reply(Der.tlv(0x7C, Der.tlv(0x82, sig)))
                    }
                }
                else -> reply(ByteArray(0), 0x6D00)
            }
        }
    }

    private fun parse(der: ByteArray) =
        CertificateFactory.getInstance("X.509").generateCertificate(der.inputStream()) as X509Certificate

    @Test fun probeFindsEmptySlots() {
        val status = Piv.probe(FakeCard())!!
        assertEquals(listOf(Piv.Slot.AUTHENTICATION, Piv.Slot.KEY_MANAGEMENT), status.missing)
    }

    @Test fun noPivApplet() {
        assertNull(Piv.probe(FakeCard(hasPiv = false)))
    }

    @Test fun cardThatSelectsAnythingIsNotPiv() {
        assertNull(Piv.probe(FakeCard(selectsEverything = true)))
    }

    @Test fun setUpWritesVerifiableCertificates() {
        for (aes in listOf(false, true)) {
            val card = FakeCard(aes = aes)
            val result = Piv.setUp(card, "123456", "Ari PIV", now = Instant.parse("2026-10-09T12:00:00Z"))
            assertEquals(listOf(Piv.Slot.AUTHENTICATION, Piv.Slot.KEY_MANAGEMENT), result.slots)
            for (slot in result.slots) {
                val stored = Tlv.find(Tlv.find(card.objects[slot.objectId]!!, 0x53)!!, 0x70)!!
                val cert = parse(stored)
                assertArrayEquals(result.certificates[slot], stored)
                cert.verify(card.keys[slot.id]!!.public) // self-signed by the on-card key
                assertEquals(card.keys[slot.id]!!.public, cert.publicKey)
                assertEquals("CN=Ari PIV", cert.subjectX500Principal.name)
                assertEquals(cert.subjectX500Principal, cert.issuerX500Principal)
                assertEquals(3, cert.version)
                assertEquals("SHA256withECDSA", cert.sigAlgName)
                assertEquals(Instant.parse("2036-10-06T12:00:00Z"), cert.notAfter.toInstant())
            }
            assertNotNull(card.objects[0x5FC102]) // CHUID written
            assertTrue(Piv.probe(card)!!.missing.isEmpty())
        }
    }

    @Test fun longPutDataIsChained() {
        val card = FakeCard()
        Piv.setUp(card, "123456", "x".repeat(60))
        assertTrue(card.log.any { it[0].toInt() and 0x10 != 0 && it[1] == 0xDB.toByte() })
        assertTrue(card.log.all { it.size <= 5 + 255 + 1 })
    }

    @Test fun onlyMissingSlotsAreFilled() {
        val card = FakeCard()
        Piv.setUp(card, "123456", "a", slots = listOf(Piv.Slot.AUTHENTICATION))
        val first = card.keys[0x9A]
        val result = Piv.setUp(card, "123456", "b")
        assertEquals(listOf(Piv.Slot.KEY_MANAGEMENT), result.slots)
        assertSame(first, card.keys[0x9A])
    }

    @Test fun wrongPinReportsRetries() {
        val card = FakeCard()
        val error = assertThrows(Piv.WrongPin::class.java) { Piv.setUp(card, "654321", "a") }
        assertEquals(2, error.retries)
        assertTrue(card.keys.isEmpty())
    }

    @Test fun pinLengthChecked() {
        assertThrows(Piv.PivException::class.java) { Piv.setUp(FakeCard(), "12345", "a") }
    }

    @Test fun utf8CommonName() {
        val card = FakeCard()
        Piv.setUp(card, "123456", "Zoë PIV", slots = listOf(Piv.Slot.AUTHENTICATION))
        val cert = parse(Tlv.find(Tlv.find(card.objects[0x5FC105]!!, 0x53)!!, 0x70)!!)
        assertEquals("CN=Zoë PIV", cert.subjectX500Principal.name)
    }
}
