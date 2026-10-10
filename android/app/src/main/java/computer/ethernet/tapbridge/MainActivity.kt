package computer.ethernet.tapbridge

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.content.pm.PackageManager
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
import computer.ethernet.tapbridge.ui.KeysScreen
import computer.ethernet.tapbridge.ui.AddKeyDialogs
import computer.ethernet.tapbridge.ui.PairScreen
import computer.ethernet.tapbridge.ui.PivPrompt
import computer.ethernet.tapbridge.ui.PivSetupDialogs
import computer.ethernet.tapbridge.ui.SettingsScreen
import computer.ethernet.tapbridge.ui.theme.LocalReducedMotion
import computer.ethernet.tapbridge.ui.theme.TapBridgeTheme
import java.util.UUID
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
        var pendingRecord by mutableStateOf<RegisteredKeys.KeyRecord?>(null)
        /** Phone-side PIV setup offer/flow; null when none is showing. */
        var pivPrompt by mutableStateOf<PivPrompt?>(null)
    }

    /** Keys whose PIV offer was turned down this run (stable UID hex). */
    private val pivDeclined = HashSet<String>()
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
                    it.connectedHost =
                        if (status == TapBridgeService.Status.CONNECTED) service?.targetHost else null
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
            state?.connectedHost = service?.takeIf { it.status == TapBridgeService.Status.CONNECTED }?.targetHost
            deliverForwardedTag()
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
            ui?.addKeyMode == true -> runOnUiThread { handleRegistrationTag(tag) }
            // No PC is using the key: look for an unconfigured PIV applet
            // before handing the tag on. Never during a PC request, where
            // extra SELECTs would disturb the PC's session with the card.
            ui != null && ui.pivPrompt == null && !ui.tapRequested &&
                ui.status != TapBridgeService.Status.CONNECTED -> pivWorker.execute {
                    probePiv(tag)
                    service?.onTagDiscovered(tag)
                }
            else -> service?.onTagDiscovered(tag)
        }
    }

    /** Offers PIV setup when the tapped key has PIV with empty login slots. */
    private fun probePiv(tag: Tag) {
        val uid = tag.id?.takeIf { RegisteredKeys.uidStable(it) }?.let { RegisteredKeys.hex(it) }
        if (uid != null && uid in pivDeclined) return
        val iso = android.nfc.tech.IsoDep.get(tag) ?: return
        val status = try {
            iso.connect()
            iso.timeout = 5000
            Piv.probe { iso.transceive(it) }
        } catch (_: Exception) {
            null
        } finally {
            runCatching { iso.close() }
        }
        if (status?.needsSetup == true) {
            runOnUiThread {
                state?.let { if (it.pivPrompt == null) it.pivPrompt = PivPrompt.Offer(status) }
                pivOfferedUid = uid
            }
        }
    }

    private var pivOfferedUid: String? = null

    /** Your keys -> "Set up smart-card login": any key, any time. */
    internal fun startPivSetup() {
        state?.pivPrompt = PivPrompt.Scan
    }

    /**
     * Reads the tapped key's PIV state for a manual setup. Both login slots
     * are (re)created, even ones that already hold a certificate: this is
     * how a user replaces certificates after the first setup.
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
        val next = when {
            iso == null -> PivPrompt.Failed("That isn't a smart card.")
            status == null -> PivPrompt.Failed("This key has no smart-card (PIV) applet, or it moved away too soon.")
            else -> PivPrompt.Form(status.copy(missing = Piv.SETUP_SLOTS), slots = Piv.SETUP_SLOTS)
        }
        runOnUiThread { if (state?.pivPrompt == PivPrompt.Scan) state?.pivPrompt = next }
    }

    internal fun declinePiv() {
        pivOfferedUid?.let { pivDeclined.add(it) }
        endPiv()
    }

    internal fun endPiv() {
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
            val result = Piv.setUp(transport, pin, form.name ?: "", slots = form.slots, managementKey = key)
            if (result.slots.isEmpty()) PivPrompt.Failed("This key already has its certificates.")
            else PivPrompt.Done(result.slots)
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
        runOnUiThread { state?.pivPrompt = next }
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
        handleForwardedTag(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handleForwardedTag(intent)
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
    /** Add-key flow: read UID (stable only) + AAGUID from a tapped card. */
    private fun handleRegistrationTag(tag: Tag) {
        val iso = android.nfc.tech.IsoDep.get(tag)
        var uid: ByteArray? = null
        var aaguid: ByteArray? = null
        if (iso != null) {
            try {
                iso.connect()
                iso.timeout = 5000
                uid = tag.id?.takeIf { RegisteredKeys.uidStable(it) }
                if (Ctap2.selectFido(iso)) {
                    Ctap2.getInfoResponse(iso)?.let { info ->
                        aaguid = Ctap2.aaguidFromInfo(info)
                    }
                }
                // Offered once the key is named (the dialog waits for that).
                runCatching { Piv.probe { iso.transceive(it) } }.getOrNull()
                    ?.takeIf { it.needsSetup }
                    ?.let { state?.pivPrompt = PivPrompt.Offer(it) }
            } catch (e: Exception) {
                state?.let { it.addKeyError = "Could not read the key: ${e.message ?: "card error"}" }
            } finally {
                runCatching { iso.close() }
            }
        }
        if (uid == null && aaguid == null) {
            state?.let {
                it.addKeyError = "This card has no stable identifier; it can't be registered"
                it.addKeyMode = false
            }
            return
        }
        state?.let {
            it.pendingRecord = RegisteredKeys.KeyRecord(
                id = UUID.randomUUID().toString(),
                label = "",
                uid = uid,
                aaguid = aaguid,
            )
            it.addKeyMode = false
        }
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
        // DESIGN: opening the app while paired connects immediately (to the
        // last-used host).
        if (IdentityStore.pairedHosts(this).isNotEmpty()) {
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
    var keys by remember { mutableStateOf(RegisteredKeys.list(context)) }
    var hosts by remember { mutableStateOf(IdentityStore.pairedHosts(context)) }

    // System back (and predictive back) returns to home from any sub-screen.
    BackHandler(enabled = screen != "home") { screen = "home" }

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
                    onPaired = {
                        PairingState.setPendingInvitation(it)
                        activity.startTapService()
                        // Wake alerts need notifications; ask while the user
                        // is setting the app up.
                        if (!ui.notificationsGranted && Build.VERSION.SDK_INT >= 33) {
                            activity.requestPermissionLauncher?.launch(
                                android.Manifest.permission.POST_NOTIFICATIONS,
                            )
                        }
                        screen = "home"
                    },
                    onBack = { screen = "home" },
                    Modifier.fillMaxSize(),
                )
                "keys" -> KeysScreen(
                    keys = keys,
                    onAddKey = { ui.addKeyError = null; ui.addKeyMode = true },
                    onRename = { record, label ->
                        RegisteredKeys.upsert(context, record.copy(label = label))
                        keys = RegisteredKeys.list(context)
                    },
                    onRemove = { record ->
                        RegisteredKeys.remove(context, record.id)
                        keys = RegisteredKeys.list(context)
                    },
                    onSetUpPiv = { activity.startPivSetup() },
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
                )
                else -> HomeScreen(
                    ui = ui,
                    onStartPair = { screen = "pair" },
                    onKeys = { keys = RegisteredKeys.list(context); screen = "keys" },
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
    AddKeyDialogs(
        reading = ui.addKeyMode,
        readError = ui.addKeyError,
        pending = ui.pendingRecord,
        onCancelRead = { ui.addKeyMode = false; ui.addKeyError = null },
        onSave = { record ->
            RegisteredKeys.upsert(context, record)
            ui.pendingRecord = null
            ui.addKeyMode = false
            keys = RegisteredKeys.list(context)
        },
        onDiscard = { ui.pendingRecord = null; ui.addKeyMode = false },
    )
    // After the add-key dialogs, so a newly registered key is named first.
    if (ui.pendingRecord == null && !ui.addKeyMode) {
        PivSetupDialogs(
            prompt = ui.pivPrompt,
            defaultName = IdentityStore.phoneName(context).let { "$it PIV" },
            onStart = {
                (ui.pivPrompt as? PivPrompt.Offer)?.let { ui.pivPrompt = PivPrompt.Form(it.status) }
            },
            onSubmit = { form -> activity.submitPiv(form) },
            onDismiss = {
                if (ui.pivPrompt is PivPrompt.Offer) activity.declinePiv() else activity.endPiv()
            },
        )
    }
}
