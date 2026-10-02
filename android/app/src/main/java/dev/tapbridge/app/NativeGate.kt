package dev.tapbridge.app

import java.util.concurrent.locks.ReentrantReadWriteLock
import kotlin.concurrent.read
import kotlin.concurrent.write

/** Pointer lifetime and connection generation are distinct: allocators can reuse pointers. */
class NativeGate {
    private val lock = ReentrantReadWriteLock()
    private var handle = 0L
    private var epoch = 0L
    val active: Boolean get() = lock.read { handle != 0L }

    fun install(pointer: Long): Long = lock.write {
        check(handle == 0L && pointer != 0L)
        epoch++
        handle = pointer
        epoch
    }
    fun <T> withHandle(expectedEpoch: Long, block: (Long) -> T): T? = lock.read {
        if (handle == 0L || epoch != expectedEpoch) null else block(handle)
    }
    fun closeIfActive() = lock.read { if (handle != 0L) NativeBridge.close(handle) }
    fun releaseIfActive() = lock.write {
        val old = handle
        handle = 0L
        if (old != 0L) NativeBridge.release(old)
    }
}
