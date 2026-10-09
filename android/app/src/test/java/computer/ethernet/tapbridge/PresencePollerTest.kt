package computer.ethernet.tapbridge

import org.junit.Assert.*
import org.junit.Test

class PresencePollerTest {
    /** Virtual clock: tasks run only when the test advances time. */
    private class FakeScheduler : PresencePoller.Scheduler {
        var now = 0L
        private val tasks = mutableListOf<Pair<Long, Runnable>>()
        val pending get() = tasks.size
        override fun schedule(delayMs: Long, task: Runnable): PresencePoller.Cancellable {
            val entry = (now + delayMs) to task
            tasks += entry
            return PresencePoller.Cancellable { tasks.remove(entry) }
        }
        fun advance(ms: Long) {
            val until = now + ms
            while (true) {
                val next = tasks.filter { it.first <= until }.minByOrNull { it.first } ?: break
                tasks.remove(next)
                now = next.first
                next.second.run()
            }
            now = until
        }
    }

    private class FakeCard : PresencePoller.Target<String> {
        var held: String? = "tag1"
        var busy = false
        var present = true
        val checks = mutableListOf<Long>()
        val lost = mutableListOf<String>()
        lateinit var clock: FakeScheduler
        override fun snapshot() = held?.let { PresencePoller.Snapshot(it, busy) }
        override fun isPresent(token: String): Boolean { checks += clock.now; return present }
        /** False models a transceive that grabbed the tag after the check. */
        var closes = true
        override fun onLost(token: String): Boolean {
            lost += token
            if (closes) held = null
            return closes
        }
    }

    private fun setup(): Triple<FakeScheduler, FakeCard, PresencePoller<String>> {
        val scheduler = FakeScheduler()
        val card = FakeCard().apply { clock = scheduler }
        return Triple(scheduler, card, PresencePoller(250, scheduler, card))
    }

    @Test fun checksEveryIntervalWhileHeld() {
        val (clock, card, poller) = setup()
        poller.start()
        clock.advance(1000)
        assertEquals(listOf(250L, 500L, 750L, 1000L), card.checks)
        assertTrue(card.lost.isEmpty())
        assertTrue(poller.running)
    }

    @Test fun reportsLossOnceAndStops() {
        val (clock, card, poller) = setup()
        poller.start()
        clock.advance(500)
        card.present = false
        clock.advance(250)
        assertEquals(listOf("tag1"), card.lost)
        assertFalse(poller.running)
        clock.advance(2000)
        assertEquals(1, card.lost.size)
        assertEquals(3, card.checks.size)
        assertEquals(0, clock.pending)
    }

    @Test fun neverChecksWhileTransceiveInFlight() {
        val (clock, card, poller) = setup()
        card.busy = true
        card.present = false // would be lost, but a transceive owns the tag
        poller.start()
        clock.advance(1000)
        assertTrue(card.checks.isEmpty())
        assertTrue(card.lost.isEmpty())
        assertTrue("keeps polling for the next idle gap", poller.running)
        card.busy = false
        clock.advance(250)
        assertEquals(listOf("tag1"), card.lost)
    }

    @Test fun stopsWhenTagGoneOrPaused() {
        val (clock, card, poller) = setup()
        poller.start()
        clock.advance(250)
        card.held = null // closed elsewhere (pause, session end, transceive failure)
        clock.advance(250)
        assertFalse(poller.running)
        assertEquals(0, clock.pending)
        assertEquals(1, card.checks.size)
    }

    @Test fun keepsPollingWhenLossWasNotClosed() {
        val (clock, card, poller) = setup()
        card.present = false
        card.closes = false
        poller.start()
        clock.advance(250)
        assertEquals(1, card.lost.size)
        assertTrue(poller.running)
        card.present = true
        clock.advance(500)
        assertEquals(3, card.checks.size)
        assertEquals(1, card.lost.size)
        assertEquals(1, clock.pending)
    }

    @Test fun stopCancelsPendingTick() {
        val (clock, card, poller) = setup()
        poller.start()
        poller.stop()
        clock.advance(1000)
        assertTrue(card.checks.isEmpty())
        assertEquals(0, clock.pending)
        assertFalse(poller.running)
    }

    @Test fun restartKeepsSingleLoop() {
        val (clock, card, poller) = setup()
        poller.start()
        clock.advance(100)
        poller.start() // a new tag replaced the old one
        clock.advance(1000)
        assertEquals(listOf(350L, 600L, 850L, 1100L), card.checks)
        assertEquals(1, clock.pending)
    }
}
