package computer.ethernet.tapbridge

import android.app.*
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.nfc.Tag
import android.os.*
import java.io.IOException
import java.util.concurrent.*
import java.util.concurrent.atomic.AtomicLong

/**
 * On-demand connection owner (DESIGN "Nothing runs at rest"). A
 * connectedDevice foreground service that connects only when:
 *  - the app UI is open and paired (cached hint, visible-retry backoff
 *    0.25/0.5/1/2/3s cap + immediate retry on network change), or
 *  - an FCM wake arrives (ACTION_WAKE with the pushed hint).
 *
 * Idle timeout: 60 s after the last card/APDU activity the service
 * disconnects, disables the tap gate and stops itself.
 *
 * A lifecycle worker owns connect/release. Close interrupts I/O without
 * waiting for that worker. NativeGate/NfcCardSession invariants unchanged.
 */
class TapBridgeService : Service() {
    companion object {
        const val ACTION_START = "computer.ethernet.tapbridge.START"
        const val ACTION_STOP = "computer.ethernet.tapbridge.STOP"
        const val ACTION_WAKE = "computer.ethernet.tapbridge.WAKE"
        const val EXTRA_HINT = "computer.ethernet.tapbridge.HINT"
        const val CHANNEL_FOREGROUND = "tapbridge_session"
        const val FOREGROUND_ID = 1
        const val IDLE_TIMEOUT_MS = 60_000L
        const val WAKE_WINDOW_MS = 120_000L
        private val RETRY_DELAYS_MS = longArrayOf(250, 500, 1000, 2000, 3000)
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
    private val nfcWorker = ThreadPoolExecutor(1, 1, 0L, TimeUnit.MILLISECONDS, ArrayBlockingQueue(8))
    private val requested = AtomicLong()
    private val main = Handler(Looper.getMainLooper())
    private val session = NfcCardSession(gate)
    private val connectivity = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) {
            // Immediate retry after a network change while the UI is visible.
            // Also fires once on registration; skip while an attempt runs.
            if (visible && wantsConnection && (status == Status.ERROR || status == Status.IDLE)) {
                main.post { connectAsync() }
            }
        }
    }
    private var networkCallbackRegistered = false
    /** The app UI is on screen; idle timeout never stops a visible session. */
    @Volatile private var visible = false
    private val closeWakeWindowRunnable = Runnable { closeWakeWindow() }
    @Volatile private var wantsConnection = false
    @Volatile private var destroyed = false
    @Volatile private var reader: Thread? = null
    @Volatile var listener: Listener? = null
    @Volatile var status = Status.IDLE
        private set
    @Volatile var lastError: String? = null
        private set
    @Volatile var startedFromUi = false
        private set
    @Volatile var wakeHint: String? = null
        private set
    private var retryAttempt = 0

    override fun onBind(intent: Intent?) = binder

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> { stopSession(); stopSelf() }
            ACTION_WAKE -> {
                wakeHint = intent.getStringExtra(EXTRA_HINT)
                startedFromUi = false
                foreground()
                wantsConnection = true
                retryAttempt = 0
                openWakeWindow()
                connectAsync()
            }
            else -> { // ACTION_START / rebind while the UI is visible
                startedFromUi = true
                wakeHint = null
                foreground()
                registerNetworkRetry()
                connectAsync()
            }
        }
        return START_NOT_STICKY
    }

    /**
     * Visible: keep the session and retry on drops. Hidden: the idle timeout
     * (60 s from now, or from the last card/APDU activity) ends the session.
     * Not stopping immediately avoids dropping the link on brief pauses
     * (permission dialogs, the tap gate's translucent hop, notification shade).
     */
    fun onAppVisibility(visible: Boolean) {
        if (destroyed) return
        this.visible = visible
        if (visible) {
            if (wantsConnection && status != Status.CONNECTED && status != Status.CONNECTING) connectAsync()
        } else {
            bumpActivity()
        }
    }

    private fun foreground() {
        val manager = getSystemService(NOTIFICATION_SERVICE) as NotificationManager
        manager.createNotificationChannel(
            NotificationChannel(
                CHANNEL_FOREGROUND, "Bridge session",
                NotificationManager.IMPORTANCE_LOW,
            ).apply { description = "Bridges your security key to the PC" }
        )
        val launch = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE,
        )
        val notification = Notification.Builder(this, CHANNEL_FOREGROUND)
            .setContentTitle("Tapbridge")
            .setContentText("Open the app to tap your card")
            .setSmallIcon(R.drawable.ic_notification)
            .setContentIntent(launch)
            .setOngoing(true)
            .build()
        if (Build.VERSION.SDK_INT >= 29) {
            startForeground(FOREGROUND_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE)
        } else {
            startForeground(FOREGROUND_ID, notification)
        }
    }

    private fun show(value: Status, error: String? = null) {
        status = value
        lastError = error
        main.post { listener?.onStatusChanged(value, error) }
    }

    @Synchronized
    fun connectAsync() {
        if (wantsConnection && status == Status.CONNECTED) return
        wantsConnection = true
        val attempt = requested.incrementAndGet()
        show(Status.CONNECTING)
        lifecycle.execute {
            cleanup()
            if (!wantsConnection || requested.get() != attempt) return@execute
            var pointer = 0L
            try {
                NativeBridge.initialize(applicationContext)
                val key = IdentityStore.getOrCreateKey(this@TapBridgeService)
                val invite = PairingState.consumePendingInvitation()
                val cachedHost = IdentityStore.pairedHost(this@TapBridgeService)
                val hint = when {
                    invite.isNotEmpty() -> {
                        Invitation.parse(invite)?.host ?: throw IOException("invalid invitation")
                    }
                    cachedHost != null -> (wakeHint ?: IdentityStore.cachedHint(this@TapBridgeService) ?: cachedHost)
                    else -> throw IOException("scan the PC's pairing QR first")
                }
                pointer = try {
                    NativeBridge.connect(
                        key, hint, invite,
                        IdentityStore.phoneName(this@TapBridgeService),
                        IdentityStore.pushToken(this@TapBridgeService) ?: "",
                    )
                } finally { key.fill(0) }
                if (!wantsConnection || requested.get() != attempt) return@execute
                // Persist pairing before any session use; cache the dial hint.
                IdentityStore.saveHost(this@TapBridgeService, hint.substringBefore(";"))
                NativeBridge.usedHint(pointer)?.let { IdentityStore.saveHint(this@TapBridgeService, it) }
                IdentityStore.saveHostName(
                    this@TapBridgeService,
                    NativeBridge.hostName(pointer) ?: "PC",
                )
                val epoch = gate.install(pointer)
                pointer = 0L
                session.bindHandle(epoch)
                show(Status.CONNECTED)
                retryAttempt = 0
                // Arm the idle timeout now: a wake nobody answers must not
                // hold the connection (and its heartbeats) open.
                bumpActivity()
                reader = Thread({ readLoop(epoch, attempt) }, "tapbridge-reader").apply { start() }
            } catch (error: Exception) {
                if (requested.get() == attempt) {
                    gate.closeIfActive()
                    session.close()
                    show(Status.ERROR, error.message ?: "connection failed")
                    scheduleRetry(attempt)
                }
            } finally {
                if (pointer != 0L) { NativeBridge.close(pointer); NativeBridge.release(pointer) }
            }
        }
    }

    /** Visible retry backoff: 0.25/0.5/1/2/3s cap. Wake attempts retry once. */
    private fun scheduleRetry(attempt: Long) {
        if (!wantsConnection || destroyed) return
        val delay = if (startedFromUi) {
            RETRY_DELAYS_MS[retryAttempt.coerceAtMost(RETRY_DELAYS_MS.size - 1)].also { retryAttempt++ }
        } else {
            if (retryAttempt++ < 1) 1000L else return
        }
        main.postDelayed({
            if (wantsConnection && requested.get() == attempt && status != Status.CONNECTED) connectAsync()
        }, delay)
    }

    @Synchronized
    private fun registerNetworkRetry() {
        if (networkCallbackRegistered) return
        try {
            val manager = getSystemService(ConnectivityManager::class.java) ?: return
            val request = NetworkRequest.Builder()
                .addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
                .build()
            manager.registerNetworkCallback(request, connectivity)
            networkCallbackRegistered = true
        } catch (_: Exception) {}
    }

    @Synchronized
    private fun unregisterNetworkRetry() {
        if (!networkCallbackRegistered) return
        networkCallbackRegistered = false
        try {
            getSystemService(ConnectivityManager::class.java)?.unregisterNetworkCallback(connectivity)
        } catch (_: Exception) {}
    }

    private fun readLoop(epoch: Long, attempt: Long) {
        val callbacks = object : NativeBridge.Callbacks {
            override fun request(id: Long, generation: Long, data: ByteArray) {
                bumpActivity()
                if (!session.beginRequest(epoch, id, generation)) return
                try {
                    nfcWorker.execute {
                        try {
                            val response = session.transceive(id, generation, data)
                            bumpActivity()
                            if (response != null) session.respond(epoch, id, generation, response)
                        } catch (_: Exception) { session.close() }
                    }
                } catch (_: RejectedExecutionException) { gate.closeIfActive() }
            }
            override fun cancel(id: Long) { session.onCancel(id) }
            override fun readCard() {
                bumpActivity()
                main.post { listener?.onCardPresent(session.hasTagPublic()) }
            }
        }
        var error: String? = null
        try {
            while (wantsConnection && requested.get() == attempt) {
                if (gate.withHandle(epoch) { NativeBridge.read(it, callbacks) } != true) break
            }
        } catch (failure: Exception) { error = failure.message ?: "connection lost" }
        gate.closeIfActive()
        session.close()
        if (destroyed) return
        lifecycle.execute {
            if (requested.get() == attempt) {
                cleanup()
                show(if (error == null) Status.IDLE else Status.ERROR, error)
                scheduleRetry(attempt)
            }
        }
    }

    /** Idle timeout: 60 s after the last card/APDU activity, disconnect. */
    private val activity = AtomicLong(0)
    private val idleRunnable = object : Runnable {
        override fun run() {
            if (destroyed) return
            val idleFor = SystemClock.elapsedRealtime() - activity.get()
            when {
                visible -> main.postDelayed(this, IDLE_TIMEOUT_MS)
                idleFor >= IDLE_TIMEOUT_MS -> { stopSession(); stopSelf() }
                else -> main.postDelayed(this, IDLE_TIMEOUT_MS - idleFor)
            }
        }
    }

    private fun bumpActivity() {
        activity.set(SystemClock.elapsedRealtime())
        main.removeCallbacks(idleRunnable)
        main.postDelayed(idleRunnable, IDLE_TIMEOUT_MS)
    }

    fun onTagDiscovered(tag: Tag) {
        if (!wantsConnection || session.paused) return
        try {
            nfcWorker.execute {
                runCatching { session.onDiscover(tag) }
                bumpActivity()
                main.post { listener?.onCardPresent(session.hasTagPublic()) }
            }
        } catch (_: RejectedExecutionException) { session.close() }
    }

    fun resumeTagDiscovery() { session.paused = false }
    fun pauseTagDiscovery() { session.paused = true; session.close(); main.post { listener?.onCardPresent(false) } }
    fun retireTag() = pauseTagDiscovery()

    @Synchronized
    fun stopSession() {
        wantsConnection = false
        requested.incrementAndGet()
        gate.closeIfActive(); session.close(); nfcWorker.queue.clear()
        closeWakeWindow()
        unregisterNetworkRetry()
        main.removeCallbacks(idleRunnable)
        show(Status.IDLE)
        lifecycle.execute { cleanup() }
        stopForeground(STOP_FOREGROUND_REMOVE)
    }

    /** Releases the current native session only; network retry and the idle
     *  timer belong to the service lifetime (stopSession), not one attempt. */
    private fun cleanup() {
        gate.closeIfActive(); session.close()
        reader?.let { if (it !== Thread.currentThread()) it.join(5000) }
        reader = null
        gate.releaseIfActive(); session.unbindHandle()
    }

    /** Wake window: tap gate enabled for 120 s, disabled on session stop. */
    private fun openWakeWindow() {
        TapGateActivity.setGateEnabled(this, true)
        // A newer wake restarts the window instead of an older timer closing it.
        main.removeCallbacks(closeWakeWindowRunnable)
        main.postDelayed(closeWakeWindowRunnable, WAKE_WINDOW_MS)
    }

    private fun closeWakeWindow() {
        main.removeCallbacks(closeWakeWindowRunnable)
        TapGateActivity.setGateEnabled(this, false)
    }

    override fun onDestroy() {
        destroyed = true
        stopSession()
        listener = null
        lifecycle.execute { cleanup(); nfcWorker.shutdownNow(); session.shutdownWriter() }
        lifecycle.shutdown()
        super.onDestroy()
    }
}
