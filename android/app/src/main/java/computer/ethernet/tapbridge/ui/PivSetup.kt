package computer.ethernet.tapbridge.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import computer.ethernet.tapbridge.Piv

/** Steps of the phone-side PIV setup (see Piv.kt). */
sealed class PivPrompt {
    /** A tapped key has a PIV applet with empty login slots. */
    data class Offer(val status: Piv.Status) : PivPrompt()
    /**
     * Asking for the PIN, management key and certificate name. [pinError]
     * / [keyError] come back from a failed attempt; the typed values are
     * kept in [pin] / [managementKey] so a retry doesn't retype them.
     */
    data class Form(
        val status: Piv.Status,
        val pinError: String? = null,
        val keyError: String? = null,
        val pin: String = "",
        val managementKey: String = Piv.DEFAULT_MANAGEMENT_KEY_HEX,
        val name: String? = null,
    ) : PivPrompt()
    /** Waiting for the key to be held to the phone. */
    data class Waiting(val form: Form) : PivPrompt()
    /** Generating keys and writing certificates; the key must stay put. */
    object Working : PivPrompt()
    data class Done(val slots: List<Piv.Slot>) : PivPrompt()
    data class Failed(val message: String) : PivPrompt()
}

private fun slotNames(slots: List<Piv.Slot>) = slots.joinToString(" and ") { it.label }

/** True when the app must not send a PIN to this key at all. */
fun pinLocked(retries: Int?) = retries == null || retries <= Piv.MIN_PIN_RETRIES

@Composable
fun PivSetupDialogs(
    prompt: PivPrompt?,
    defaultName: String,
    onStart: () -> Unit,
    onSubmit: (form: PivPrompt.Form) -> Unit,
    onDismiss: () -> Unit,
) {
    when (prompt) {
        null -> Unit
        is PivPrompt.Offer -> AlertDialog(
            onDismissRequest = onDismiss,
            title = { Text("Set up smart-card login?") },
            text = {
                Column {
                    Text(
                        "This key can work as a smart card, but it has no certificate in " +
                            "slot ${slotNames(prompt.status.missing)}. tapbridge can create a key and a " +
                            "self-signed certificate on it, so your Mac can pair it for login.",
                    )
                    if (pinLocked(prompt.status.pinRetries)) {
                        Text(
                            Piv.PinRetriesTooLow(prompt.status.pinRetries).message!!,
                            color = MaterialTheme.colorScheme.error,
                            modifier = Modifier.padding(top = 12.dp),
                        )
                    }
                }
            },
            confirmButton = {
                TextButton(enabled = !pinLocked(prompt.status.pinRetries), onClick = onStart) { Text("Set up") }
            },
            dismissButton = { TextButton(onClick = onDismiss) { Text("Not now") } },
        )
        is PivPrompt.Form -> {
            // Keyed on the prompt so a failed attempt's values come back in.
            var pin by remember(prompt) { mutableStateOf(prompt.pin) }
            var key by remember(prompt) { mutableStateOf(prompt.managementKey) }
            var name by remember(prompt) { mutableStateOf(prompt.name ?: defaultName) }
            val retries = prompt.status.pinRetries
            val locked = pinLocked(retries)
            val pinOk = pin.length in 6..8
            val keyOk = Piv.parseManagementKey(key) != null
            AlertDialog(
                onDismissRequest = onDismiss,
                title = { Text("Smart-card setup") },
                text = {
                    Column(Modifier.verticalScroll(rememberScrollState())) {
                        Text(
                            "Creates new keys in slot ${slotNames(prompt.status.missing)}. " +
                                "Anything already in those slots is replaced.",
                            style = MaterialTheme.typography.bodyMedium,
                        )
                        OutlinedTextField(
                            value = pin,
                            onValueChange = { pin = it.take(8) },
                            label = { Text("PIV PIN") },
                            supportingText = {
                                Text(
                                    when {
                                        locked -> Piv.PinRetriesTooLow(retries).message!!
                                        prompt.pinError != null -> prompt.pinError
                                        else -> "$retries tries left. The default is 123456."
                                    },
                                )
                            },
                            isError = locked || prompt.pinError != null,
                            enabled = !locked,
                            singleLine = true,
                            visualTransformation = PasswordVisualTransformation(),
                            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.NumberPassword),
                            modifier = Modifier.padding(top = 12.dp),
                        )
                        OutlinedTextField(
                            value = key,
                            onValueChange = { key = it.take(80) },
                            label = { Text("Management key (hex)") },
                            supportingText = {
                                Text(
                                    prompt.keyError ?: when {
                                        key.isNotBlank() && !keyOk -> "16, 24 or 32 bytes of hex"
                                        prompt.status.defaultManagementKey == false ->
                                            "The key says its management key was changed; enter yours"
                                        else -> "Prefilled with the default; safe to retry"
                                    },
                                )
                            },
                            isError = prompt.keyError != null || (key.isNotBlank() && !keyOk),
                            singleLine = true,
                        )
                        TextButton(onClick = { key = Piv.DEFAULT_MANAGEMENT_KEY_HEX }) {
                            Text("Use the default key")
                        }
                        OutlinedTextField(
                            value = name,
                            onValueChange = { name = it.take(64) },
                            label = { Text("Name on the certificate") },
                            singleLine = true,
                        )
                    }
                },
                confirmButton = {
                    TextButton(
                        enabled = !locked && pinOk && keyOk && name.isNotBlank(),
                        onClick = {
                            onSubmit(prompt.copy(pin = pin, managementKey = key, name = name.trim(),
                                pinError = null, keyError = null))
                        },
                    ) { Text("Continue") }
                },
                dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
            )
        }
        is PivPrompt.Waiting -> AlertDialog(
            onDismissRequest = onDismiss,
            title = { Text("Smart-card setup") },
            text = { KeyReaderOverlay(null) },
            confirmButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
        )
        PivPrompt.Working -> AlertDialog(
            onDismissRequest = {},
            title = { Text("Setting up…") },
            text = {
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    CircularProgressIndicator(Modifier.padding(16.dp))
                    Text("Keep the key still. This takes a few seconds.")
                }
            },
            confirmButton = {},
        )
        is PivPrompt.Done -> AlertDialog(
            onDismissRequest = onDismiss,
            title = { Text("Smart card ready") },
            text = {
                Text(
                    "Slot ${slotNames(prompt.slots)} now ${if (prompt.slots.size == 1) "has a certificate" else "have certificates"}. " +
                        "Next time you use this key with your Mac, macOS offers to pair it for login.",
                )
            },
            confirmButton = { TextButton(onClick = onDismiss) { Text("Done") } },
        )
        is PivPrompt.Failed -> AlertDialog(
            onDismissRequest = onDismiss,
            title = { Text("Setup didn't finish") },
            text = { Text(prompt.message) },
            confirmButton = { TextButton(onClick = onDismiss) { Text("OK") } },
        )
    }
}
