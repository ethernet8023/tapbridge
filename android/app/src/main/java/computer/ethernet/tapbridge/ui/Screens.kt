package computer.ethernet.tapbridge.ui

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.ArrowBack
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import computer.ethernet.tapbridge.Invitation
import computer.ethernet.tapbridge.RegisteredKeys

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

/** Keys screen: registered security keys with add/rename/remove. */
@Composable
fun KeysScreen(
    keys: List<RegisteredKeys.KeyRecord>,
    onAddKey: () -> Unit,
    onRename: (RegisteredKeys.KeyRecord, String) -> Unit,
    onRemove: (RegisteredKeys.KeyRecord) -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var renaming by remember { mutableStateOf<RegisteredKeys.KeyRecord?>(null) }
    var renameText by remember { mutableStateOf("") }
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
            Text("Your keys", style = MaterialTheme.typography.headlineSmall)
        }
        Spacer(Modifier.height(8.dp))
        Text(
            "Register the keys you want this phone to accept when the PC wakes it.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(16.dp))
        keys.forEach { record ->
            ListItem(
                headlineContent = { Text(record.label, fontWeight = FontWeight.Medium) },
                supportingContent = {
                    Text(
                        when {
                            record.uid != null -> "NFC UID · " + RegisteredKeys.hex(record.uid)
                            record.aaguid != null -> "AAGUID · " + RegisteredKeys.hex(record.aaguid)
                            else -> "No identifiers stored"
                        }
                    )
                },
                trailingContent = {
                    Row {
                        IconButton(onClick = {
                            renaming = record
                            renameText = record.label
                        }) {
                            Icon(Icons.Filled.Edit, contentDescription = "Rename ${record.label}")
                        }
                        IconButton(onClick = { onRemove(record) }) {
                            Icon(Icons.Filled.Delete, contentDescription = "Remove ${record.label}")
                        }
                    }
                },
            )
        }
        if (keys.isEmpty()) {
            Text(
                "No keys registered yet.",
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(vertical = 24.dp),
            )
        }
        Button(
            onClick = onAddKey,
            modifier = Modifier
                .fillMaxWidth()
                .padding(top = 12.dp),
        ) {
            Icon(Icons.Filled.Add, contentDescription = null)
            Spacer(Modifier.width(8.dp))
            Text("Add a key")
        }
    }
    if (renaming != null) {
        AlertDialog(
            onDismissRequest = { renaming = null },
            title = { Text("Rename key") },
            text = {
                OutlinedTextField(
                    value = renameText,
                    onValueChange = { renameText = it },
                    singleLine = true,
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    val record = renaming
                    if (record != null && renameText.isNotBlank()) {
                        onRename(record, renameText.trim())
                    }
                    renaming = null
                }) { Text("Save") }
            },
            dismissButton = {
                TextButton(onClick = { renaming = null }) { Text("Cancel") }
            },
        )
    }
}

/** Settings: device name, unpair, and permissions at the bottom. */
@Composable
fun SettingsScreen(
    phoneName: String,
    pushEnabled: Boolean,
    paired: Boolean,
    onPhoneName: (String) -> Unit,
    onRequestNotifications: () -> Unit,
    onForgetPc: () -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    // The saved name is tracked here: the caller's value is not observable.
    var savedName by remember { mutableStateOf(phoneName) }
    var nameDraft by remember { mutableStateOf(phoneName) }
    var confirmForget by remember { mutableStateOf(false) }
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
        if (paired) {
            HorizontalDivider(Modifier.padding(vertical = 16.dp))
            OutlinedButton(
                onClick = { confirmForget = true },
                colors = ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error),
                modifier = Modifier.fillMaxWidth(),
            ) { Text("Unpair your connected PC") }
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
            supportingContent = { Text("Lets your PC wake this phone when it needs your key") },
            trailingContent = {
                if (!pushEnabled) {
                    TextButton(onClick = onRequestNotifications) { Text("Allow") }
                } else {
                    Text("Allowed", color = MaterialTheme.colorScheme.primary)
                }
            },
        )
    }
    if (confirmForget) {
        AlertDialog(
            onDismissRequest = { confirmForget = false },
            title = { Text("Unpair this PC?") },
            text = { Text("You'll need to scan the PC's pairing code again to reconnect.") },
            confirmButton = {
                TextButton(onClick = {
                    confirmForget = false
                    onForgetPc()
                }) { Text("Unpair") }
            },
            dismissButton = {
                TextButton(onClick = { confirmForget = false }) { Text("Cancel") }
            },
        )
    }
}

/** Add-key flow, shown above whatever screen is open: tap prompt, then name. */
@Composable
fun AddKeyDialogs(
    reading: Boolean,
    readError: String?,
    pending: RegisteredKeys.KeyRecord?,
    onCancelRead: () -> Unit,
    onSave: (RegisteredKeys.KeyRecord) -> Unit,
    onDiscard: () -> Unit,
) {
    if (reading && pending == null) {
        AlertDialog(
            onDismissRequest = onCancelRead,
            title = { Text("Add a key") },
            text = {
                Column {
                    KeyReaderOverlay(null)
                    if (readError != null) {
                        Text(
                            readError,
                            color = MaterialTheme.colorScheme.error,
                            modifier = Modifier.padding(top = 12.dp),
                        )
                    }
                }
            },
            confirmButton = { TextButton(onClick = onCancelRead) { Text("Cancel") } },
        )
    }
    pending?.let { record ->
        var name by remember(record) { mutableStateOf("") }
        AlertDialog(
            onDismissRequest = onDiscard,
            title = { Text("Name this key") },
            text = {
                OutlinedTextField(
                    value = name,
                    onValueChange = { name = it.take(40) },
                    placeholder = { Text("e.g. Blue key") },
                    singleLine = true,
                )
            },
            confirmButton = {
                TextButton(
                    enabled = name.isNotBlank(),
                    onClick = { onSave(record.copy(label = name.trim())) },
                ) { Text("Save") }
            },
            dismissButton = { TextButton(onClick = onDiscard) { Text("Cancel") } },
        )
    }
}

/** Tap prompt while registering a key: the home hero's motion, smaller. */
@Composable
fun KeyReaderOverlay(keyName: String?, modifier: Modifier = Modifier) {
    Column(modifier, horizontalAlignment = Alignment.CenterHorizontally) {
        TapHero(TapPhase.WAITING, Modifier.widthIn(max = 220.dp))
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
