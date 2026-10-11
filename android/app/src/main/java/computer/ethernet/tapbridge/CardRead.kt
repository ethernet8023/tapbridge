package computer.ethernet.tapbridge

import android.nfc.tech.IsoDep

/**
 * What a card supports, read by the phone itself: SELECT of the FIDO
 * applet, then the PIV applet's read-only probe (certificates, metadata,
 * PIN tries without spending one). Never writes; never sends a PIN.
 */
object CardRead {
    /**
     * [caps] holds what was found; [known] the bits the read actually
     * settled (a probe that errored, e.g. the card moved, settles nothing,
     * so what was known before is kept).
     */
    data class Result(val caps: Int, val piv: Piv.Status?, val known: Int = CardInfo.Caps.ALL)

    /** The caller has connected [iso] and closes it afterwards. */
    fun read(iso: IsoDep): Result {
        // PIV first: its probe is the longer exchange, and a FIDO SELECT
        // afterwards can't disturb it.
        val pivRead = runCatching { Piv.probe { iso.transceive(it) } }
            .onFailure { log("PIV probe failed: ${it.javaClass.simpleName}: ${it.message}") }
        val fidoRead = runCatching { Ctap2.selectFido(iso) }
            .onFailure { log("FIDO select failed: ${it.javaClass.simpleName}: ${it.message}") }
        val piv = pivRead.getOrNull()
        val caps = (if (fidoRead.getOrDefault(false)) CardInfo.Caps.FIDO else 0) or pivCaps(piv)
        var known = 0
        if (pivRead.isSuccess) known = known or CardInfo.Caps.PIV or CardInfo.Caps.CERTS or CardInfo.Caps.BLOCKED
        if (fidoRead.isSuccess) known = known or CardInfo.Caps.FIDO
        log("read caps=$caps known=$known")
        return Result(caps, piv, known)
    }

    /** Debug builds only; never card data. */
    private fun log(text: String) {
        if (BuildConfig.DEBUG) android.util.Log.d("tapbridge-card", text)
    }

    /** The PIV half of [CardInfo.caps] from a probe (0 without an applet). */
    fun pivCaps(status: Piv.Status?): Int {
        if (status == null) return 0
        var caps = CardInfo.Caps.PIV
        if (Piv.Slot.AUTHENTICATION in status.certificates) caps = caps or CardInfo.Caps.CERTS
        if (status.contactlessBlocked == true) caps = caps or CardInfo.Caps.BLOCKED
        return caps
    }

    /** [old] caps with the bits a read settled ([known]) replaced by what it found. */
    fun merged(old: Int, read: Result): Int = (old and read.known.inv()) or (read.caps and read.known)

    /** [old] caps with the PIV half replaced by a fresh probe's. */
    fun withPiv(old: Int, status: Piv.Status?): Int = (old and CardInfo.Caps.FIDO) or pivCaps(status)
}
