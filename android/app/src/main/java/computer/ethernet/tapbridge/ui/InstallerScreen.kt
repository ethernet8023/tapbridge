package computer.ethernet.tapbridge.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ArrowBack
import androidx.compose.material.icons.filled.Usb
import androidx.compose.material.icons.filled.Wifi
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.currentStateAsState
import computer.ethernet.tapbridge.InstallerAddresses
import computer.ethernet.tapbridge.InstallerAssets
import computer.ethernet.tapbridge.InstallerHttp
import computer.ethernet.tapbridge.InstallerServer
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext

/**
 * Settings -> "Install tapbridge on a PC". Serves the bundled Windows
 * installers over USB tethering (or Wi-Fi) only while this screen is on
 * screen and the app is in the foreground.
 */
@Composable
fun InstallerScreen(installers: List<String>, onBack: () -> Unit, modifier: Modifier = Modifier) {
    val context = LocalContext.current
    val lifecycle by LocalLifecycleOwner.current.lifecycle.currentStateAsState()
    val visible = lifecycle.isAtLeast(Lifecycle.State.STARTED)
    var port by remember { mutableStateOf<Int?>(null) }
    var failed by remember { mutableStateOf(false) }
    var downloads by remember { mutableIntStateOf(0) }
    var urls by remember { mutableStateOf<List<InstallerAddresses.Url>>(emptyList()) }

    // Leaving the screen (or the app) stops the server and closes any open
    // connection; coming back starts it again.
    DisposableEffect(visible) {
        val server = if (visible) {
            // Counted here, not per server: the server restarts on return.
            InstallerServer(InstallerAssets.source(context)) {
                android.os.Handler(android.os.Looper.getMainLooper()).post { downloads += 1 }
            }
        } else null
        if (server != null) {
            try {
                port = server.start()
                failed = false
            } catch (_: Exception) {
                port = null
                failed = true
            }
        }
        onDispose {
            server?.stop()
            port = null
        }
    }

    // Tethering doesn't show up as a ConnectivityManager network, so poll
    // the interfaces while the server runs: cheap, and catches every change.
    LaunchedEffect(port) {
        val p = port ?: run { urls = emptyList(); return@LaunchedEffect }
        while (true) {
            urls = withContext(Dispatchers.IO) { InstallerAssets.urls(p) }
            delay(2000)
        }
    }

    Column(
        modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(20.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            IconButton(onClick = onBack) {
                Icon(Icons.Filled.ArrowBack, contentDescription = "Back")
            }
            Text("Install on a PC", style = MaterialTheme.typography.headlineSmall)
        }
        Spacer(Modifier.height(16.dp))
        Text(
            "Your phone can hand the Windows app to a PC, no internet needed.",
            style = MaterialTheme.typography.bodyLarge,
        )
        Spacer(Modifier.height(16.dp))
        Step(1, "Plug this phone into the PC with a USB cable.")
        Step(2, "Turn on USB tethering on this phone.")
        OutlinedButton(
            onClick = { InstallerAssets.openTetherSettings(context) },
            modifier = Modifier.padding(start = 32.dp, top = 4.dp, bottom = 8.dp),
        ) { Text("Open tethering settings") }
        Step(3, "On the PC, open a web browser and go to the address below. Then run the installer it downloads.")
        Spacer(Modifier.height(12.dp))

        when {
            failed -> Text(
                "Couldn't start sharing. Close this screen and try again.",
                color = MaterialTheme.colorScheme.error,
            )
            port == null -> Text("Starting\u2026", color = MaterialTheme.colorScheme.onSurfaceVariant)
            urls.isEmpty() -> Text(
                "Waiting for USB tethering\u2026 You can also connect the PC and this phone to the same Wi-Fi.",
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            else -> urls.forEachIndexed { i, url ->
                val usb = url.kind == InstallerAddresses.Kind.USB
                ListItem(
                    leadingContent = {
                        Icon(if (usb) Icons.Filled.Usb else Icons.Filled.Wifi, contentDescription = null)
                    },
                    headlineContent = {
                        SelectionContainer {
                            Text(
                                url.url,
                                fontFamily = FontFamily.Monospace,
                                fontWeight = if (i == 0) FontWeight.SemiBold else FontWeight.Normal,
                                style = if (i == 0) MaterialTheme.typography.titleLarge else MaterialTheme.typography.bodyLarge,
                            )
                        }
                    },
                    supportingContent = {
                        Text(if (usb) "USB tethering" else "Wi-Fi (the PC must be on the same network)")
                    },
                )
            }
        }

        HorizontalDivider(Modifier.padding(vertical = 16.dp))
        Text(
            "Installers on this phone",
            style = MaterialTheme.typography.titleSmall,
            color = MaterialTheme.colorScheme.primary,
            modifier = Modifier.padding(bottom = 4.dp),
        )
        installers.forEach { name ->
            val arch = InstallerHttp.archOf(name)
            ListItem(
                headlineContent = { Text(arch?.label ?: name) },
                supportingContent = { Text(arch?.hint ?: "") },
            )
        }
        Spacer(Modifier.height(8.dp))
        Text(
            when (downloads) {
                0 -> "No downloads yet."
                1 -> "Downloaded once."
                else -> "Downloaded $downloads times."
            },
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(8.dp))
        Text(
            "Sharing stops when you leave this screen.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun Step(n: Int, text: String) {
    Row(Modifier.padding(vertical = 4.dp)) {
        Text("$n.", fontWeight = FontWeight.SemiBold, modifier = Modifier.width(32.dp))
        Text(text)
    }
}
