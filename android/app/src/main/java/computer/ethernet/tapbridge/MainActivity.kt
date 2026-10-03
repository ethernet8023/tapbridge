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
import computer.ethernet.tapbridge.ui.SettingsScreen
import computer.ethernet.tapbridge.ui.theme.LocalReducedMotion
import computer.ethernet.tapbridge.ui.theme.TapBridgeTheme
import java.util.UUID

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
        var notificationsGranted by mutableStateOf(false)
        var dndGranted by mutableStateOf(false)
        var addKeyMode by mutableStateOf(false)
        var addKeyError by mutableStateOf<String?>(null)
        var pendingRecord by mutableStateOf<RegisteredKeys.KeyRecord?>(null)
    }

    private var state: UiState? = null

    /** Runtime notification permission launcher, shared with the Settings screen. */
    var requestPermissionLauncher: androidx.activity.result.ActivityResultLauncher<String>? = null
        private set

    private val serviceListener = object : TapBridgeService.Listener {
        override fun onStatusChanged(status: TapBridgeService.Status, error: String?) {
            runOnUiThread {
                state?.let { it.status = status; it.error = error }
            }
        }
        override fun onCardPresent(present: Boolean) {
            runOnUiThread { state?.let { it.cardPresent = present } }
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
            deliverForwardedTag()
        }
        override fun onServiceDisconnected(name: ComponentName?) {
            service = null
            bound = false
        }
    }

    private val readerCallback = NfcAdapter.ReaderCallback { tag ->
        if (state?.addKeyMode == true) {
            runOnUiThread { handleRegistrationTag(tag) }
        } else {
            service?.onTagDiscovered(tag)
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
        // DESIGN: opening the app while paired connects immediately.
        if (IdentityStore.pairedHost(this) != null) {
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
                    paired = IdentityStore.pairedHost(context) != null,
                    onPhoneName = { name ->
                        runCatching { IdentityStore.setPhoneName(context, name) }
                    },
                    onRequestNotifications = {
                        activity.requestPermissionLauncher?.launch(
                            android.Manifest.permission.POST_NOTIFICATIONS,
                        )
                    },
                    onForgetPc = {
                        activity.stopTapService()
                        IdentityStore.forgetHost(context)
                        screen = "home"
                    },
                    onBack = { screen = "home" },
                    Modifier.fillMaxSize(),
                )
                else -> HomeScreen(
                    ui = ui,
                    onStartPair = { screen = "pair" },
                    onKeys = { keys = RegisteredKeys.list(context); screen = "keys" },
                    onSettings = { screen = "settings" },
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
}
