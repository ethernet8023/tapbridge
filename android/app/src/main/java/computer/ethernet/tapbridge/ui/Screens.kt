package computer.ethernet.tapbridge.ui

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.animateContentSize
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
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
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.platform.LocalContext
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
            "On the PC, choose “Pair a phone” and hold the QR in the frame.",
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

/** Settings screen: phone name, notifications, forget PC. */
@Composable
fun SettingsScreen(
    phoneName: String,
    pushEnabled: Boolean,
    onPhoneName: (String) -> Unit,
    onRequestNotifications: () -> Unit,
    onForgetPc: () -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var nameDraft by remember(phoneName) { mutableStateOf(phoneName) }
    var confirmForget by remember { mutableStateOf(false) }
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
            onValueChange = { nameDraft = it },
            label = { Text("Phone name shown on your PC") },
            singleLine = true,
            supportingText = { Text("1–64 characters") },
            modifier = Modifier.fillMaxWidth(),
        )
        TextButton(
            onClick = { onPhoneName(nameDraft.trim()) },
            enabled = nameDraft.isNotBlank() && nameDraft.trim() != phoneName,
        ) { Text("Save name") }
        HorizontalDivider(Modifier.padding(vertical = 12.dp))
        ListItem(
            headlineContent = { Text("Notifications") },
            supportingContent = { Text("Needed for tap-request wake-ups") },
            trailingContent = {
                if (!pushEnabled) {
                    TextButton(onClick = onRequestNotifications) { Text("Allow") }
                } else {
                    Text("Allowed", color = MaterialTheme.colorScheme.primary)
                }
            },
        )
        HorizontalDivider(Modifier.padding(vertical = 12.dp))
        Button(
            onClick = { confirmForget = true },
            colors = ButtonDefaults.buttonColors(
                containerColor = MaterialTheme.colorScheme.errorContainer,
                contentColor = MaterialTheme.colorScheme.onErrorContainer,
            ),
            modifier = Modifier.fillMaxWidth(),
        ) { Text("Forget this PC") }
    }
    if (confirmForget) {
        AlertDialog(
            onDismissRequest = { confirmForget = false },
            title = { Text("Forget this PC?") },
            text = { Text("You'll need to scan the pairing QR again to reconnect.") },
            confirmButton = {
                TextButton(onClick = {
                    confirmForget = false
                    onForgetPc()
                }) { Text("Forget") }
            },
            dismissButton = {
                TextButton(onClick = { confirmForget = false }) { Text("Cancel") }
            },
        )
    }
}

/** Reader-mode hint shown while registering a key. */
@Composable
fun KeyReaderOverlay(keyName: String?, modifier: Modifier = Modifier) {
    val context = LocalContext.current
    val pulse = rememberInfiniteTransition(label = "reader").animateFloat(
        initialValue = 0.85f,
        targetValue = 1f,
        animationSpec = infiniteRepeatable(
            tween(700),
            RepeatMode.Reverse,
        ),
        label = "reader-pulse",
    ).value
    AnimatedVisibility(true, modifier = modifier) {
        Surface(
            shape = RoundedCornerShape(24.dp),
            color = MaterialTheme.colorScheme.primaryContainer,
            modifier = Modifier
                .animateContentSize()
                .graphicsLayer { alpha = pulse },
        ) {
            Column(Modifier.padding(24.dp), horizontalAlignment = Alignment.CenterHorizontally) {
                Text(
                    if (keyName == null) "Hold your key against the phone's back"
                    else "Hold again to confirm \u201C$keyName\u201D",
                    style = MaterialTheme.typography.titleMedium,
                )
                Text(
                    "Keep the key still until it vibrates.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onPrimaryContainer,
                )
            }
        }
    }
}
