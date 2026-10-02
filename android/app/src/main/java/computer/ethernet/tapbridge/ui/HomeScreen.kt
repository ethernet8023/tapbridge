package computer.ethernet.tapbridge.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Key
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.unit.dp
import computer.ethernet.tapbridge.IdentityStore
import computer.ethernet.tapbridge.MainActivity
import computer.ethernet.tapbridge.R
import computer.ethernet.tapbridge.RegisteredKeys
import computer.ethernet.tapbridge.TapBridgeService

/** Home: status card, big animated tap hero, quick actions, add-key dialog. */
@Composable
fun HomeScreen(
    ui: MainActivity.UiState,
    onStartPair: () -> Unit,
    onKeys: () -> Unit,
    onSettings: () -> Unit,
    readerMode: Boolean,
    onReaderModeDismiss: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val context = androidx.compose.ui.platform.LocalContext.current
    val paired = IdentityStore.pairedHost(context) != null
    val hostName = IdentityStore.hostName(context)
    val status = remember(ui.status, paired) {
        when (ui.status) {
            TapBridgeService.Status.CONNECTED -> HomeStatus.CONNECTED
            TapBridgeService.Status.CONNECTING -> HomeStatus.CONNECTING
            TapBridgeService.Status.ERROR -> HomeStatus.ERROR
            else -> if (paired) HomeStatus.CONNECTING else HomeStatus.UNPAIRED
        }
    }
    val phase = when {
        ui.cardPresent && ui.status == TapBridgeService.Status.CONNECTED -> TapPhase.SUCCESS
        ui.cardPresent -> TapPhase.DETECTED
        ui.status == TapBridgeService.Status.ERROR -> TapPhase.ERROR
        else -> TapPhase.WAITING
    }
    Column(
        modifier
            .fillMaxSize()
            .padding(20.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Icon(
                painterResource(R.drawable.ic_notification),
                contentDescription = null,
                tint = MaterialTheme.colorScheme.primary,
            )
            Spacer(Modifier.width(8.dp))
            Text("TapBridge", style = MaterialTheme.typography.titleLarge)
            Spacer(Modifier.weight(1f))
            IconButton(onClick = onKeys) {
                Icon(Icons.Filled.Key, contentDescription = "Your keys")
            }
            IconButton(onClick = onSettings) {
                Icon(Icons.Filled.Settings, contentDescription = "Settings")
            }
        }
        Spacer(Modifier.height(16.dp))
        StatusCard(status, hostName, ui.error)
        Spacer(Modifier.height(24.dp))
        TapHero(phase, Modifier.fillMaxWidth())
        Spacer(Modifier.height(24.dp))
        when (status) {
            HomeStatus.UNPAIRED -> Button(onClick = onStartPair, modifier = Modifier.fillMaxWidth()) {
                Text("Pair your PC")
            }
            else -> Text(
                "Hold your key against the back of this phone.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        if (!ui.notificationsGranted && paired) {
            Spacer(Modifier.height(8.dp))
            Text(
                "Push wake is off: the PC can't wake this phone. Allow notifications in Settings.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
    if (readerMode) {
        AlertDialog(
            onDismissRequest = onReaderModeDismiss,
            title = { Text("Register a key") },
            text = {
                Column {
                    KeyReaderOverlay(null)
                    if (ui.addKeyError != null) {
                        Text(
                            ui.addKeyError!!,
                            color = MaterialTheme.colorScheme.error,
                            modifier = Modifier.padding(top = 8.dp),
                        )
                    }
                }
            },
            confirmButton = {
                TextButton(onClick = onReaderModeDismiss) { Text("Done") }
            },
            dismissButton = {},
        )
    }
    ui.pendingRecord?.let { record ->
        var name by remember { mutableStateOf("") }
        AlertDialog(
            onDismissRequest = { ui.pendingRecord = null },
            title = { Text("Name this key") },
            text = {
                OutlinedTextField(value = name, onValueChange = { name = it }, singleLine = true)
            },
            confirmButton = {
                TextButton(
                    enabled = name.isNotBlank(),
                    onClick = {
                        RegisteredKeys.upsert(context, record.copy(label = name.trim()))
                        ui.pendingRecord = null
                    },
                ) { Text("Save") }
            },
            dismissButton = {
                TextButton(onClick = { ui.pendingRecord = null }) { Text("Cancel") }
            },
        )
    }
}
