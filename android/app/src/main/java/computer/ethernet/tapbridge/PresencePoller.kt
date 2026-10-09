package computer.ethernet.tapbridge

/**
 * Notices a card leaving the phone while we hold it, without sending APDUs.
 *
 * Android has no push signal for tag removal. Every [intervalMs] while a
 * tag is held and NO transceive is in flight, the poller asks [Target.isPresent]
 * (production: `IsoDep.isConnected()`, which asks the NFC service's
 * `isPresent`, i.e. the controller's own presence check). A false answer is
 * reported once through [Target.onLost] and the loop ends; the next tag
 * starts a new loop.
 *
 * Never sends an APDU: one mid-PIN-protocol would disturb the applet state.
 * A tick that finds a transceive in flight skips the check (a failed
 * transceive reports the loss itself) and polls again later. The loop ends
 * by itself when no tag is held, so pause and session end need no extra
 * wiring beyond closing the tag.
 *
 * Pure (scheduler and target are injected) so it is unit-tested with a
 * virtual clock.
 */
class PresencePoller<T : Any>(
    private val intervalMs: Long,
    private val scheduler: Scheduler,
    private val target: Target<T>,
) {
    fun interface Cancellable { fun cancel() }

    fun interface Scheduler { fun schedule(delayMs: Long, task: Runnable): Cancellable }

    /** The held tag and whether a transceive currently owns it. */
    data class Snapshot<T>(val token: T, val busy: Boolean)

    interface Target<T> {
        /** The held tag, or null when none is held (ends the loop). */
        fun snapshot(): Snapshot<T>?
        /** Presence check for [token]; must not send an APDU. */
        fun isPresent(token: T): Boolean
        /**
         * [token] left the field: close it and tell the host and the UI.
         * Returns false when it was not closed after all (a transceive took
         * the tag between the check and now); polling then continues.
         */
        fun onLost(token: T): Boolean
    }

    private val lock = Any()
    /** Bumped on every start/stop so a tick from an older loop does nothing. */
    private var loop = 0L
    private var pending: Cancellable? = null

    val running: Boolean get() = synchronized(lock) { pending != null }

    /** (Re)starts polling; called when a tag is connected. */
    fun start() {
        synchronized(lock) {
            pending?.cancel()
            val mine = ++loop
            pending = scheduler.schedule(intervalMs) { tick(mine) }
        }
    }

    fun stop() {
        synchronized(lock) {
            loop++
            pending?.cancel()
            pending = null
        }
    }

    /** Target calls run outside [lock]: they may block (binder) or re-enter stop(). */
    private fun tick(mine: Long) {
        if (synchronized(lock) { loop != mine }) return
        val snapshot = target.snapshot()
        if (snapshot == null) { stopIf(mine); return }
        if (!snapshot.busy && !target.isPresent(snapshot.token)) {
            if (!stopIf(mine)) return
            if (target.onLost(snapshot.token)) return
            // Not closed: resume this loop unless a newer one started.
            synchronized(lock) {
                if (pending == null) {
                    val again = ++loop
                    pending = scheduler.schedule(intervalMs) { tick(again) }
                }
            }
            return
        }
        synchronized(lock) {
            if (loop == mine) pending = scheduler.schedule(intervalMs) { tick(mine) }
        }
    }

    private fun stopIf(mine: Long): Boolean = synchronized(lock) {
        if (loop != mine) return false
        loop++
        pending = null
        true
    }
}
