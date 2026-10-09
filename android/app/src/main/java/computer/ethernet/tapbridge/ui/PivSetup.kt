package computer.ethernet.tapbridge.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
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
    data class Offer(val missing: List<Piv.Slot>) : PivPrompt()
    /** Asking for the PIN and the certificate name. */
    data class Form(val missing: List<Piv.Slot>, val error: String? = null) : PivPrompt()
    /** Waiting for the key to be held to the phone. */
    data class Waiting(val missing: List<Piv.Slot>) : PivPrompt()
    /** Generating keys and writing certificates; the key must stay put. */
    object Working : PivPrompt()
    data class Done(val slots: List<Piv.Slot>) : PivPrompt()
    data class Failed(val message: String) : PivPrompt()
}

private fun slotNames(slots: List<Piv.Slot>) = slots.joinToString(" and ") { it.label }

@Composable
fun PivSetupDialogs(
    prompt: PivPrompt?,
    defaultName: String,
    onStart: () -> Unit,
    onSubmit: (pin: String, name: String) -> Unit,
    onDismiss: () -> Unit,
) {
    when (prompt) {
        null -> Unit
        is PivPrompt.Offer -> AlertDialog(
            onDismissRequest = onDismiss,
            title = { Text("Set up smart-card login?") },
            text = {
                Text(
                    "This key can work as a smart card, but it has no certificate in " +
                        "slot ${slotNames(prompt.missing)}. tapbridge can create a key and a " +
                        "self-signed certificate on it, so your Mac can pair it for login.",
                )
            },
            confirmButton = { TextButton(onClick = onStart) { Text("Set up") } },
            dismissButton = { TextButton(onClick = onDismiss) { Text("Not now") } },
        )
        is PivPrompt.Form -> {
            var pin by remember { mutableStateOf("") }
            var name by remember { mutableStateOf(defaultName) }
            val pinOk = pin.length in 6..8
            AlertDialog(
                onDismissRequest = onDismiss,
                title = { Text("Smart-card setup") },
                text = {
                    Column {
                        Text(
                            "Creates new keys in slot ${slotNames(prompt.missing)}. " +
                                "Anything already in those slots is replaced.",
                            style = MaterialTheme.typography.bodyMedium,
                        )
                        OutlinedTextField(
                            value = pin,
                            onValueChange = { pin = it.take(8) },
                            label = { Text("PIV PIN") },
                            supportingText = { Text(prompt.error ?: "A new key's PIN is 123456") },
                            isError = prompt.error != null,
                            singleLine = true,
                            visualTransformation = PasswordVisualTransformation(),
                            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.NumberPassword),
                            modifier = Modifier.padding(top = 12.dp),
                        )
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
                        enabled = pinOk && name.isNotBlank(),
                        onClick = { onSubmit(pin, name.trim()) },
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
