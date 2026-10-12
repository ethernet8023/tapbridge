package computer.ethernet.tapbridge

import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.ServiceConnection
import android.content.pm.PackageManager
import android.hardware.usb.UsbAccessory
import android.hardware.usb.UsbManager
import android.nfc.NfcAdapter
import android.nfc.Tag
import android.os.Build
import android.os.Bundle
import android.os.IBinder
import android.provider.Settings
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.scaleIn
import androidx.compose.animation.togetherWith
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.core.view.WindowCompat
import computer.ethernet.tapbridge.ui.HomeScreen
import computer.ethernet.tapbridge.ui.InstallerScreen
import computer.ethernet.tapbridge.ui.AddCardDialogs
import computer.ethernet.tapbridge.ui.CardsScreen
import computer.ethernet.tapbridge.ui.LoginTarget
import computer.ethernet.tapbridge.ui.PendingCard
import computer.ethernet.tapbridge.ui.PairScreen
import computer.ethernet.tapbridge.ui.PivPrompt
import computer.ethernet.tapbridge.ui.PivSetupDialogs
import computer.ethernet.tapbridge.ui.SettingsScreen
import computer.ethernet.tapbridge.ui.theme.LocalReducedMotion
import computer.ethernet.tapbridge.ui.theme.TapBridgeTheme
import java.util.concurrent.Executors

/**
 * Single-activity Compose app. Screens: home (status + tap hero), pair (QR),
 * keys, settings. NFC reader mode is enabled only while resumed.
 */
class MainActivity : ComponentActivity() {
    private var service: TapBridgeService? = null
    private var bound = false
    private var resumed = false
    private var nfc: NfcAdapter? = null
    /** Tag forwarded by TapGate, held until the activity is resumed AND the
     *  service is bound (cold start binds asynchronously; onNewIntent runs
     *  before onResume while discovery is still paused). */
    private var forwardedTag: Tag? = null

    // ------------------------------------------------------------ USB link
    private val usbPermissionAction get() = "$packageName.USB_PERMISSION"
    /** A USB permission request is showing; don't ask twice. */
    private var usbPermissionPending = false
    private val usbReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            if (intent.action != usbPermissionAction) return
            usbPermissionPending = false
            @Suppress("DEPRECATION")
            val accessory = intent.getParcelableExtra<UsbAccessory>(UsbManager.EXTRA_ACCESSORY)
            if (intent.getBooleanExtra(UsbManager.EXTRA_PERMISSION_GRANTED, false) && accessory != null) {
                startUsbService(accessory)
            } else {
                state?.usbNotice = "USB access was declined. Unplug the cable and plug it in again to retry."
                if (IdentityStore.pairedHosts(this@MainActivity).isNotEmpty()) startTapService()
            }
        }
    }

    /** Activity-scoped observable state. */
    class UiState {
        var status by mutableStateOf(TapBridgeService.Status.IDLE)
        var error by mutableStateOf<String?>(null)
        var cardPresent by mutableStateOf(false)
        /** Host id of the PC with a live session right now, else null. */
        var connectedHost by mutableStateOf<String?>(null)
        /** A PC asked for a tap; only then does the home screen animate. */
        var tapRequested by mutableStateOf(false)
        /** The PC's sign-in state (Auth frames); IDLE without a session. */
        var authState by mutableStateOf(AuthState.IDLE)
        var notificationsGranted by mutableStateOf(false)
        var dndGranted by mutableStateOf(false)
        var addKeyMode by mutableStateOf(false)
        var addKeyError by mutableStateOf<String?>(null)
        /** A tapped card that isn't one of the user's yet, being named. */
        var pendingCard by mutableStateOf<PendingCard?>(null)
        /** The user's cards (Cards.list), refreshed on every change. */
        var cards by mutableStateOf<List<CardInfo>>(emptyList())
        /** OS of the PC on the live session (NativeBridge.PLATFORM_*), -1 without one. */
        var hostPlatform by mutableStateOf(-1)
        /** The PC on the live session can pair a card with its login. */
        var hostPivLogin by mutableStateOf(false)
        /** Hex UID of the card whose login is being set up on the PC. */
        var loginBusy by mutableStateOf<String?>(null)
        /** How the last login setup request ended, shown once. */
        var loginResult by mutableStateOf<String?>(null)
        /** Phone-side PIV setup offer/flow; null when none is showing. */
        var pivPrompt by mutableStateOf<PivPrompt?>(null)
        /** A pairing code opened as a tapbridge: link, waiting for "Pair". */
        var linkInvitation by mutableStateOf<String?>(null)
        /** The live session runs over the USB cable. */
        var viaUsb by mutableStateOf(false)
        /** An unpaired PC on the USB cable offers pairing, waiting for "Pair". */
        var usbPrompt by mutableStateOf<TapBridgeService.UsbPrompt?>(null)
        /** Why the USB link was refused or failed, shown once. */
        var usbNotice by mutableStateOf<String?>(null)
    }

    /** A new card's PIV state, offered for setup once the card is named. */
    private var pendingPiv: Piv.Status? = null

    private val cardsListener = Cards.Listener { _, _ ->
        runOnUiThread { state?.cards = Cards.list(this) }
    }
    private val pivWorker = Executors.newSingleThreadExecutor { r -> Thread(r, "piv").apply { isDaemon = true } }

    private var state: UiState? = null

    /** Runtime notification permission launcher, shared with the Settings screen. */
    var requestPermissionLauncher: androidx.activity.result.ActivityResultLauncher<String>? = null
        private set

    private val serviceListener = object : TapBridgeService.Listener {
        override fun onStatusChanged(status: TapBridgeService.Status, error: String?) {
            runOnUiThread {
                state?.let {
                    it.status = status
                    it.error = error
                    val connected = status == TapBridgeService.Status.CONNECTED
                    it.connectedHost = if (connected) service?.connectedHost else null
                    it.hostPlatform = if (connected) service?.hostPlatform ?: -1 else -1
                    it.hostPivLogin = connected && service?.hostPivLogin == true
                    it.viaUsb = connected && service?.viaUsb == true
                    if (!connected) it.loginBusy = null
                }
            }
        }
        override fun onCardPresent(present: Boolean) {
            runOnUiThread { state?.let { it.cardPresent = present } }
        }
        override fun onTapRequested(requested: Boolean) {
            runOnUiThread { state?.let { it.tapRequested = requested } }
        }
        override fun onAuthState(state: AuthState) {
            runOnUiThread { this@MainActivity.state?.let { it.authState = state } }
        }
        override fun onSetupCerts(uid: ByteArray) {
            runOnUiThread { startHostPivSetup(uid) }
        }
        override fun onUsbPairRequest(prompt: TapBridgeService.UsbPrompt?) {
            runOnUiThread { state?.usbPrompt = prompt }
        }
        override fun onUsbNotice(text: String) {
            runOnUiThread { state?.usbNotice = text }
        }
        override fun onLoginDone(uid: ByteArray, ok: Boolean) {
            runOnUiThread {
                val ui = state ?: return@runOnUiThread
                if (ui.loginBusy != Cards.hex(uid)) return@runOnUiThread
                ui.loginBusy = null
                val name = Cards.get(this@MainActivity, uid)?.label ?: "The card"
                val pc = PairedHosts.osName(ui.hostPlatform)
                ui.loginResult = if (ok) "\u201C$name\u201D now unlocks your $pc." else "Login wasn't set up. Check your $pc for details."
            }
        }
    }

    private val conn = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName?, binder: IBinder?) {
            service = (binder as TapBridgeService.LocalBinder).service
            bound = true
            service?.listener = serviceListener
            if (resumed) service?.resumeTagDiscovery() else service?.pauseTagDiscovery()
            // onResume ran before the bind finished on a cold start; tell the
            // service now, or the idle timeout treats the open app as hidden.
            service?.onAppVisibility(resumed)
            state?.status = service?.status ?: TapBridgeService.Status.IDLE
            state?.error = service?.lastError
            state?.tapRequested = service?.tapRequested == true
            state?.authState = service?.authState ?: AuthState.IDLE
            state?.connectedHost = service?.takeIf { it.status == TapBridgeService.Status.CONNECTED }?.connectedHost
            state?.hostPlatform = service?.takeIf { it.status == TapBridgeService.Status.CONNECTED }?.hostPlatform ?: -1
            state?.hostPivLogin = service?.takeIf { it.status == TapBridgeService.Status.CONNECTED }?.hostPivLogin == true
            state?.viaUsb = service?.takeIf { it.status == TapBridgeService.Status.CONNECTED }?.viaUsb == true
            state?.usbPrompt = service?.usbPrompt
            deliverForwardedTag()
            if (resumed) service?.takePendingPivSetup()?.let { startHostPivSetup(it) }
        }
        override fun onServiceDisconnected(name: ComponentName?) {
            service = null
            bound = false
        }
    }

    private val readerCallback = NfcAdapter.ReaderCallback { tag ->
        val ui = state
        when {
            ui?.pivPrompt is PivPrompt.Waiting -> pivWorker.execute { runPivSetup(tag) }
            ui?.pivPrompt is PivPrompt.Scan -> pivWorker.execute { scanPiv(tag) }
            ui?.addKeyMode == true -> pivWorker.execute { handleRegistrationTag(tag) }
            // No PC is using the card: read what it supports (and offer to
            // add it when it's new) before handing the tag on. Never during
            // a PC session, where extra SELECTs would disturb the PC's
            // exchange with the card.
            ui != null && ui.pivPrompt == null && ui.pendingCard == null && !ui.tapRequested &&
                ui.status != TapBridgeService.Status.CONNECTED -> pivWorker.execute {
                    readIdle(tag)
                    service?.onTagDiscovered(tag)
                }
            else -> service?.onTagDiscovered(tag)
        }
    }

    /**
     * Reads what the tapped card supports. A known card's caps are updated
     * (and so synced); a new one is offered to add.
     */
    private fun readIdle(tag: Tag) {
        val uid = tag.id?.takeIf { Cards.uidStable(it) && it.size <= Cards.MAX_UID } ?: return
        val read = readCard(tag) ?: return
        if (Cards.get(this, uid) != null) {
            Cards.noteRead(this, uid, read)
            return
        }
        runOnUiThread {
            val ui = state ?: return@runOnUiThread
            if (ui.pendingCard != null || ui.pivPrompt != null || ui.addKeyMode) return@runOnUiThread
            pendingPiv = read.piv
            ui.pendingCard = PendingCard(uid, read.caps, Cards.defaultLabel(this))
        }
    }

    /** [CardRead] on its own connection; null when the card moved away. */
    private fun readCard(tag: Tag): CardRead.Result? {
        val iso = android.nfc.tech.IsoDep.get(tag) ?: return null
        return try {
            iso.connect()
            iso.timeout = 5000
            CardRead.read(iso)
        } catch (_: Exception) {
            null
        } finally {
            runCatching { iso.close() }
        }
    }

    /** Stores a fresh PIV probe in the card's caps (FIDO bit kept). */
    private fun notePiv(uid: ByteArray?, status: Piv.Status?) {
        val card = uid?.let { Cards.get(this, it) } ?: return
        Cards.setCaps(this, card.uid, CardRead.withPiv(card.caps, status))
    }

    /** Your cards -> "Set up" certificates for this card. */
    internal fun startPivSetup(card: CardInfo) {
        pivUid = card.uid
        pivFromPc = false
        state?.pivPrompt = PivPrompt.Scan
    }

    /** Your cards -> "Set up" login: the connected PC pairs it. */
    internal fun requestLogin(card: CardInfo) {
        val ui = state ?: return
        if (service?.requestLogin(card.uid) == true) {
            ui.loginBusy = Cards.hex(card.uid)
        } else {
            ui.loginResult = "Connect to a computer that supports card login first."
        }
    }

    /**
     * Stable UID of the card being set up, when the setup is for one card
     * (asked by a PC, or picked in Your cards). [pivFromPc]: a PC asked
     * (CardAction), so its outcome goes back to the PC once, through
     * [finishHostPiv].
     */
    private var pivUid: ByteArray? = null
    private var pivFromPc = false

    /** A PC asked to set up this card's certificates: the same flow as Your cards. */
    private fun startHostPivSetup(uid: ByteArray) {
        val ui = state ?: return
        // Never interrupt a setup or card naming already on screen.
        if (ui.pivPrompt != null || ui.addKeyMode || ui.pendingCard != null) {
            service?.reportSetupCerts(uid, false)
            return
        }
        pivUid = uid
        pivFromPc = true
        ui.pivPrompt = PivPrompt.Scan
    }

    /** Reports a PC-requested setup's outcome, once. Main thread. */
    private fun finishHostPiv(ok: Boolean) {
        val uid = pivUid
        val fromPc = pivFromPc
        pivUid = null
        pivFromPc = false
        if (fromPc && uid != null) service?.reportSetupCerts(uid, ok)
    }

    /**
     * Reads the tapped key's PIV state for a manual setup and shows it
     * (Review). Nothing is written yet: filling empty slots or replacing
     * certificates are separate, explicit choices from there.
     */
    private fun scanPiv(tag: Tag) {
        val iso = android.nfc.tech.IsoDep.get(tag)
        val status = try {
            iso?.connect()
            iso?.timeout = 5000
            iso?.let { card -> Piv.probe { card.transceive(it) } }
        } catch (_: Exception) {
            null
        } finally {
            runCatching { iso?.close() }
        }
        val wanted = pivUid
        val tapped = tag.id?.takeIf { Cards.uidStable(it) }
        val next = when {
            wanted != null && (tapped == null || !tapped.contentEquals(wanted)) ->
                PivPrompt.Failed(
                    if (pivFromPc) "That's a different card from the one you picked on your computer."
                    else "That's a different card from \u201C${Cards.get(this, wanted)?.label ?: "the one you picked"}\u201D.",
                )
            iso == null -> PivPrompt.Failed("That isn't a smart card.")
            status == null -> PivPrompt.Failed("This key has no smart-card (PIV) applet, or it moved away too soon.")
            else -> {
                notePiv(tapped, status)
                PivPrompt.Review(status)
            }
        }
        runOnUiThread {
            if (state?.pivPrompt == PivPrompt.Scan) {
                state?.pivPrompt = next
                if (next is PivPrompt.Failed) finishHostPiv(false)
            }
        }
    }

    internal fun endPiv() {
        // Closed before it finished: the PC stops waiting. A review that
        // found the certificates already there counts as done.
        val prompt = state?.pivPrompt
        finishHostPiv(prompt is PivPrompt.Done || (prompt is PivPrompt.Review && !prompt.status.needsSetup))
        state?.pivPrompt = null
    }

    /** The PIN only lives in the prompt state, so ending the flow drops it. */
    internal fun submitPiv(form: PivPrompt.Form) {
        if (state?.pivPrompt !is PivPrompt.Form) return
        state?.pivPrompt = PivPrompt.Waiting(form)
    }

    /**
     * Runs on [pivWorker] once the key is held to the phone again. One
     * attempt per tap, never retried on its own: a wrong PIN goes back to
     * the form with the new count, and Piv.setUp itself refuses to send a
     * PIN at MIN_PIN_RETRIES or fewer tries.
     */
    private fun runPivSetup(tag: Tag) {
        val form = (state?.pivPrompt as? PivPrompt.Waiting)?.form ?: return
        val pin = form.pin
        val key = Piv.parseManagementKey(form.managementKey) ?: return
        runOnUiThread { state?.pivPrompt = PivPrompt.Working }
        val iso = android.nfc.tech.IsoDep.get(tag)
        // Debug builds log every PIV exchange (command header + SW only,
        // never data: the VERIFY body is the PIN).
        val transport = Piv.Transport { apdu ->
            val reply = iso!!.transceive(apdu)
            if (BuildConfig.DEBUG && apdu.size >= 4 && reply.size >= 2) {
                android.util.Log.d(
                    "tapbridge-piv",
                    "%02X %02X %02X %02X len=%d -> SW %02X%02X (%d bytes)".format(
                        apdu[0], apdu[1], apdu[2], apdu[3], apdu.size,
                        reply[reply.size - 2], reply[reply.size - 1], reply.size - 2,
                    ),
                )
            }
            reply
        }
        val next: PivPrompt = if (iso == null) {
            PivPrompt.Failed("That isn't a smart card.")
        } else try {
            iso.connect()
            // On-card key generation over NFC takes a few seconds.
            iso.timeout = 20000
            // slots = null: fill whatever is empty on the key actually held
            // now, which may not be the one that got the offer.
            val result = Piv.setUp(
                transport, pin, form.name ?: "",
                slots = form.slots, managementKey = key,
                replace = form.replace, expected = form.status,
            )
            runCatching { Piv.probe(transport) }.getOrNull()?.let { notePiv(tag.id, it) }
            if (result.slots.isEmpty()) PivPrompt.Failed("This key already has its certificates.")
            else PivPrompt.Done(result.slots)
        } catch (changed: Piv.SetupChanged) {
            notePiv(tag.id, changed.status)
            PivPrompt.Changed(changed.status)
        } catch (wrong: Piv.WrongPin) {
            val status = form.status.copy(pinRetries = wrong.retries)
            if (wrong.retries == 0) PivPrompt.Failed("The PIN is blocked. Unblock it with the PUK in yubico-piv-tool.")
            else form.copy(status = status, pin = "", pinError = wrong.message)
        } catch (_: Piv.WrongManagementKey) {
            // Safe to retry: the management key has no retry counter.
            form.copy(keyError = "That management key didn't work. Check it and try again.")
        } catch (low: Piv.PinRetriesTooLow) {
            PivPrompt.Failed(low.message!!)
        } catch (e: android.nfc.TagLostException) {
            PivPrompt.Failed("The key moved away before setup finished. Some slots may be half set up; tap the key again to retry.")
        } catch (e: Exception) {
            PivPrompt.Failed(e.message ?: "Card error")
        } finally {
            runCatching { iso?.close() }
        }
        runOnUiThread {
            state?.pivPrompt = next
            when (next) {
                is PivPrompt.Done -> finishHostPiv(true)
                is PivPrompt.Failed -> finishHostPiv(false)
                else -> {}
            }
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        WindowCompat.setDecorFitsSystemWindows(window, false)
        NativeBridge.initialize(applicationContext)
        nfc = NfcAdapter.getDefaultAdapter(this)
        bindService(Intent(this, TapBridgeService::class.java), conn, Context.BIND_AUTO_CREATE)

        val notificationsLauncher = registerForActivityResult(
            ActivityResultContracts.RequestPermission(),
        ) { granted -> state?.let { it.notificationsGranted = granted } }
        requestPermissionLauncher = notificationsLauncher

        val ui = UiState()
        state = ui
        ui.notificationsGranted = hasNotifications()
        ui.cards = Cards.list(this)
        Cards.addListener(cardsListener)

        setContent {
            val reducedMotion = remember {
                Settings.Global.getFloat(
                    contentResolver,
                    Settings.Global.ANIMATOR_DURATION_SCALE, 1f,
                ) == 0f
            }
            TapBridgeTheme {
                CompositionLocalProvider(LocalReducedMotion provides reducedMotion) {
                    Surface(Modifier.fillMaxSize()) {
                        App(ui = ui, activity = this)
                    }
                }
            }
        }
        val usbFilter = IntentFilter(usbPermissionAction)
        if (Build.VERSION.SDK_INT >= 33) registerReceiver(usbReceiver, usbFilter, RECEIVER_NOT_EXPORTED)
        else @Suppress("UnspecifiedRegisterReceiverFlag") registerReceiver(usbReceiver, usbFilter)
        handleForwardedTag(intent)
        // A recreated activity (rotation, process death) gets the same
        // intent back: the link was already handled then.
        if (savedInstanceState == null) handlePairingLink(intent)
        handleUsbAttached(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handleForwardedTag(intent)
        handlePairingLink(intent)
        handleUsbAttached(intent)
    }

    /**
     * USB_ACCESSORY_ATTACHED: a PC running tapbridge switched the phone to
     * accessory mode. Launching from this intent grants the USB permission.
     */
    private fun handleUsbAttached(intent: Intent?) {
        if (intent?.action != UsbManager.ACTION_USB_ACCESSORY_ATTACHED) return
        @Suppress("DEPRECATION")
        val accessory = intent.getParcelableExtra<UsbAccessory>(UsbManager.EXTRA_ACCESSORY) ?: return
        if (UsbLink.isOurs(accessory.manufacturer, accessory.model)) startUsbService(accessory)
    }

    /**
     * The attached tapbridge accessory, if any, permitted or after asking:
     * an app opened by hand (not by the attach intent) must request the USB
     * permission itself. True when one is attached (the cable then owns the
     * session).
     */
    private fun checkAttachedAccessory(): Boolean {
        val usb = getSystemService(UsbManager::class.java) ?: return false
        val accessory = usb.accessoryList?.firstOrNull { UsbLink.isOurs(it.manufacturer, it.model) } ?: return false
        if (usb.hasPermission(accessory)) {
            startUsbService(accessory)
        } else if (!usbPermissionPending) {
            usbPermissionPending = true
            // The USB service fills in extras, so the PendingIntent is
            // mutable, but scoped to this package and a private action.
            val pending = PendingIntent.getBroadcast(
                this, 0, Intent(usbPermissionAction).setPackage(packageName),
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_MUTABLE,
            )
            usb.requestPermission(accessory, pending)
        }
        return true
    }

    private fun startUsbService(accessory: UsbAccessory) {
        val i = Intent(this, TapBridgeService::class.java)
            .setAction(TapBridgeService.ACTION_USB)
            .putExtra(UsbManager.EXTRA_ACCESSORY, accessory)
        try {
            if (Build.VERSION.SDK_INT >= 26) startForegroundService(i) else startService(i)
        } catch (_: IllegalStateException) {
            // Not visible yet (locked); onResume checks the cable again.
        }
    }

    /** The user's answer to "Pair with this computer?" for the PC on the cable. */
    internal fun answerUsbPairing(pair: Boolean) {
        state?.usbPrompt = null
        service?.answerUsbPairing(pair)
        if (pair && state?.notificationsGranted == false && Build.VERSION.SDK_INT >= 33) {
            requestPermissionLauncher?.launch(android.Manifest.permission.POST_NOTIFICATIONS)
        }
    }

    /**
     * A tapbridge:1:... link (the PC's pairing QR, scanned with the camera).
     * Never pairs on its own: the user confirms in a dialog first, since any
     * page or QR could carry such a link. Handled once: clearing
     * `intent.data` only changes this copy, and Android keeps the task's
     * original intent, so a launch from Recents (or after the app was
     * killed) delivers the old link again. Those launches are ignored.
     */
    private fun handlePairingLink(intent: Intent?) {
        if (intent?.action != Intent.ACTION_VIEW || intent.data?.scheme != "tapbridge") return
        if (intent.flags and Intent.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY != 0) return
        val text = intent.dataString
        intent.data = null
        if (text != null && Invitation.parse(text) != null) state?.linkInvitation = text
    }

    private fun handleForwardedTag(intent: Intent?) {
        val tag = intent?.getParcelableExtra<Tag>(EXTRA_FORWARDED_TAG) ?: return
        intent.removeExtra(EXTRA_FORWARDED_TAG)
        state?.addKeyMode = false
        forwardedTag = tag
        deliverForwardedTag()
    }

    private fun deliverForwardedTag() {
        val tag = forwardedTag ?: return
        val bridge = service ?: return
        if (!resumed) return
        forwardedTag = null
        bridge.resumeTagDiscovery()
        bridge.onTagDiscovered(tag)
    }

    private fun hasNotifications(): Boolean =
        Build.VERSION.SDK_INT < 33 ||
            checkSelfPermission(android.Manifest.permission.POST_NOTIFICATIONS) ==
            PackageManager.PERMISSION_GRANTED

    /** Set when Android refused the foreground start; retried on focus. */
    private var startDeferred = false

    internal fun startTapService() {
        val i = Intent(this, TapBridgeService::class.java)
            .setAction(TapBridgeService.ACTION_START)
        try {
            if (Build.VERSION.SDK_INT >= 26) startForegroundService(i) else startService(i)
            startDeferred = false
        } catch (_: IllegalStateException) {
            // ForegroundServiceStartNotAllowedException: resumed while the
            // device is asleep or locked, so the app does not count as
            // visible yet. The bound service keeps running meanwhile; start
            // it in the foreground once the window really has focus.
            startDeferred = true
        }
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus && startDeferred) startTapService()
    }

    internal fun stopTapService() {
        startService(Intent(this, TapBridgeService::class.java).setAction(TapBridgeService.ACTION_STOP))
    }
    /** Add-card flow (worker thread): read the tapped card, then ask for a name. */
    private fun handleRegistrationTag(tag: Tag) {
        val uid = tag.id?.takeIf { Cards.uidStable(it) && it.size <= Cards.MAX_UID }
        val read = uid?.let { readCard(tag) }
        val known = uid?.let { Cards.get(this, it) }
        runOnUiThread {
            val ui = state ?: return@runOnUiThread
            if (!ui.addKeyMode) return@runOnUiThread
            when {
                uid == null -> ui.addKeyError = "This card has no stable ID, so tapbridge can't tell it apart. Try another card."
                read == null -> ui.addKeyError = "Couldn't read the card. Hold it still and try again."
                known != null -> {
                    Cards.noteRead(this, uid, read)
                    ui.addKeyError = "That's \u201C${known.label}\u201D, already one of your cards."
                }
                else -> {
                    pendingPiv = read.piv
                    ui.pendingCard = PendingCard(uid, read.caps, Cards.defaultLabel(this))
                    ui.addKeyMode = false
                }
            }
        }
    }

    /** Adds the named card; offers the smart-card setup when it has empty slots. */
    internal fun savePendingCard(card: PendingCard, label: String) {
        val ui = state ?: return
        Cards.add(this, card.uid, label, card.caps)
        ui.pendingCard = null
        ui.addKeyMode = false
        val piv = pendingPiv
        pendingPiv = null
        if (piv?.needsSetup == true && piv.contactlessBlocked != true && ui.pivPrompt == null) {
            pivUid = card.uid
            pivFromPc = false
            ui.pivPrompt = PivPrompt.Offer(piv)
        }
    }

    internal fun discardPendingCard() {
        pendingPiv = null
        state?.pendingCard = null
        state?.addKeyMode = false
    }

    override fun onResume() {
        super.onResume()
        resumed = true
        state?.let {
            // Both can change in system settings while the app is away.
            it.notificationsGranted = hasNotifications()
            it.dndGranted = getSystemService(android.app.NotificationManager::class.java)
                .isNotificationPolicyAccessGranted
        }
        service?.resumeTagDiscovery()
        service?.onAppVisibility(true)
        service?.takePendingPivSetup()?.let { startHostPivSetup(it) }
        // A tapbridge USB cable is the session while attached; otherwise
        // (DESIGN) opening the app while paired connects immediately (to
        // the last-used host). USB goes first: the service then ignores the
        // network start.
        val usbAttached = checkAttachedAccessory()
        if (!usbAttached && IdentityStore.pairedHosts(this).isNotEmpty()) {
            startTapService()
        }
        try {
            nfc?.enableReaderMode(
                this, readerCallback,
                NfcAdapter.FLAG_READER_NFC_A
                    or NfcAdapter.FLAG_READER_NFC_B
                    or NfcAdapter.FLAG_READER_SKIP_NDEF_CHECK,
                null,
            )
        } catch (_: IllegalStateException) { /* adapter off */ }
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        deliverForwardedTag()
    }

    override fun onPause() {
        // ALWAYS retire/close the tag when leaving the foreground.
        resumed = false
        service?.pauseTagDiscovery()
        service?.onAppVisibility(false)
        try { nfc?.disableReaderMode(this) } catch (_: IllegalStateException) { /* adapter already off */ }
        window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        super.onPause()
    }

    override fun onDestroy() {
        Cards.removeListener(cardsListener)
        runCatching { unregisterReceiver(usbReceiver) }
        pivWorker.shutdownNow()
        if (bound) {
            service?.listener = null
            unbindService(conn)
            bound = false
        }
        super.onDestroy()
    }

    companion object {
        const val EXTRA_FORWARDED_TAG = "computer.ethernet.tapbridge.FORWARDED_TAG"
    }
}

/** Screen navigation with animated transitions; back returns to home. */
@Composable
private fun App(ui: MainActivity.UiState, activity: MainActivity) {
    val context = LocalContext.current
    var screen by rememberSaveable { mutableStateOf("home") }
    var hosts by remember { mutableStateOf(IdentityStore.pairedHosts(context)) }
    // Bundled by CI only; empty in local builds, which hides the entry.
    val installers = remember { InstallerAssets.names(context) }

    // Pairing from the in-app scanner or a confirmed tapbridge: link.
    val pair: (String) -> Unit = { invitation ->
        PairingState.setPendingInvitation(invitation)
        activity.startTapService()
        // Wake alerts need notifications; ask while the user is setting the
        // app up.
        if (!ui.notificationsGranted && Build.VERSION.SDK_INT >= 33) {
            activity.requestPermissionLauncher?.launch(
                android.Manifest.permission.POST_NOTIFICATIONS,
            )
        }
        screen = "home"
    }
    ui.linkInvitation?.let { invitation ->
        PairConfirmDialog(
            pcName = null,
            onPair = { ui.linkInvitation = null; pair(invitation) },
            onCancel = { ui.linkInvitation = null },
        )
    }
    // An unpaired PC on the USB cable with its pairing window open.
    ui.usbPrompt?.let { prompt ->
        if (ui.linkInvitation == null) {
            PairConfirmDialog(
                pcName = prompt.hostName,
                onPair = { activity.answerUsbPairing(true); screen = "home" },
                onCancel = { activity.answerUsbPairing(false) },
            )
        }
    }
    ui.usbNotice?.let { text ->
        AlertDialog(
            onDismissRequest = { ui.usbNotice = null },
            title = { Text("USB connection") },
            text = { Text(text) },
            confirmButton = { TextButton(onClick = { ui.usbNotice = null }) { Text("OK") } },
        )
    }

    // System back (and predictive back) returns to home from any sub-screen.
    BackHandler(enabled = screen != "home") { screen = if (screen == "installer") "settings" else "home" }

    AnimatedContent(
        targetState = screen,
        transitionSpec = {
            (fadeIn(tween(220)) + scaleIn(tween(220), initialScale = 0.96f))
                .togetherWith(fadeOut(tween(160)))
        },
        label = "screens",
    ) { route ->
        // Edge-to-edge: backgrounds draw behind the system bars, content
        // stays inside them.
        Box(Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing)) {
            when (route) {
                "pair" -> PairScreen(
                    onPaired = { pair(it) },
                    onBack = { screen = "home" },
                    Modifier.fillMaxSize(),
                )
                "cards" -> CardsScreen(
                    cards = ui.cards,
                    login = loginTarget(ui, hosts),
                    loginBusy = ui.loginBusy,
                    onAddCard = { ui.addKeyError = null; ui.addKeyMode = true },
                    onRename = { card, label -> Cards.rename(context, card.uid, label) },
                    onForget = { card -> Cards.forget(context, card.uid) },
                    onWake = { card, wake -> Cards.setWake(context, card.uid, wake) },
                    onSetUpCerts = { card -> activity.startPivSetup(card) },
                    onSetUpLogin = { card -> activity.requestLogin(card) },
                    onBack = { screen = "home" },
                    Modifier.fillMaxSize(),
                )
                "settings" -> SettingsScreen(
                    phoneName = IdentityStore.phoneName(context),
                    pushEnabled = ui.notificationsGranted,
                    dndGranted = ui.dndGranted,
                    onRequestDnd = {
                        activity.startActivity(
                            Intent(android.provider.Settings.ACTION_NOTIFICATION_POLICY_ACCESS_SETTINGS),
                        )
                    },
                    hosts = hosts,
                    connectedHost = ui.connectedHost,
                    onPhoneName = { name ->
                        runCatching { IdentityStore.setPhoneName(context, name) }
                    },
                    onRequestNotifications = {
                        activity.requestPermissionLauncher?.launch(
                            android.Manifest.permission.POST_NOTIFICATIONS,
                        )
                    },
                    onRenameHost = { hostId, name ->
                        runCatching { IdentityStore.renameHost(context, hostId, name) }
                        hosts = IdentityStore.pairedHosts(context)
                    },
                    onForgetHost = { hostId ->
                        // The connection is shared state: stop it when the
                        // host it serves is forgotten, or when nothing is
                        // left to connect to.
                        activity.stopTapService()
                        IdentityStore.forgetHost(context, hostId)
                        hosts = IdentityStore.pairedHosts(context)
                    },
                    onPairAnother = { screen = "pair" },
                    onBack = { screen = "home" },
                    Modifier.fillMaxSize(),
                    onInstallOnPc = if (installers.isEmpty()) null else ({ screen = "installer" }),
                )
                "installer" -> InstallerScreen(
                    installers = installers,
                    onBack = { screen = "settings" },
                    Modifier.fillMaxSize(),
                )
                else -> HomeScreen(
                    ui = ui,
                    onStartPair = { screen = "pair" },
                    onKeys = { hosts = IdentityStore.pairedHosts(context); screen = "cards" },
                    // Pairing completes in the background after the scan,
                    // so read the list fresh each time Settings opens.
                    onSettings = { hosts = IdentityStore.pairedHosts(context); screen = "settings" },
                    Modifier.fillMaxSize(),
                )
            }
        }
    }
    // Dialogs live above the navigation, so they open over whichever screen
    // started them (they used to belong to home and opened behind Keys).
    AddCardDialogs(
        reading = ui.addKeyMode,
        readError = ui.addKeyError,
        pending = ui.pendingCard,
        onCancelRead = { ui.addKeyMode = false; ui.addKeyError = null },
        onSave = { card, label -> activity.savePendingCard(card, label) },
        onDiscard = { activity.discardPendingCard() },
    )
    ui.loginResult?.let { text ->
        AlertDialog(
            onDismissRequest = { ui.loginResult = null },
            title = { Text("Card login") },
            text = { Text(text) },
            confirmButton = { TextButton(onClick = { ui.loginResult = null }) { Text("OK") } },
        )
    }
    // After the add-card dialogs, so a new card is named first.
    if (ui.pendingCard == null && !ui.addKeyMode) {
        PivSetupDialogs(
            prompt = ui.pivPrompt,
            defaultName = IdentityStore.phoneName(context).let { "$it PIV" },
            onStart = {
                (ui.pivPrompt as? PivPrompt.Offer)?.let {
                    ui.pivPrompt = PivPrompt.Form(it.status, pin = defaultPin(it.status))
                }
            },
            onSubmit = { form -> activity.submitPiv(form) },
            onCreate = { status -> ui.pivPrompt = PivPrompt.Form(status, pin = defaultPin(status)) },
            onAskReplace = { status -> ui.pivPrompt = PivPrompt.ConfirmReplace(status) },
            onReplace = { status ->
                ui.pivPrompt = PivPrompt.Form(
                    status, pin = defaultPin(status), slots = Piv.SETUP_SLOTS, replace = true,
                )
            },
            onDismiss = { activity.endPiv() },
        )
    }
}

/**
 * "Pair with this computer?" for a scanned/linked code ([pcName] null) or
 * a PC on the USB cable (named; the name is the PC's own claim, but its
 * identity key was verified by the USB handshake).
 */
@Composable
private fun PairConfirmDialog(pcName: String?, onPair: () -> Unit, onCancel: () -> Unit) {
    AlertDialog(
        onDismissRequest = onCancel,
        title = { Text("Pair with this computer?") },
        text = {
            Text(
                if (pcName != null) {
                    "\u201C$pcName\u201D is asking to pair over the USB cable. " +
                        "Only pair with your own computer. A paired computer can ask to use your cards."
                } else {
                    "Only pair with a code shown by tapbridge on your own computer. " +
                        "A paired computer can ask to use your cards."
                },
            )
        },
        confirmButton = { TextButton(onClick = onPair) { Text("Pair") } },
        dismissButton = { TextButton(onClick = onCancel) { Text("Cancel") } },
    )
}

/**
 * The login step for Your cards: shown when any paired PC supports card
 * login, named after that PC's OS (the connected one when it supports it).
 */
private fun loginTarget(ui: MainActivity.UiState, hosts: List<PairedHosts.Record>): LoginTarget? {
    if (ui.hostPivLogin) return LoginTarget(PairedHosts.osName(ui.hostPlatform), connected = true)
    val capable = hosts.filter { it.pivLogin }.map { it.osName }.distinct()
    return when (capable.size) {
        0 -> null
        1 -> LoginTarget(capable[0], connected = false)
        else -> LoginTarget("computer", connected = false)
    }
}

/**
 * The factory PIN, prefilled only while the card says it is still the
 * default (or doesn't say). A changed PIN is never guessed: a careless
 * Continue would spend a try.
 */
private fun defaultPin(status: Piv.Status) = if (status.pinDefault == false) "" else Piv.DEFAULT_PIN
