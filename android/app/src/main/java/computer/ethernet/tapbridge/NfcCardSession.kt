package computer.ethernet.tapbridge

import android.nfc.Tag
import android.nfc.tech.IsoDep
import android.nfc.tech.NfcA
import java.io.IOException
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.ScheduledThreadPoolExecutor
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit

/**
 * NFC I/O runs on one worker. close() runs independently to unblock transceive.
 *
 * All host-bound native writes (card / response / failed) go through ONE
 * bounded single-thread writer executor, in submission order, so an APDU
 * response can never overtake the card-present report that precedes it and a
 * failure report cannot interleave with a response for the same id. The queue
 * is bounded: when the host stops draining, further reports are dropped
 * rather than accumulating forever (the host times the exchange out).
 *
 * Removal: Android gives no push notification that a card left the field
 * while we hold it. Removal is reported when an I/O fails (IOException in
 * transceive), a cancel arrives, the app leaves the foreground (retireTag),
 * or [presence] sees `IsoDep.isConnected()` turn false. That last one is not
 * invented: isConnected asks the NFC service's isPresent, i.e. the NFC
 * controller's own presence check, and sends no APDU. It runs every
 * [PRESENCE_INTERVAL_MS] only while a tag is held and no transceive is in
 * flight, so it never interleaves with the PC's APDUs (a stray SELECT
 * mid-PIN-protocol would reset the applet state).
 */
class NfcCardSession(private val gate: NativeGate) {
    companion object {
        const val PRESENCE_INTERVAL_MS = 250L
    }
    private val lock = Any()
    private var generation = 0L
    private var tag: IsoDep? = null
    private var pending: Long? = null

    /** Native handle this session is bound to; 0 = unbound. Fixed by the service under its lock. */
    @Volatile var boundHandle: Long = 0
        private set

    /** Set while the app is paused; tag discovery is refused. Cleared on resume. */
    @Volatile var paused: Boolean = true

    /**
     * Called (on whichever thread closed it) after a held tag was dropped for
     * any reason: transceive failure, cancel, presence loss, pause. The host
     * already got the absent Card report; this is for the UI and the
     * service's notification. Must not block.
     */
    @Volatile var onTagLost: (() -> Unit)? = null

    /**
     * What a fresh tap showed the card supports ([CardRead]), read before
     * the host hears of the card so it never interleaves with the PC's own
     * APDUs. Called on the NFC worker; must not block.
     */
    @Volatile var onCardRead: ((uid: ByteArray, result: CardRead.Result) -> Unit)? = null

    /** Whether a tap of this card should be read first (it costs a moment). */
    @Volatile var wantsRead: ((uid: ByteArray) -> Boolean)? = null

    private val presenceTimer = ScheduledThreadPoolExecutor(1) { r ->
        Thread(r, "nfc-presence").apply { isDaemon = true }
    }.apply { removeOnCancelPolicy = true }

    private val presence = PresencePoller(
        PRESENCE_INTERVAL_MS,
        { delay, task ->
            val future = runCatching { presenceTimer.schedule(task, delay, TimeUnit.MILLISECONDS) }.getOrNull()
            PresencePoller.Cancellable { future?.cancel(false) }
        },
        object : PresencePoller.Target<IsoDep> {
            override fun snapshot() = synchronized(lock) {
                tag?.takeIf { !paused }?.let { PresencePoller.Snapshot(it, busy = pending != null) }
            }
            // A binder call, no APDU. A dead NFC service counts as gone.
            override fun isPresent(token: IsoDep) = runCatching { token.isConnected }.getOrDefault(false)
            override fun onLost(token: IsoDep): Boolean {
                // Close only the tag that was checked, and never under a
                // transceive that started after the check.
                if (synchronized(lock) { tag !== token || pending != null }) return false
                close()
                return true
            }
        },
    )

    private val writer = ThreadPoolExecutor(
        1, 1, 0L, TimeUnit.MILLISECONDS,
        LinkedBlockingQueue(64),
        { r -> Thread(r, "nfc-writer").apply { isDaemon = true } },
        java.util.concurrent.RejectedExecutionHandler { _, _ -> gate.closeIfActive() },
    )

    /** Called by the service after gate.setHandle(h), under the service lock. */
    fun bindHandle(h: Long) {
        boundHandle = h
    }

    fun unbindHandle() {
        boundHandle = 0
    }

    fun onDiscover(discovered: Tag) {
        if (paused) return
        close()
        require(NfcA.get(discovered) != null) { "NFC-B is not supported yet" }
        val iso = IsoDep.get(discovered) ?: throw IOException("not an ISO-DEP card")
        val epoch = synchronized(lock) { generation }
        try {
            iso.connect()
            val uid = Cards.wireUid(discovered.id)
            // Read-only: FIDO SELECT and the PIV probe. Done before the Card
            // report, so the PC hasn't started talking to the card yet.
            if (uid.isNotEmpty() && wantsRead?.invoke(uid) == true) {
                iso.timeout = 5000
                runCatching { CardRead.read(iso) }.getOrNull()?.let { onCardRead?.invoke(uid, it) }
            }
            iso.timeout = 15000
            val historical = iso.historicalBytes ?: byteArrayOf()
            val atr = Atr.fromHistorical(historical)
            val maximum = iso.maxTransceiveLength.coerceAtMost(65536)
            synchronized(lock) {
                if (generation != epoch || paused) { iso.close(); return }
                tag = iso
                reportCard(epoch, atr, maximum, uid)
            }
            presence.start()
        } catch (error: Exception) { runCatching { iso.close() }; throw error }
    }

    fun beginRequest(epoch: Long, id: Long, gen: Long): Boolean = synchronized(lock) {
        if (epoch != boundHandle || paused || generation != gen || tag == null || pending != null) {
            fail(id,gen); false
        } else { pending = id; true }
    }

    fun transceive(id: Long, gen: Long, apdu: ByteArray): ByteArray? {
        val iso = synchronized(lock) {
            if (generation != gen || pending != id) null else tag
        }
        if (iso == null) { fail(id, gen); return null }
        return try {
            val response = iso.transceive(apdu)
            synchronized(lock) {
                if (generation != gen || tag !== iso || pending != id) null
                else { response }
            }
        } catch (_: IOException) { fail(id, gen); close(); null }
    }

    fun onCancel(id: Long) {
        val gen = synchronized(lock) { if (pending != id) return; generation }
        close()
        fail(id, gen)
    }

    /**
     * Send an APDU response to the exact handle the request came from. The
     * caller captures that handle in the reader callback; withHandle refuses
     * to write if the session was replaced or released meanwhile.
     */
    fun respond(handle: Long, id: Long, gen: Long, data: ByteArray) {
        writer.execute {
            synchronized(lock) {
                if (generation != gen || pending != id || handle != boundHandle) return@execute
                pending = null
            }
            runCatching { gate.withHandle(handle) { NativeBridge.response(it, id, gen, data) } }
        }
    }

    private fun reportCard(gen: Long, atr: ByteArray, maximum: Int, uid: ByteArray = byteArrayOf()) {
        val h = boundHandle
        writer.execute {
            runCatching { gate.withHandle(h) { NativeBridge.card(it, gen, atr, maximum, uid) } }
        }
    }

    /** Sends a card record to the host. Queued in order with every other
     *  host-bound write, so a record sent before a card report arrives first. */
    fun sendCardInfo(card: CardInfo) {
        val h = boundHandle
        if (h == 0L) return
        writer.execute {
            runCatching {
                gate.withHandle(h) {
                    NativeBridge.cardInfo(
                        it, card.uid, card.label, card.labelAt, card.wake, card.wakeAt,
                        card.caps, card.capsAt, card.login, card.loginAt, card.deletedAt,
                    )
                }
            }
        }
    }

    /** A card action request ([done] false) or its outcome ([done] true). */
    fun sendCardAction(uid: ByteArray, action: Int, done: Boolean, ok: Boolean) {
        val h = boundHandle
        if (h == 0L) return
        writer.execute {
            runCatching { gate.withHandle(h) { NativeBridge.cardAction(it, uid, action, done, ok) } }
        }
    }

    private fun fail(id: Long, gen: Long) {
        val h = boundHandle
        writer.execute {
            runCatching { gate.withHandle(h) { NativeBridge.failed(it, id, gen) } }
        }
    }

    fun readCard() = Unit
    fun hasTagPublic(): Boolean = synchronized(lock) { tag != null }

    fun close() {
        val old: IsoDep?
        val epoch: Long
        synchronized(lock) {
            old = tag
            tag = null
            pending = null
            generation++
            epoch = generation
        }
        presence.stop()
        runCatching { old?.close() }
        reportCard(epoch, byteArrayOf(), 0)
        if (old != null) onTagLost?.invoke()
    }

    /** Flush pending writes and stop the writer (service teardown only). */
    fun shutdownWriter() {
        presence.stop()
        presenceTimer.shutdownNow()
        writer.shutdown()
        writer.awaitTermination(2, TimeUnit.SECONDS)
    }
}
