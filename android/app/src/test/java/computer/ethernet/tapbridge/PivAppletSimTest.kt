package computer.ethernet.tapbridge

import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Before
import org.junit.Test
import java.io.File
import java.net.URLClassLoader
import java.security.cert.CertificateFactory
import java.security.cert.X509Certificate

/**
 * Piv against the real PivApplet (arekinath/PivApplet, as used on the
 * flexSecure) running in jcardsim. Opt-in: needs the applet classes and
 * jcardsim on PIVAPPLET_SIM_CP; build them with scripts/pivapplet-sim.sh.
 * Skipped otherwise.
 */
class PivAppletSimTest {
    private lateinit var card: Piv.Transport
    private var pinVerifies = 0

    @Before fun boot() {
        val cp = System.getenv("PIVAPPLET_SIM_CP")
        assumeTrue("PIVAPPLET_SIM_CP not set; run scripts/pivapplet-sim.sh", !cp.isNullOrBlank())
        val loader = URLClassLoader(
            cp!!.split(File.pathSeparator).map { File(it).toURI().toURL() }.toTypedArray(),
            javaClass.classLoader,
        )
        val aidUtil = loader.loadClass("com.licel.jcardsim.utils.AIDUtil")
        val aidClass = loader.loadClass("javacard.framework.AID")
        val aid = aidUtil.getMethod("create", String::class.java).invoke(null, "A000000308000010000100")
        val simClass = loader.loadClass("com.licel.jcardsim.smartcardio.CardSimulator")
        val sim = simClass.getConstructor().newInstance()
        val appletClass = loader.loadClass("net.cooperi.pivapplet.PivApplet")
        simClass.getMethod("installApplet", aidClass, Class::class.java).invoke(sim, aid, appletClass)
        val transmit = simClass.getMethod("transmitCommand", ByteArray::class.java)
        card = Piv.Transport { apdu ->
            if (apdu[1] == 0x20.toByte() && apdu.size > 5) pinVerifies++
            transmit.invoke(sim, apdu) as ByteArray
        }
    }

    private fun certIn(slot: Piv.Slot): X509Certificate {
        val obj = Piv.readObject(card, slot.objectId)!!
        val der = Tlv.find(Tlv.find(obj, 0x53)!!, 0x70)!!
        return CertificateFactory.getInstance("X.509").generateCertificate(der.inputStream()) as X509Certificate
    }

    /** Spends one PIN try with a wrong PIN, straight at the card. */
    private fun burnPinTry() {
        assertTrue(Piv.select(card))
        val padded = ByteArray(8) { 0xFF.toByte() }.also { "000000".toByteArray().copyInto(it) }
        Piv.send(card, Piv.apdu(0x00, 0x20, 0x00, 0x80, padded))
    }

    @Test fun freshAppletProbe() {
        val status = Piv.probe(card)!!
        assertEquals(Piv.SETUP_SLOTS, status.missing)
        assertEquals(5, status.pinRetries)
        assertEquals(true, status.defaultManagementKey)
        assertEquals(0, pinVerifies)
    }

    @Test fun setUpWithDefaults() {
        val result = Piv.setUp(card, "123456", "flexSecure PIV")
        assertEquals(Piv.SETUP_SLOTS, result.slots)
        for (slot in result.slots) {
            val cert = certIn(slot)
            cert.verify(cert.publicKey) // signed on-card by the slot's own key
            assertEquals("CN=flexSecure PIV", cert.subjectX500Principal.name)
        }
        assertTrue(Piv.probe(card)!!.missing.isEmpty())
        assertEquals(1, pinVerifies)
    }

    @Test fun wrongManagementKeysCostNothing() {
        repeat(10) {
            assertThrows(Piv.WrongManagementKey::class.java) {
                Piv.setUp(card, "123456", "x", managementKey = ByteArray(24) { 0x55 })
            }
        }
        assertEquals(0, pinVerifies)
        assertEquals(5, Piv.probe(card)!!.pinRetries)
        Piv.setUp(card, "123456", "x")
    }

    @Test fun refusesPinAtTwoTriesLeft() {
        repeat(3) { burnPinTry() }
        assertEquals(2, Piv.probe(card)!!.pinRetries)
        pinVerifies = 0
        assertThrows(Piv.PinRetriesTooLow::class.java) { Piv.setUp(card, "123456", "x") }
        assertEquals(0, pinVerifies)
        assertEquals(2, Piv.pinRetries(card))
    }

    @Test fun wrongPinAtThreeLeftThenStops() {
        repeat(2) { burnPinTry() }
        pinVerifies = 0
        val wrong = assertThrows(Piv.WrongPin::class.java) { Piv.setUp(card, "999999", "x") }
        assertEquals(2, wrong.retries)
        assertThrows(Piv.PinRetriesTooLow::class.java) { Piv.setUp(card, "123456", "x") }
        assertEquals(1, pinVerifies)
    }

    /**
     * A slot left with PIN policy "always" (an earlier tool generated it
     * with tag AA 03) wants VERIFY right before every signature; one VERIFY
     * up front then gets 6982 on the certificate signature. Seen live.
     */
    @Test fun setUpSignsSlotsWithPinPolicyAlways() {
        assertTrue(Piv.select(card))
        Piv.authenticate(card, Piv.DEFAULT_MANAGEMENT_KEY)
        val template = Der.tlv(0xAC, Der.tlv(0x80, byteArrayOf(0x11)) + Der.tlv(0xAA, byteArrayOf(0x03)))
        val (_, sw) = Piv.send(card, Piv.apdu(0x00, 0x47, 0x00, 0x9A, template, le = true))
        assertEquals(0x9000, sw)
        pinVerifies = 0
        val result = Piv.setUp(card, "123456", "x")
        assertEquals(Piv.SETUP_SLOTS, result.slots)
        certIn(Piv.Slot.AUTHENTICATION).let { it.verify(it.publicKey) }
        assertEquals(5, Piv.probe(card)!!.pinRetries)
    }
}
