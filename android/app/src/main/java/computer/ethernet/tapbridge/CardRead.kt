package computer.ethernet.tapbridge

import android.nfc.tech.IsoDep

/**
 * What a card supports, read by the phone itself: SELECT of the FIDO
 * applet, then the PIV applet's read-only probe (certificates, metadata,
 * PIN tries without spending one). Never writes; never sends a PIN.
 */
object CardRead {
    data class Result(val caps: Int, val piv: Piv.Status?)

    /** The caller has connected [iso] and closes it afterwards. */
    fun read(iso: IsoDep): Result {
        val fido = runCatching { Ctap2.selectFido(iso) }.getOrDefault(false)
        val piv = runCatching { Piv.probe { iso.transceive(it) } }.getOrNull()
        return Result((if (fido) CardInfo.Caps.FIDO else 0) or pivCaps(piv), piv)
    }

    /** The PIV half of [CardInfo.caps] from a probe (0 without an applet). */
    fun pivCaps(status: Piv.Status?): Int {
        if (status == null) return 0
        var caps = CardInfo.Caps.PIV
        if (Piv.Slot.AUTHENTICATION in status.certificates) caps = caps or CardInfo.Caps.CERTS
        if (status.contactlessBlocked == true) caps = caps or CardInfo.Caps.BLOCKED
        return caps
    }

    /** [old] caps with the PIV half replaced by a fresh probe's. */
    fun withPiv(old: Int, status: Piv.Status?): Int = (old and CardInfo.Caps.FIDO) or pivCaps(status)
}
