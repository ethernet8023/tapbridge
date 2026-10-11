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
    /** Started from Your cards: waiting for a key to read its PIV state. */
    object Scan : PivPrompt()
    /** A tapped key has a PIV applet with empty login slots. */
    data class Offer(val status: Piv.Status) : PivPrompt()
    /**
     * Started from Your cards and the tap is read: what the card holds now,
     * with "create the missing certificates" or "replace certificates".
     */
    data class Review(val status: Piv.Status) : PivPrompt()
    /** "Replace certificates" pressed: the explicit are-you-sure step. */
    data class ConfirmReplace(val status: Piv.Status) : PivPrompt()
    /**
     * The card held at setup is not set up the way the review showed
     * (another card, or its certificates changed). Nothing was written.
     */
    data class Changed(val status: Piv.Status) : PivPrompt()
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
        /** Slots to fill; null = the setup slots. */
        val slots: List<Piv.Slot>? = null,
        /** Overwrite slots that already hold certificates (confirmed). */
        val replace: Boolean = false,
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

/** One line per setup slot for the review: present, missing, or blocked. */
@Composable
private fun SlotStatus(status: Piv.Status) {
    Column(Modifier.padding(top = 8.dp)) {
        for (slot in Piv.SETUP_SLOTS) {
            val has = slot in status.certificates
            Text(
                (if (has) "\u2713 " else "\u25CB ") + "Slot ${slot.label} (${slotPurpose(slot)}): " +
                    if (has) "certificate" else "empty",
                style = MaterialTheme.typography.bodyMedium,
            )
        }
    }
}

private fun slotPurpose(slot: Piv.Slot) = when (slot) {
    Piv.Slot.AUTHENTICATION -> "login"
    Piv.Slot.KEY_MANAGEMENT -> "encryption"
}

@Composable
fun PivSetupDialogs(
    prompt: PivPrompt?,
    defaultName: String,
    onStart: () -> Unit,
    onSubmit: (form: PivPrompt.Form) -> Unit,
    onDismiss: () -> Unit,
    /** Review / Changed: create the missing certificates. */
    onCreate: (Piv.Status) -> Unit = {},
    /** Review: "Replace certificates", which asks to confirm first. */
    onAskReplace: (Piv.Status) -> Unit = {},
    /** ConfirmReplace: confirmed. */
    onReplace: (Piv.Status) -> Unit = {},
) {
    when (prompt) {
        null -> Unit
        is PivPrompt.Review -> {
            val status = prompt.status
            val blocked = status.contactlessBlocked == true
            val locked = pinLocked(status.pinRetries)
            AlertDialog(
                onDismissRequest = onDismiss,
                title = { Text(if (status.needsSetup) "Smart-card setup" else "Smart card is set up") },
                text = {
                    Column(Modifier.verticalScroll(rememberScrollState())) {
                        Text(
                            if (status.needsSetup) {
                                "tapbridge creates a key and a self-signed certificate in each empty slot. " +
                                    "Certificates already on the card stay as they are."
                            } else {
                                "Both login slots already have certificates. Nothing needs to change."
                            },
                        )
                        SlotStatus(status)
                        if (blocked) ContactlessWarning()
                        else if (locked) {
                            Text(
                                Piv.PinRetriesTooLow(status.pinRetries).message!!,
                                color = MaterialTheme.colorScheme.error,
                                modifier = Modifier.padding(top = 12.dp),
                            )
                        }
                        if (!blocked && !locked && status.certificates.isNotEmpty()) {
                            TextButton(onClick = { onAskReplace(status) }, modifier = Modifier.padding(top = 8.dp)) {
                                Text("Replace certificates\u2026", color = MaterialTheme.colorScheme.error)
                            }
                        }
                    }
                },
                confirmButton = {
                    if (status.needsSetup) {
                        TextButton(enabled = !blocked && !locked, onClick = { onCreate(status) }) {
                            Text(if (status.certificates.isEmpty()) "Set up" else "Fill empty slots")
                        }
                    } else {
                        TextButton(onClick = onDismiss) { Text("Done") }
                    }
                },
                dismissButton = if (status.needsSetup) {
                    { TextButton(onClick = onDismiss) { Text("Cancel") } }
                } else null,
            )
        }
        is PivPrompt.ConfirmReplace -> AlertDialog(
            onDismissRequest = onDismiss,
            title = { Text("Replace smart-card certificates?") },
            text = {
                Text(
                    "This deletes the keys in slot ${slotNames(prompt.status.certificates.keys.toList())} " +
                        "and makes new ones. Anything that used the old certificates stops working: " +
                        "a computer paired for login has to be paired again, and files encrypted to the old " +
                        "key can't be opened with this card. This can't be undone.",
                )
            },
            confirmButton = {
                TextButton(onClick = { onReplace(prompt.status) }) {
                    Text("Replace", color = MaterialTheme.colorScheme.error)
                }
            },
            dismissButton = { TextButton(onClick = onDismiss) { Text("Keep them") } },
        )
        is PivPrompt.Changed -> AlertDialog(
            onDismissRequest = onDismiss,
            title = { Text("This card is set up differently") },
            text = {
                Column {
                    Text(
                        "The card you held isn't set up the way it was when you started: it may be " +
                            "another card, or its certificates changed. Nothing was changed. Here's " +
                            "what it holds now.",
                    )
                    SlotStatus(prompt.status)
                }
            },
            confirmButton = { TextButton(onClick = onDismiss) { Text("OK") } },
        )
        is PivPrompt.Offer -> AlertDialog(
            onDismissRequest = onDismiss,
            title = { Text("Set up smart-card login?") },
            text = {
                Column {
                    Text(
                        "This key can work as a smart card, but it has no certificate in " +
                            "slot ${slotNames(prompt.status.missing)}. tapbridge can create a key and a " +
                            "self-signed certificate on it, so your computer can pair it for login.",
                    )
                    if (prompt.status.contactlessBlocked == true) {
                        ContactlessWarning()
                    } else if (pinLocked(prompt.status.pinRetries)) {
                        Text(
                            Piv.PinRetriesTooLow(prompt.status.pinRetries).message!!,
                            color = MaterialTheme.colorScheme.error,
                            modifier = Modifier.padding(top = 12.dp),
                        )
                    }
                }
            },
            confirmButton = {
                TextButton(
                    enabled = !pinLocked(prompt.status.pinRetries) && prompt.status.contactlessBlocked != true,
                    onClick = onStart,
                ) { Text("Set up") }
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
                        if (prompt.status.contactlessBlocked == true) ContactlessWarning()
                        Text(
                            if (prompt.replace) {
                                "Replaces the keys and certificates in slot " +
                                    "${slotNames(prompt.slots ?: Piv.SETUP_SLOTS)}."
                            } else {
                                "Creates keys and certificates in slot ${slotNames(prompt.status.missing)}. " +
                                    "Existing certificates stay as they are."
                            },
                            style = MaterialTheme.typography.bodyMedium,
                            color = if (prompt.replace) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface,
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
                                        prompt.status.pinDefault != false && pin == Piv.DEFAULT_PIN ->
                                            "Prefilled with the default PIN. $retries tries left."
                                        else -> "$retries tries left."
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
                        enabled = !locked && pinOk && keyOk && name.isNotBlank() &&
                            prompt.status.contactlessBlocked != true,
                        onClick = {
                            onSubmit(prompt.copy(pin = pin, managementKey = key, name = name.trim(),
                                pinError = null, keyError = null))
                        },
                    ) { Text("Continue") }
                },
                dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
            )
        }
        PivPrompt.Scan -> AlertDialog(
            onDismissRequest = onDismiss,
            title = { Text("Smart-card setup") },
            text = { KeyReaderOverlay(null) },
            confirmButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
        )
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
                        "You can now pair it for login on your computer, from Your cards or the computer's tapbridge preferences.",
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

/** Shown when the applet refuses login slots over NFC (strict contactless). */
@Composable
private fun ContactlessWarning() {
    Text(
        "This key's smart-card applet blocks login slots over NFC, so a phone can't set it up or use it. " +
            "If it runs PivApplet, install a build without \"S\" in its name (for example REePAxaD).",
        color = MaterialTheme.colorScheme.error,
        modifier = Modifier.padding(vertical = 12.dp),
    )
}
