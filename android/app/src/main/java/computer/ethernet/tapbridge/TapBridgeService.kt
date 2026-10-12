package computer.ethernet.tapbridge

import android.app.*
import android.content.Context
import android.content.BroadcastReceiver
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ServiceInfo
import android.hardware.usb.UsbAccessory
import android.hardware.usb.UsbManager
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
 *  - an FCM wake arrives (ACTION_WAKE with the pushed hint), or
 *  - a PC running tapbridge is plugged in over USB (ACTION_USB, AOA; see
 *    docs/USB-PLAN.md). While the accessory is attached there is no idle
 *    timeout and network connects/retries are suspended; detach ends the
 *    session and the network path resumes.
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
        /** A tapbridge USB accessory is attached (EXTRA_ACCESSORY) and permitted. */
        const val ACTION_USB = "computer.ethernet.tapbridge.USB"
        /** How long an unanswered USB pairing question holds the cable. */
        const val USB_PAIR_PROMPT_MS = 60_000L
        /** Wake target host id (64 hex chars); the hint alone no longer names a PC. */
        const val EXTRA_HOST = "computer.ethernet.tapbridge.HOST"
        const val EXTRA_HINT = "computer.ethernet.tapbridge.HINT"
        const val CHANNEL_FOREGROUND = "tapbridge_session"
        /** Tap requests. A new id: a channel's importance and sound are
         *  frozen once created, so the old "tapbridge_wake" is deleted. */
        const val CHANNEL_TAP_REQUEST = "tap_request"
        private const val CHANNEL_TAP_REQUEST_OLD = "tapbridge_wake"

        /** Creates (idempotently) both channels. */
        fun ensureChannels(context: Context) {
            val manager = context.getSystemService(NotificationManager::class.java)
            manager.deleteNotificationChannel(CHANNEL_TAP_REQUEST_OLD)
            manager.createNotificationChannel(
                NotificationChannel(
                    CHANNEL_FOREGROUND,
                    context.getString(R.string.channel_session),
                    NotificationManager.IMPORTANCE_LOW,
                ).apply { description = context.getString(R.string.channel_session_desc) },
            )
            manager.createNotificationChannel(
                NotificationChannel(
                    CHANNEL_TAP_REQUEST,
                    context.getString(R.string.channel_tap),
                    NotificationManager.IMPORTANCE_HIGH,
                ).apply {
                    description = context.getString(R.string.channel_tap_desc)
                    lockscreenVisibility = Notification.VISIBILITY_PUBLIC
                    enableVibration(true)
                    vibrationPattern = longArrayOf(0, 300, 150, 300, 150, 600)
                    enableLights(true)
                    lightColor = 0xFF7C5CFF.toInt()
                    setShowBadge(true)
                    // Honored only after the user grants Do Not Disturb
                    // access (Settings -> Permissions in the app).
                    setBypassDnd(true)
                },
            )
        }
        const val FOREGROUND_ID = 1
        /** After WakeService.WAKE_NOTIFICATION_ID (2). */
        const val PIV_SETUP_NOTIFICATION_ID = 3
        const val IDLE_TIMEOUT_MS = 60_000L
        const val WAKE_WINDOW_MS = 120_000L
        /** How long the success / failure art stays before returning to idle. */
        const val SUCCESS_HOLD_MS = 2_500L
        const val FAILED_HOLD_MS = 4_000L
        private val RETRY_DELAYS_MS = longArrayOf(250, 500, 1000, 2000, 3000)
    }

    enum class Status { IDLE, CONNECTING, CONNECTED, ERROR }
    interface Listener {
        fun onStatusChanged(status: Status, error: String?)
        fun onCardPresent(present: Boolean)
        /** A PC is asking for a tap (wake or ReadCard), or stopped asking. */
        fun onTapRequested(requested: Boolean) {}
        /** The PC's sign-in state changed (Auth frame, or a local timeout). */
        fun onAuthState(state: AuthState) {}
        /** The PC asks to set up the certificates of the card with this UID. */
        fun onSetupCerts(uid: ByteArray) {}
        /** The login setup the phone asked a PC for ended. */
        fun onLoginDone(uid: ByteArray, ok: Boolean) {}
        /** An unpaired PC on the USB cable offers pairing (null: question withdrawn). */
        fun onUsbPairRequest(prompt: UsbPrompt?) {}
        /** The USB link was refused or failed; tell the user why. */
        fun onUsbNotice(text: String) {}
    }

    /** A verified, unpaired PC on the cable whose pairing window is open. */
    data class UsbPrompt(val hostId: String, val hostName: String)
    inner class LocalBinder : Binder() { val service get() = this@TapBridgeService }
    private val binder = LocalBinder()
    private val gate = NativeGate()
    private val lifecycle = Executors.newSingleThreadExecutor()
    private val nfcWorker = ThreadPoolExecutor(1, 1, 0L, TimeUnit.MILLISECONDS, ArrayBlockingQueue(8))
    private val requested = AtomicLong()
    private val main = Handler(Looper.getMainLooper())
    private val session = NfcCardSession(gate).also { s ->
        // Presence loss, transceive failure or cancel dropped the tag: the
        // host already has the absent Card frame; update the UI and notice.
        s.onTagLost = { main.post { onCardGone() } }
        s.onCardRead = { uid, read -> Cards.noteRead(this, uid, read) }
        // Once per card per app run: the read delays the PC's first APDU,
        // so not on every sign-in, but often enough that a card whose caps
        // were learned partially (e.g. only its PIV side) gets them all.
        s.wantsRead = { uid -> readThisRun.add(Cards.hex(uid)) }
    }
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
    private val endTapRequestRunnable = Runnable { setTapRequested(false) }
    private val clearAuthRunnable = Runnable { setAuthState(AuthState.IDLE) }
    /** The PC's latest sign-in state; IDLE without a session. Main thread. */
    @Volatile var authState = AuthState.IDLE
        private set
    /** The foreground notification currently shown, null when none. Main
     *  thread. Re-posting only on change keeps the tap alert from buzzing
     *  again for every unrelated update. */
    private var notice: TapNotice? = null
    /** A PC asked for a tap and no card has answered it yet. Drives the home
     *  screen's tap animation; cleared by a card, the session ending or
     *  [WAKE_WINDOW_MS]. */
    @Volatile var tapRequested = false
        private set
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
    /** Host id this wake (or app-open auto-connect) is for; the per-host
     *  record supplies the cached hint. */
    @Volatile var targetHost: String? = null
        private set
    @Volatile var wakeHint: String? = null
        private set
    /** OS of the PC on the live session (NativeBridge.PLATFORM_*), for wording; -1 without one. */
    @Volatile var hostPlatform: Int = -1
        private set
    /** The PC on the live session can pair a card with its OS login. */
    @Volatile var hostPivLogin: Boolean = false
        private set

    // ------------------------------------------------------------ USB link
    /** The attached tapbridge accessory being served; null when none. While
     *  set: no idle timeout, no network connect/retry. Main thread writes. */
    @Volatile private var usbAccessory: UsbAccessory? = null
    /** An accessory whose link already ended (refused, failed): not reopened
     *  until it detaches, since the PC probes once per plug-in. */
    @Volatile private var usbEnded: UsbAccessory? = null
    /** The live session runs over the USB cable (status "via USB"). */
    @Volatile var viaUsb = false
        private set
    /** The pairing question waiting for the user; null when none. */
    @Volatile var usbPrompt: UsbPrompt? = null
        private set
    /** Lifecycle thread only: the accessory PFD (Kotlin owns the fd; Rust
     *  uses a dup) and a handshaken handle awaiting the pairing answer. */
    private var usbPfd: ParcelFileDescriptor? = null
    private var usbPendingPointer = 0L
    private val usbPromptTimeout = Runnable { answerUsbPairing(false) }
    private val usbReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            if (intent.action != UsbManager.ACTION_USB_ACCESSORY_DETACHED) return
            @Suppress("DEPRECATION")
            val gone = intent.getParcelableExtra<UsbAccessory>(UsbManager.EXTRA_ACCESSORY)
            if (gone != null && !UsbLink.isOurs(gone.manufacturer, gone.model)) return
            onUsbDetached()
        }
    }
    private var usbReceiverRegistered = false

    /** Cards read on a tap since the app started (hex UID). */
    private val readThisRun: MutableSet<String> = java.util.concurrent.ConcurrentHashMap.newKeySet()

    /** Local card edits go to the PC on the live session (PC edits came from it). */
    private val cardSync = Cards.Listener { card, fromPc ->
        if (!fromPc && status == Status.CONNECTED) session.sendCardInfo(card)
    }

    override fun onCreate() {
        super.onCreate()
        Cards.addListener(cardSync)
        val filter = IntentFilter(UsbManager.ACTION_USB_ACCESSORY_DETACHED)
        runCatching {
            if (Build.VERSION.SDK_INT >= 33) registerReceiver(usbReceiver, filter, RECEIVER_NOT_EXPORTED)
            else @Suppress("UnspecifiedRegisterReceiverFlag") registerReceiver(usbReceiver, filter)
            usbReceiverRegistered = true
        }
    }
    private var retryAttempt = 0

    override fun onBind(intent: Intent?) = binder

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> { stopSession(); stopSelf() }
            ACTION_USB -> {
                @Suppress("DEPRECATION")
                val accessory = intent.getParcelableExtra<UsbAccessory>(UsbManager.EXTRA_ACCESSORY)
                startedFromUi = true
                foreground(TapNotice.QUIET)
                // Declined (already served this plug-in, or not ours): the
                // app was opened, so behave like an app-open start.
                if ((accessory == null || !connectUsb(accessory)) && !usbBusy &&
                    IdentityStore.pairedHosts(this).isNotEmpty()
                ) onStartNetwork(null)
            }
            // The PC on the cable asks for taps itself (ReadCard); a wake
            // or app-open must not retarget the live USB session.
            ACTION_WAKE, ACTION_START, null -> if (usbBusy) {
                startedFromUi = startedFromUi || intent?.action != ACTION_WAKE
                if (notice == null) foreground(TapNotice.QUIET)
            } else onStartNetwork(intent)
            else -> onStartNetwork(intent)
        }
        return START_NOT_STICKY
    }

    private fun onStartNetwork(intent: Intent?) {
        when (intent?.action) {
            ACTION_WAKE -> {
                targetHost = intent.getStringExtra(EXTRA_HOST)
                wakeHint = intent.getStringExtra(EXTRA_HINT)
                startedFromUi = false
                foreground(TapNotice.TAP)
                wantsConnection = true
                retryAttempt = 0
                openWakeWindow()
                connectAsync()
            }
            else -> { // ACTION_START / rebind while the UI is visible
                startedFromUi = true
                wakeHint = null
                targetHost = null // app-open: connect to the last-used host
                // The app is on screen: no need to keep shouting.
                foreground(TapNotice.QUIET)
                registerNetworkRetry()
                connectAsync()
            }
        }
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
            // Left the app mid-request: the tap gate must catch the re-tap,
            // since reader mode is off while the app is hidden.
            if (tapRequested) openWakeWindow(restartRequest = false)
        }
        main.post { updateNotice() }
    }

    /**
     * Shows the notification [TapNotice.decide] picks for the current state.
     * Only while the service is in the foreground: after stopSession the
     * notification is gone and must stay gone. Main thread.
     */
    private fun updateNotice() {
        val current = notice ?: return
        val next = TapNotice.decide(visible, tapRequested, authState, session.hasTagPublic())
        if (next != current) foreground(next)
    }

    /**
     * The service's single notification. While a wake window is open it IS
     * the tap request: high-importance channel, sound and vibration, full
     * content on the lockscreen, ongoing, and promoted to a Live Update
     * (status-bar chip, top of the shade) on Android 16+. Otherwise, while
     * the app itself is on screen, it is the quiet session notice.
     */
    private fun foreground(kind: TapNotice) {
        val tapRequest = kind != TapNotice.QUIET
        val manager = getSystemService(NOTIFICATION_SERVICE) as NotificationManager
        ensureChannels(this)
        val launch = PendingIntent.getActivity(
            this, 0,
            Intent(this, MainActivity::class.java)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val builder = if (tapRequest) {
            val pc = targetHost?.let { IdentityStore.hostById(this, it)?.name }
            val body = if (kind == TapNotice.TAP_AGAIN) {
                pc?.let { getString(R.string.removed_body_named, it) } ?: getString(R.string.removed_body)
            } else {
                pc?.let { getString(R.string.wake_body_named, it) } ?: getString(R.string.wake_body)
            }
            Notification.Builder(this, CHANNEL_TAP_REQUEST)
                .setContentTitle(
                    getString(if (kind == TapNotice.TAP_AGAIN) R.string.removed_title else R.string.wake_title),
                )
                .setContentText(body)
                .setCategory(Notification.CATEGORY_STATUS)
                .setVisibility(Notification.VISIBILITY_PUBLIC)
                .setWhen(System.currentTimeMillis())
                .setShowWhen(true)
                .also { builder ->
                    if (Build.VERSION.SDK_INT >= 36) {
                        builder.setShortCriticalText(getString(R.string.wake_chip))
                        // Live Update request (what AndroidX's
                        // setRequestPromotedOngoing writes).
                        builder.addExtras(android.os.Bundle().apply {
                            putBoolean("android.requestPromotedOngoing", true)
                        })
                    }
                }
        } else {
            Notification.Builder(this, CHANNEL_FOREGROUND)
                .setContentTitle(getString(R.string.app_name))
                .setContentText(getString(R.string.session_body))
        }
        val notification = builder
            .setSmallIcon(R.drawable.ic_notification)
            .setContentIntent(launch)
            .setOngoing(true)
            .build()
        try {
            if (Build.VERSION.SDK_INT >= 29) {
                startForeground(FOREGROUND_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE)
            } else {
                startForeground(FOREGROUND_ID, notification)
            }
            notice = kind
        } catch (_: IllegalStateException) {
            // ForegroundServiceStartNotAllowedException: a bound-only service
            // (the app's deferred start) can't enter the foreground from the
            // background. Post it as a plain notification instead; the
            // session itself is unaffected.
            runCatching { manager.notify(FOREGROUND_ID, notification) }
            notice = kind
        }
        // The heads-up WakeService posts only when the foreground start was
        // refused; this notification replaces it.
        if (tapRequest) manager.cancel(WakeService.WAKE_NOTIFICATION_ID)
    }

    private fun show(value: Status, error: String? = null) {
        status = value
        lastError = error
        main.post { listener?.onStatusChanged(value, error) }
    }

    /** A USB accessory is being served (opening, asking, or live). */
    private val usbBusy get() = usbAccessory != null

    @Synchronized
    fun connectAsync() {
        // The cable is the session while it is attached; no network dialing.
        if (usbBusy) return
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
                val hosts = IdentityStore.pairedHosts(this@TapBridgeService)
                val invitedHost = invite.takeIf { it.isNotEmpty() }?.let {
                    Invitation.parse(it)?.host ?: throw IOException("invalid invitation")
                }
                // Connect-by-host-id: a wake names its host; the app (or a
                // fresh pairing) uses the last-used/invited host. The hint is
                // always scoped to THAT host's record.
                val hostId = targetHost ?: invitedHost ?: IdentityStore.lastUsedHost(this@TapBridgeService)
                val record = hostId?.let { id -> hosts.firstOrNull { it.hostId == id } }
                val hint = when {
                    invitedHost != null -> invitedHost
                    record != null -> (wakeHint ?: record.cachedHint ?: record.hostId)
                    else -> throw IOException("scan the PC's pairing QR first")
                }
                pointer = try {
                    NativeBridge.connect(
                        key, hint, invite,
                        IdentityStore.phoneName(this@TapBridgeService),
                        currentPushToken() ?: "",
                    )
                } finally { key.fill(0) }
                if (!wantsConnection || requested.get() != attempt) return@execute
                // Persist the pairing before any session use; cache the dial
                // hint on THIS host's record.
                val acceptedHost = hint.substringBefore(";")
                IdentityStore.saveAcceptedHost(
                    this@TapBridgeService,
                    acceptedHost,
                    NativeBridge.hostName(pointer) ?: "PC",
                    NativeBridge.hostPlatform(pointer).coerceAtLeast(0),
                    NativeBridge.hostPivLogin(pointer),
                )
                targetHost = acceptedHost
                hostPlatform = NativeBridge.hostPlatform(pointer)
                hostPivLogin = NativeBridge.hostPivLogin(pointer)
                NativeBridge.usedHint(pointer)?.let {
                    IdentityStore.saveHint(this@TapBridgeService, acceptedHost, it)
                }
                val epoch = gate.install(pointer)
                pointer = 0L
                session.bindHandle(epoch)
                // Sync: every record, forgotten ones too, before anything else.
                Cards.records(this@TapBridgeService).forEach { session.sendCardInfo(it) }
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

    /**
     * The install's current FCM token, asked from Firebase on every connect
     * (onNewToken only fires when the token changes, so a cached copy alone
     * can be missing). Runs on the lifecycle worker, never the main thread.
     * Falls back to the cached token; null when push is unavailable.
     */
    private fun currentPushToken(): String? {
        val fresh = runCatching {
            com.google.android.gms.tasks.Tasks.await(
                com.google.firebase.messaging.FirebaseMessaging.getInstance().token,
                5, TimeUnit.SECONDS,
            )
        }.onFailure {
            // Never log the token; the failure type is enough to diagnose.
            android.util.Log.w("tapbridge", "FCM token unavailable: ${it.javaClass.simpleName}: ${it.message}")
        }.getOrNull()
        if (!fresh.isNullOrBlank()) {
            IdentityStore.savePushToken(this, fresh)
            return fresh
        }
        return IdentityStore.pushToken(this)
    }

    /** Visible retry backoff: 0.25/0.5/1/2/3s cap. Wake attempts retry once. */
    private fun scheduleRetry(attempt: Long) {
        if (!wantsConnection || destroyed || usbBusy) return
        val delay = when {
            // Visible app: keep trying, fast.
            visible -> RETRY_DELAYS_MS[retryAttempt.coerceAtMost(RETRY_DELAYS_MS.size - 1)].also { retryAttempt++ }
            // FCM wake: one more try, then give up until the next wake.
            !startedFromUi -> if (retryAttempt++ < 1) 1000L else return
            // UI-started session whose app is no longer on screen: nothing
            // may keep running at rest. The next app open or wake reconnects.
            else -> {
                main.post { stopSession(); stopSelf() }
                return
            }
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

    private fun readLoop(epoch: Long, attempt: Long, usb: Boolean = false) {
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
            override fun authState(state: Int) {
                bumpActivity()
                val value = AuthState.fromWire(state)
                main.post { if (requested.get() == attempt) setAuthState(value) }
            }
            override fun cardInfo(
                uid: ByteArray, label: String, labelAt: Long,
                wake: Boolean, wakeAt: Long, caps: Int, capsAt: Long,
                login: Boolean, loginAt: Long, deletedAt: Long,
            ) {
                val theirs = CardInfo(uid, label, labelAt, wake, wakeAt, caps, capsAt, login, loginAt, deletedAt)
                Cards.merge(this@TapBridgeService, theirs)?.let { session.sendCardInfo(it) }
            }
            override fun cardActionDone(uid: ByteArray, action: Int, ok: Boolean) {
                if (action != NativeBridge.ACTION_SETUP_LOGIN) return
                main.post { listener?.onLoginDone(uid, ok) }
            }
            override fun cardAction(uid: ByteArray, action: Int) {
                bumpActivity()
                if (action != NativeBridge.ACTION_SETUP_CERTS) {
                    session.sendCardAction(uid, action, done = true, ok = false)
                    return
                }
                main.post {
                    if (requested.get() != attempt) return@post
                    val ui = listener
                    if (visible && ui != null) {
                        ui.onSetupCerts(uid)
                    } else {
                        // The setup runs in the app, and Android won't let a
                        // service open it: ask with a notification and keep
                        // the request for when the app comes up.
                        pendingPivSetup = uid
                        notifyPivSetup()
                    }
                }
            }
            override fun readCard() {
                bumpActivity()
                main.post {
                    val present = session.hasTagPublic()
                    listener?.onCardPresent(present)
                    if (!present) requestTap()
                }
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
        // The session is over and its sign-in state with it, even when a
        // reconnect already started (the old PC's PIN prompt must not carry
        // over). A wake's tap request outlives a reconnect, so only this
        // attempt's end clears it.
        main.post {
            clearAuthState()
            if (requested.get() == attempt) setTapRequested(false)
        }
        if (destroyed) return
        lifecycle.execute {
            if (requested.get() == attempt) {
                cleanup()
                if (usb) {
                    // The PC ended the link (or it failed) while still
                    // plugged in: don't reopen this plug-in; go back to the
                    // network path.
                    releaseUsbHandles()
                    main.post {
                        if (requested.get() != attempt) return@post
                        usbEnded = usbAccessory
                        usbAccessory = null
                        show(if (error == null) Status.IDLE else Status.ERROR, error)
                        resumeAfterUsb()
                    }
                } else {
                    show(if (error == null) Status.IDLE else Status.ERROR, error)
                    scheduleRetry(attempt)
                }
            }
        }
    }

    /**
     * Serves a permitted tapbridge accessory (MainActivity checked the USB
     * permission; a USB_ACCESSORY_ATTACHED launch carries it). Replaces any
     * network session: one session at a time. Main thread.
     */
    @Synchronized
    fun connectUsb(accessory: UsbAccessory): Boolean {
        if (destroyed || !UsbLink.isOurs(accessory.manufacturer, accessory.model)) return false
        if (usbAccessory != null) return usbAccessory == accessory
        if (usbEnded == accessory) return false
        usbAccessory = accessory
        wantsConnection = true
        val attempt = requested.incrementAndGet()
        show(Status.CONNECTING)
        main.removeCallbacks(idleRunnable)
        lifecycle.execute {
            cleanup()
            releaseUsbHandles()
            if (requested.get() != attempt) return@execute
            var pointer = 0L
            try {
                NativeBridge.initialize(applicationContext)
                val pfd = getSystemService(UsbManager::class.java)?.openAccessory(accessory)
                    ?: throw IOException("couldn't open the USB connection; unplug and plug in again")
                usbPfd = pfd
                val key = IdentityStore.getOrCreateKey(this@TapBridgeService)
                pointer = try { NativeBridge.usbOpen(pfd.fd, key) } finally { key.fill(0) }
                if (requested.get() != attempt) return@execute
                val hostId = NativeBridge.hostId(pointer) ?: throw IOException("USB handshake failed")
                val hostName = NativeBridge.hostName(pointer) ?: "PC"
                val paired = IdentityStore.pairedHosts(this@TapBridgeService).map { it.hostId }
                when (val decision = UsbLink.decide(paired, hostId, hostName, NativeBridge.pairingOpen(pointer))) {
                    is UsbLink.Decision.Reconnect -> {
                        finishUsb(pointer, attempt, decision.hostId, usePairing = false)
                        pointer = 0L
                    }
                    is UsbLink.Decision.AskToPair -> {
                        usbPendingPointer = pointer
                        pointer = 0L
                        val prompt = UsbPrompt(decision.hostId, decision.hostName)
                        main.post {
                            if (requested.get() != attempt) return@post
                            usbPrompt = prompt
                            listener?.onUsbPairRequest(prompt)
                            main.removeCallbacks(usbPromptTimeout)
                            main.postDelayed(usbPromptTimeout, USB_PAIR_PROMPT_MS)
                        }
                    }
                    is UsbLink.Decision.OpenPairingFirst ->
                        throw IOException(UsbLink.openPairingFirstMessage(decision.hostName))
                }
            } catch (error: Exception) {
                if (pointer != 0L) { NativeBridge.close(pointer); NativeBridge.release(pointer); pointer = 0L }
                if (requested.get() == attempt) failUsb(attempt, error.message ?: "USB connection failed")
            } finally {
                // Superseded (unplugged, stopped) between steps.
                if (pointer != 0L) { NativeBridge.close(pointer); NativeBridge.release(pointer) }
            }
        }
        return true
    }

    /**
     * The user's answer to the USB pairing question. Yes: Hello with the
     * PC's capability; no (or no answer): the cable stays unused until it
     * is plugged in again. Main thread.
     */
    fun answerUsbPairing(pair: Boolean) {
        main.removeCallbacks(usbPromptTimeout)
        val prompt = usbPrompt ?: return
        usbPrompt = null
        listener?.onUsbPairRequest(null)
        val attempt = requested.get()
        lifecycle.execute {
            val pointer = usbPendingPointer
            usbPendingPointer = 0L
            if (pointer == 0L || requested.get() != attempt) {
                if (pointer != 0L) { NativeBridge.close(pointer); NativeBridge.release(pointer) }
                return@execute
            }
            if (!pair) {
                NativeBridge.close(pointer); NativeBridge.release(pointer)
                failUsb(attempt, null)
                return@execute
            }
            try {
                finishUsb(pointer, attempt, prompt.hostId, usePairing = true)
            } catch (error: Exception) {
                if (requested.get() == attempt) failUsb(attempt, error.message ?: "pairing failed")
            }
        }
    }

    /**
     * Lifecycle thread. Hello/Accepted on a handshaken USB handle, then the
     * same session setup as a network connect. Takes ownership of [pointer]
     * (released on failure).
     */
    private fun finishUsb(pointer: Long, attempt: Long, hostId: String, usePairing: Boolean) {
        var owned = pointer
        try {
            NativeBridge.usbHello(
                owned, usePairing,
                IdentityStore.phoneName(this),
                // No network on the cable: the cached token, never a fetch.
                IdentityStore.pushToken(this) ?: "",
            )
            if (requested.get() != attempt) return
            val platform = NativeBridge.hostPlatform(owned)
            val pivLogin = NativeBridge.hostPivLogin(owned)
            // Persist the pairing before any session use. The bare host id is
            // the hint, so network wakes and reconnects can find this PC.
            IdentityStore.saveAcceptedHost(
                this, hostId, NativeBridge.hostName(owned) ?: "PC", platform.coerceAtLeast(0), pivLogin,
            )
            UsbLink.hintToSave(IdentityStore.cachedHint(this, hostId), hostId)?.let {
                IdentityStore.saveHint(this, hostId, it)
            }
            targetHost = hostId
            hostPlatform = platform
            hostPivLogin = pivLogin
            viaUsb = true
            val epoch = gate.install(owned)
            owned = 0L
            session.bindHandle(epoch)
            Cards.records(this).forEach { session.sendCardInfo(it) }
            show(Status.CONNECTED)
            reader = Thread({ readLoop(epoch, attempt, usb = true) }, "tapbridge-usb-reader").apply { start() }
        } finally {
            if (owned != 0L) { NativeBridge.close(owned); NativeBridge.release(owned) }
        }
    }

    /** Lifecycle thread: the USB attempt ended without a session. */
    private fun failUsb(attempt: Long, error: String?) {
        releaseUsbHandles()
        main.post {
            if (requested.get() != attempt) return@post
            usbEnded = usbAccessory
            usbAccessory = null
            show(if (error == null) Status.IDLE else Status.ERROR, error)
            if (error != null) listener?.onUsbNotice(error)
            // The refusal message stays up; the network path still resumes.
            resumeAfterUsb(keepError = error != null)
        }
    }

    /** The cable was unplugged: end the USB session, resume the network. Main thread. */
    private fun onUsbDetached() {
        usbEnded = null
        if (usbAccessory == null) return
        usbAccessory = null
        val attempt = requested.incrementAndGet()
        main.removeCallbacks(usbPromptTimeout)
        if (usbPrompt != null) { usbPrompt = null; listener?.onUsbPairRequest(null) }
        gate.closeIfActive(); session.close()
        main.post { clearAuthState(); setTapRequested(false) }
        lifecycle.execute {
            cleanup()
            releaseUsbHandles()
            main.post {
                if (requested.get() != attempt) return@post
                show(Status.IDLE)
                resumeAfterUsb()
            }
        }
    }

    /**
     * After a USB session (or attempt): an open app reconnects over the
     * network as usual; nothing runs at rest otherwise. Main thread.
     */
    private fun resumeAfterUsb(keepError: Boolean = false) {
        if (destroyed) return
        retryAttempt = 0
        when {
            visible && IdentityStore.pairedHosts(this).isNotEmpty() -> {
                targetHost = null
                wakeHint = null
                registerNetworkRetry()
                if (!keepError) connectAsync()
                else main.postDelayed({ if (!usbBusy && status == Status.ERROR) connectAsync() }, FAILED_HOLD_MS)
            }
            visible -> {}
            else -> { stopSession(); stopSelf() }
        }
    }

    /** Lifecycle thread: closes an unanswered USB handle and the accessory PFD. */
    private fun releaseUsbHandles() {
        viaUsb = false
        val pending = usbPendingPointer
        usbPendingPointer = 0L
        if (pending != 0L) { NativeBridge.close(pending); NativeBridge.release(pending) }
        usbPfd?.let { runCatching { it.close() } }
        usbPfd = null
    }

    /** Idle timeout: 60 s after the last card/APDU activity, disconnect. */
    private val activity = AtomicLong(0)
    private val idleRunnable = object : Runnable {
        override fun run() {
            if (destroyed) return
            val idleFor = SystemClock.elapsedRealtime() - activity.get()
            when {
                // A visible app or an attached cable keeps the session.
                visible || usbBusy -> main.postDelayed(this, IDLE_TIMEOUT_MS)
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
                main.post {
                    val present = session.hasTagPublic()
                    listener?.onCardPresent(present)
                    if (present) setTapRequested(false)
                    updateNotice()
                }
            }
        } catch (_: RejectedExecutionException) { session.close() }
    }

    /** A PC's smart-card setup request the app hasn't shown yet. Main thread. */
    private var pendingPivSetup: ByteArray? = null

    /** Hands the app a waiting smart-card setup request once. Main thread. */
    fun takePendingPivSetup(): ByteArray? {
        val uid = pendingPivSetup ?: return null
        pendingPivSetup = null
        getSystemService(NotificationManager::class.java)?.cancel(PIV_SETUP_NOTIFICATION_ID)
        return uid
    }

    private fun notifyPivSetup() {
        ensureChannels(this)
        val launch = PendingIntent.getActivity(
            this, 1,
            Intent(this, MainActivity::class.java)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val pc = targetHost?.let { IdentityStore.hostById(this, it)?.name }
        val notification = Notification.Builder(this, CHANNEL_TAP_REQUEST)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(getString(R.string.setup_piv_title))
            .setContentText(
                pc?.let { getString(R.string.setup_piv_body_named, it) } ?: getString(R.string.setup_piv_body),
            )
            .setContentIntent(launch)
            .setAutoCancel(true)
            .setTimeoutAfter(WAKE_WINDOW_MS)
            .build()
        runCatching {
            getSystemService(NotificationManager::class.java)?.notify(PIV_SETUP_NOTIFICATION_ID, notification)
        }
    }

    /** The PC-requested certificate setup for [uid] ended (the app's PIV flow). */
    fun reportSetupCerts(uid: ByteArray, ok: Boolean) {
        if (status == Status.CONNECTED) session.sendCardAction(uid, NativeBridge.ACTION_SETUP_CERTS, done = true, ok = ok)
    }

    /** Asks the PC on the live session to pair [uid] with its login. False when it can't. */
    fun requestLogin(uid: ByteArray): Boolean {
        if (status != Status.CONNECTED || !hostPivLogin) return false
        session.sendCardAction(uid, NativeBridge.ACTION_SETUP_LOGIN, done = false, ok = false)
        return true
    }

    fun resumeTagDiscovery() { session.paused = false }
    fun pauseTagDiscovery() { session.paused = true; session.close(); main.post { listener?.onCardPresent(false) } }
    fun retireTag() = pauseTagDiscovery()

    @Synchronized
    fun stopSession() {
        // First: the state resets below must not re-post a notification
        // that stopForeground is about to remove.
        notice = null
        wantsConnection = false
        requested.incrementAndGet()
        // Stopping ends a USB session too; this plug-in is not reopened.
        usbAccessory?.let { usbEnded = it }
        usbAccessory = null
        main.removeCallbacks(usbPromptTimeout)
        if (usbPrompt != null) { usbPrompt = null; listener?.onUsbPairRequest(null) }
        gate.closeIfActive(); session.close(); nfcWorker.queue.clear()
        closeWakeWindow()
        setTapRequested(false)
        setAuthState(AuthState.IDLE)
        unregisterNetworkRetry()
        main.removeCallbacks(idleRunnable)
        show(Status.IDLE)
        lifecycle.execute { cleanup(); releaseUsbHandles() }
        stopForeground(STOP_FOREGROUND_REMOVE)
        // Covers the plain-notification fallback in foreground().
        getSystemService(NotificationManager::class.java)?.cancel(FOREGROUND_ID)
    }

    /** Releases the current native session only; network retry and the idle
     *  timer belong to the service lifetime (stopSession), not one attempt. */
    private fun cleanup() {
        gate.closeIfActive(); session.close()
        reader?.let { if (it !== Thread.currentThread()) it.join(5000) }
        reader = null
        gate.releaseIfActive(); session.unbindHandle()
    }

    /** Marks a tap request for [WAKE_WINDOW_MS]; main thread. */
    private fun beginTapRequest() {
        main.removeCallbacks(endTapRequestRunnable)
        main.postDelayed(endTapRequestRunnable, WAKE_WINDOW_MS)
        setTapRequested(true)
    }

    /**
     * A connected PC wants the card (ReadCard, or the card left mid-auth).
     * Visible: the tap art (and its haptic) is enough. Hidden: reader mode
     * is off, so open the tap gate like an FCM wake does; [updateNotice]
     * (via setTapRequested) raises the tap-request notification. Main thread.
     */
    private fun requestTap() {
        if (visible) beginTapRequest() else openWakeWindow()
    }

    private fun setTapRequested(value: Boolean) {
        if (!value) main.removeCallbacks(endTapRequestRunnable)
        if (tapRequested == value) return
        tapRequested = value
        main.post {
            listener?.onTapRequested(value)
            updateNotice()
        }
    }

    /**
     * Applies the PC's sign-in state. SUCCESS and FAILED are final for this
     * sign-in: they end the tap request and clear themselves after a short
     * hold, so a lost IDLE frame can't leave the art stuck. REMOVED asks for
     * the card again. Main thread.
     */
    private fun setAuthState(value: AuthState) {
        main.removeCallbacks(clearAuthRunnable)
        when (value) {
            AuthState.SUCCESS -> { setTapRequested(false); main.postDelayed(clearAuthRunnable, SUCCESS_HOLD_MS) }
            AuthState.FAILED -> { setTapRequested(false); main.postDelayed(clearAuthRunnable, FAILED_HOLD_MS) }
            AuthState.IDLE -> setTapRequested(false)
            AuthState.REMOVED -> if (!session.hasTagPublic()) requestTap()
            AuthState.WORKING, AuthState.PIN -> {}
        }
        if (authState != value) {
            authState = value
            listener?.onAuthState(value)
        }
        updateNotice()
    }

    /** Drops the sign-in state without touching the tap request. Main thread. */
    private fun clearAuthState() {
        main.removeCallbacks(clearAuthRunnable)
        if (authState == AuthState.IDLE) return
        authState = AuthState.IDLE
        listener?.onAuthState(AuthState.IDLE)
        updateNotice()
    }

    /** The held tag was dropped (presence loss, I/O failure, cancel). */
    private fun onCardGone() {
        if (session.hasTagPublic()) return // a new tag already replaced it
        listener?.onCardPresent(false)
        when (authState) {
            // Success art returns to idle once the card leaves.
            AuthState.SUCCESS -> setAuthState(AuthState.IDLE)
            // REMOVED arrived while we still thought the card was there.
            AuthState.REMOVED -> requestTap()
            else -> updateNotice()
        }
    }

    /** Wake window: tap gate enabled for 120 s, disabled on session stop.
     *  [restartRequest] false keeps the current request's own 120 s timer. */
    private fun openWakeWindow(restartRequest: Boolean = true) {
        if (restartRequest) beginTapRequest()
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
        Cards.removeListener(cardSync)
        if (usbReceiverRegistered) runCatching { unregisterReceiver(usbReceiver) }
        destroyed = true
        stopSession()
        listener = null
        lifecycle.execute { cleanup(); releaseUsbHandles(); nfcWorker.shutdownNow(); session.shutdownWriter() }
        lifecycle.shutdown()
        super.onDestroy()
    }
}
