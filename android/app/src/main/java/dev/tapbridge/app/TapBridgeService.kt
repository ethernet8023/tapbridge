package dev.tapbridge.app

import android.app.*
import android.content.Intent
import android.nfc.Tag
import android.os.*
import java.io.IOException
import java.util.concurrent.*
import java.util.concurrent.atomic.AtomicLong

/** A lifecycle worker owns connect/release. Close interrupts I/O without waiting for that worker. */
class TapBridgeService : Service() {
    companion object {
        const val ACTION_START = "dev.tapbridge.app.START"
        const val ACTION_STOP = "dev.tapbridge.app.STOP"
    }
    enum class Status { IDLE, CONNECTING, CONNECTED, ERROR }
    interface Listener {
        fun onStatusChanged(status: Status, error: String?)
        fun onCardPresent(present: Boolean)
    }
    inner class LocalBinder : Binder() { val service get() = this@TapBridgeService }
    private val binder = LocalBinder()
    private val gate = NativeGate()
    private val lifecycle = Executors.newSingleThreadExecutor()
    private val nfcWorker = ThreadPoolExecutor(1,1,0L,TimeUnit.MILLISECONDS,ArrayBlockingQueue(8))
    private val requested = AtomicLong()
    private val main = Handler(Looper.getMainLooper())
    private val session = NfcCardSession(gate)
    @Volatile private var wantsConnection = false
    @Volatile private var destroyed = false
    @Volatile private var reader: Thread? = null
    @Volatile var listener: Listener? = null
    @Volatile var status = Status.IDLE
        private set
    @Volatile var lastError: String? = null
        private set
    override fun onBind(intent: Intent?) = binder
    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) { stopSession(); stopSelf() }
        else { foreground(); connectAsync() }
        return START_NOT_STICKY
    }
    private fun foreground() {
        val manager = getSystemService(NOTIFICATION_SERVICE) as NotificationManager
        manager.createNotificationChannel(NotificationChannel("tapbridge","Tapbridge",NotificationManager.IMPORTANCE_LOW))
        val launch = PendingIntent.getActivity(this,0,Intent(this,MainActivity::class.java),PendingIntent.FLAG_IMMUTABLE)
        val notification = Notification.Builder(this,"tapbridge").setContentTitle("Tapbridge")
            .setContentText("Open the app to tap your card").setSmallIcon(android.R.drawable.ic_menu_manage)
            .setContentIntent(launch).setOngoing(true).build()
        if (Build.VERSION.SDK_INT >= 29) startForeground(1,notification,android.content.pm.ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE)
        else startForeground(1,notification)
    }
    private fun show(value: Status, error: String? = null) {
        status = value; lastError = error
        main.post { listener?.onStatusChanged(value,error) }
    }
    @Synchronized fun connectAsync() {
        if (wantsConnection) return
        wantsConnection = true
        val attempt = requested.incrementAndGet()
        show(Status.CONNECTING)
        lifecycle.execute {
            cleanup()
            if (!wantsConnection || requested.get() != attempt) return@execute
            var pointer = 0L
            try {
                NativeBridge.initialize(applicationContext)
                val key = IdentityStore.getOrCreateKey(this)
                val invite = PairingState.consumePendingInvitation()
                val host = if (invite.isNotEmpty()) Invitation.parse(invite)?.host ?: throw IOException("invalid invitation")
                    else IdentityStore.pairedHost(this) ?: throw IOException("paste an invitation first")
                pointer = try { NativeBridge.connect(key,host,invite) } finally { key.fill(0) }
                if (!wantsConnection || requested.get() != attempt) return@execute
                IdentityStore.saveHost(this,host)
                val epoch = gate.install(pointer)
                pointer = 0L
                session.bindHandle(epoch)
                show(Status.CONNECTED)
                reader = Thread({ readLoop(epoch,attempt) },"tapbridge-reader").apply { start() }
            } catch (error: Exception) {
                if (requested.get() == attempt) { wantsConnection = false; show(Status.ERROR,error.message ?: "connection failed") }
            } finally {
                if (pointer != 0L) { NativeBridge.close(pointer); NativeBridge.release(pointer) }
            }
        }
    }
    private fun readLoop(epoch: Long, attempt: Long) {
        val callbacks = object : NativeBridge.Callbacks {
            override fun request(id: Long, generation: Long, data: ByteArray) {
                if (!session.beginRequest(epoch,id,generation)) return
                try { nfcWorker.execute {
                    try {
                        val response = session.transceive(id,generation,data)
                        if (response != null) session.respond(epoch,id,generation,response)
                    } catch (_: Exception) { session.close() }
                } } catch (_: RejectedExecutionException) { gate.closeIfActive() }
            }
            override fun cancel(id: Long) { session.onCancel(id) }
            override fun readCard() { main.post { listener?.onCardPresent(session.hasTagPublic()) } }
        }
        var error: String? = null
        try {
            while (wantsConnection && requested.get() == attempt) {
                if (gate.withHandle(epoch) { NativeBridge.read(it,callbacks) } != true) break
            }
        } catch (failure: Exception) { error = failure.message ?: "connection lost" }
        gate.closeIfActive()
        session.close()
        if (destroyed) return
        lifecycle.execute {
            if (requested.get() == attempt) {
                cleanup(); wantsConnection = false
                show(if (error == null) Status.IDLE else Status.ERROR,error)
            }
        }
    }
    fun onTagDiscovered(tag: Tag) {
        if (!wantsConnection || session.paused) return
        try { nfcWorker.execute {
            runCatching { session.onDiscover(tag) }
            main.post { listener?.onCardPresent(session.hasTagPublic()) }
        } } catch (_: RejectedExecutionException) { session.close() }
    }
    fun resumeTagDiscovery() { session.paused = false }
    fun pauseTagDiscovery() { session.paused = true; session.close(); main.post { listener?.onCardPresent(false) } }
    fun retireTag() = pauseTagDiscovery()
    @Synchronized fun stopSession() {
        wantsConnection = false; requested.incrementAndGet()
        gate.closeIfActive(); session.close(); nfcWorker.queue.clear()
        show(Status.IDLE)
        lifecycle.execute { cleanup() }
        stopForeground(STOP_FOREGROUND_REMOVE)
    }
    private fun cleanup() {
        gate.closeIfActive(); session.close()
        reader?.let { if (it !== Thread.currentThread()) it.join(5000) }
        reader = null
        gate.releaseIfActive(); session.unbindHandle()
    }
    override fun onDestroy() {
        destroyed = true
        stopSession(); listener = null
        lifecycle.execute { cleanup(); nfcWorker.shutdownNow(); session.shutdownWriter() }
        lifecycle.shutdown()
        super.onDestroy()
    }
}
