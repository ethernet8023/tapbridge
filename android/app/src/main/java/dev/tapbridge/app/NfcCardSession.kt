package dev.tapbridge.app

import android.nfc.Tag
import android.nfc.tech.IsoDep
import android.nfc.tech.NfcA
import java.io.IOException
import java.util.concurrent.LinkedBlockingQueue
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
 * Removal reporting limitation: Android gives us no push notification that a
 * card left the field while we hold it. We report removal only when an actual
 * I/O fails (IOException in transceive), a cancel arrives, or the app leaves
 * the foreground (retireTag). We never invent an immediate removal signal.
 */
class NfcCardSession(private val gate: NativeGate) {
    private val lock = Any()
    private var generation = 0L
    private var tag: IsoDep? = null
    private var pending: Long? = null

    /** Native handle this session is bound to; 0 = unbound. Fixed by the service under its lock. */
    @Volatile var boundHandle: Long = 0
        private set

    /** Set while the app is paused; tag discovery is refused. Cleared on resume. */
    @Volatile var paused: Boolean = true

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
            iso.timeout = 15000
            val historical = iso.historicalBytes ?: byteArrayOf()
            val atr = Atr.fromHistorical(historical)
            val maximum = iso.maxTransceiveLength.coerceAtMost(65536)
            synchronized(lock) {
                if (generation != epoch || paused) { iso.close(); return }
                tag = iso
                reportCard(epoch, atr, maximum)
            }
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

    private fun reportCard(gen: Long, atr: ByteArray, maximum: Int) {
        val h = boundHandle
        writer.execute {
            runCatching { gate.withHandle(h) { NativeBridge.card(it, gen, atr, maximum) } }
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
        runCatching { old?.close() }
        reportCard(epoch, byteArrayOf(), 0)
    }

    /** Flush pending writes and stop the writer (service teardown only). */
    fun shutdownWriter() {
        writer.shutdown()
        writer.awaitTermination(2, TimeUnit.SECONDS)
    }
}
