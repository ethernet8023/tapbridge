package computer.ethernet.tapbridge.ui

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.ArrowBack
import androidx.compose.material.icons.filled.Badge
import androidx.compose.material.icons.filled.Computer
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import computer.ethernet.tapbridge.Invitation

/** Pair screen: animated QR scan viewfinder plus a paste fallback. */
@Composable
fun PairScreen(
    onPaired: (invitation: String) -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var paste by remember { mutableStateOf("") }
    var error by remember { mutableStateOf<String?>(null) }
    Column(
        modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(20.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            IconButton(onClick = onBack) {
                Icon(Icons.Filled.ArrowBack, contentDescription = "Back")
            }
            Text("Pair your PC", style = MaterialTheme.typography.headlineSmall)
        }
        Spacer(Modifier.height(16.dp))
        Text(
            "On your PC, choose Pair phone… in the tapbridge tray menu, then point this camera at the code.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(16.dp))
        Surface(
            shape = RoundedCornerShape(28.dp),
            tonalElevation = 2.dp,
            modifier = Modifier
                .fillMaxWidth()
                .height(320.dp),
        ) {
            Box {
                QrScanner(
                    onCode = { value ->
                        if (Invitation.parse(value) != null) onPaired(value)
                        else error = "That QR is not a TapBridge invitation"
                    },
                    Modifier.fillMaxSize(),
                )
                if (error != null) {
                    Text(
                        error!!,
                        color = MaterialTheme.colorScheme.error,
                        modifier = Modifier
                            .align(Alignment.BottomCenter)
                            .padding(12.dp),
                    )
                }
            }
        }
        Spacer(Modifier.height(20.dp))
        OutlinedTextField(
            value = paste,
            onValueChange = {
                paste = it
                error = null
            },
            label = { Text("Or paste the invitation") },
            placeholder = { Text("tapbridge:1:…") },
            singleLine = true,
            supportingText = { error?.let { Text(it, color = MaterialTheme.colorScheme.error) } },
            modifier = Modifier.fillMaxWidth(),
        )
        Button(
            onClick = {
                if (Invitation.parse(paste) != null) onPaired(paste.trim())
                else error = "Invalid invitation format"
            },
            enabled = paste.isNotBlank(),
            modifier = Modifier
                .fillMaxWidth()
                .padding(top = 12.dp),
        ) { Text("Pair") }
    }
}

/** Settings: device name, paired PCs (rename/forget each), permissions. */
@Composable
fun SettingsScreen(
    phoneName: String,
    pushEnabled: Boolean,
    dndGranted: Boolean,
    onRequestDnd: () -> Unit,
    hosts: List<computer.ethernet.tapbridge.PairedHosts.Record>,
    connectedHost: String?,
    onPhoneName: (String) -> Unit,
    onRequestNotifications: () -> Unit,
    onRenameHost: (hostId: String, name: String) -> Unit,
    onForgetHost: (hostId: String) -> Unit,
    onPairAnother: () -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
    /** Null when this APK carries no PC installers (local builds). */
    onInstallOnPc: (() -> Unit)? = null,
) {
    // The saved name is tracked here: the caller's value is not observable.
    var savedName by remember { mutableStateOf(phoneName) }
    var nameDraft by remember { mutableStateOf(phoneName) }
    var renaming by remember { mutableStateOf<computer.ethernet.tapbridge.PairedHosts.Record?>(null) }
    var renameText by remember { mutableStateOf("") }
    var forgetting by remember { mutableStateOf<computer.ethernet.tapbridge.PairedHosts.Record?>(null) }
    val edited = nameDraft.isNotBlank() && nameDraft.trim() != savedName
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
            Text("Settings", style = MaterialTheme.typography.headlineSmall)
        }
        Spacer(Modifier.height(16.dp))
        OutlinedTextField(
            value = nameDraft,
            onValueChange = { nameDraft = it.take(64) },
            label = { Text("Name this device") },
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )
        AnimatedVisibility(visible = edited) {
            Button(
                onClick = {
                    val name = nameDraft.trim()
                    onPhoneName(name)
                    savedName = name
                    nameDraft = name
                },
                modifier = Modifier.padding(top = 8.dp),
            ) { Text("Save name") }
        }
        HorizontalDivider(Modifier.padding(vertical = 16.dp))
        Text(
            "Paired PCs",
            style = MaterialTheme.typography.titleSmall,
            color = MaterialTheme.colorScheme.primary,
            modifier = Modifier.padding(bottom = 4.dp),
        )
        if (hosts.isEmpty()) {
            Text(
                "No PCs paired yet. Scan a PC's pairing code to add it.",
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(vertical = 12.dp),
            )
        }
        hosts.forEach { host ->
            ListItem(
                // "Online" = this phone has a live session with the PC right
                // now. The phone connects on demand, so an idle PC that is
                // running fine still reads "Offline".
                leadingContent = {
                    PresenceDot(if (host.hostId == connectedHost) HomeStatus.CONNECTED else HomeStatus.UNPAIRED)
                },
                headlineContent = { Text(host.name, fontWeight = FontWeight.Medium) },
                supportingContent = { Text(if (host.hostId == connectedHost) "Online" else "Offline") },
                trailingContent = {
                    Row {
                        IconButton(onClick = {
                            renaming = host
                            renameText = host.name
                        }) {
                            Icon(Icons.Filled.Edit, contentDescription = "Rename ${host.name}")
                        }
                        IconButton(onClick = { forgetting = host }) {
                            Icon(Icons.Filled.Delete, contentDescription = "Forget ${host.name}")
                        }
                    }
                },
            )
        }
        TextButton(
            onClick = onPairAnother,
            contentPadding = PaddingValues(horizontal = 8.dp, vertical = 4.dp),
        ) {
            Icon(Icons.Filled.Add, contentDescription = null, modifier = Modifier.size(18.dp))
            Spacer(Modifier.width(6.dp))
            Text(if (hosts.isEmpty()) "Pair a PC" else "Pair another PC")
        }
        if (onInstallOnPc != null) {
            ListItem(
                leadingContent = { Icon(Icons.Filled.Computer, contentDescription = null) },
                headlineContent = { Text("Install tapbridge on a PC") },
                supportingContent = { Text("Send the Windows app from this phone over a USB cable") },
                modifier = Modifier.clickable(onClick = onInstallOnPc),
            )
        }
        HorizontalDivider(Modifier.padding(vertical = 16.dp))
        Text(
            "Permissions",
            style = MaterialTheme.typography.titleSmall,
            color = MaterialTheme.colorScheme.primary,
            modifier = Modifier.padding(bottom = 4.dp),
        )
        ListItem(
            headlineContent = { Text("Notifications") },
            supportingContent = { Text("Lets your PCs wake this phone when they need your key") },
            trailingContent = {
                if (!pushEnabled) {
                    TextButton(onClick = onRequestNotifications) { Text("Allow") }
                } else {
                    Text("Allowed", color = MaterialTheme.colorScheme.primary)
                }
            },
        )
        ListItem(
            headlineContent = { Text("Do Not Disturb") },
            supportingContent = { Text("Let tap requests ring through Do Not Disturb") },
            trailingContent = {
                if (!dndGranted) {
                    TextButton(onClick = onRequestDnd) { Text("Allow") }
                } else {
                    Text("Allowed", color = MaterialTheme.colorScheme.primary)
                }
            },
        )
    }
    if (renaming != null) {
        AlertDialog(
            onDismissRequest = { renaming = null },
            title = { Text("Rename PC") },
            text = {
                OutlinedTextField(
                    value = renameText,
                    onValueChange = { renameText = it.take(64) },
                    singleLine = true,
                )
            },
            confirmButton = {
                TextButton(
                    enabled = renameText.isNotBlank(),
                    onClick = {
                        val host = renaming
                        if (host != null && renameText.isNotBlank()) {
                            onRenameHost(host.hostId, renameText.trim())
                        }
                        renaming = null
                    },
                ) { Text("Save") }
            },
            dismissButton = {
                TextButton(onClick = { renaming = null }) { Text("Cancel") }
            },
        )
    }
    if (forgetting != null) {
        AlertDialog(
            onDismissRequest = { forgetting = null },
            title = { Text("Forget ${forgetting?.name ?: "this PC"}?") },
            text = { Text("You'll need to scan that PC's pairing code again to reconnect. Your other PCs stay paired.") },
            confirmButton = {
                TextButton(onClick = {
                    forgetting?.let { onForgetHost(it.hostId) }
                    forgetting = null
                }) { Text("Forget") }
            },
            dismissButton = {
                TextButton(onClick = { forgetting = null }) { Text("Cancel") }
            },
        )
    }
}

/** Tap prompt while registering a key: the home hero's motion, smaller. */
@Composable
fun KeyReaderOverlay(keyName: String?, modifier: Modifier = Modifier) {
    Column(modifier, horizontalAlignment = Alignment.CenterHorizontally) {
        TapHero(TapPhase.WAITING, cardPresent = false, pcName = "your PC", modifier = Modifier.widthIn(max = 220.dp))
        Text(
            if (keyName == null) "Hold your key against the back of this phone"
            else "Hold again to confirm \u201C$keyName\u201D",
            style = MaterialTheme.typography.titleMedium,
            textAlign = androidx.compose.ui.text.style.TextAlign.Center,
        )
        Text(
            "Keep it still until the phone vibrates.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(top = 4.dp),
        )
    }
}
