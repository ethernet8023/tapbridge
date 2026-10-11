package computer.ethernet.tapbridge.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.ArrowBack
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.RadioButtonUnchecked
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import computer.ethernet.tapbridge.CardInfo
import computer.ethernet.tapbridge.Cards

/** A tapped card that isn't one of the user's yet, waiting for a name. */
data class PendingCard(val uid: ByteArray, val caps: Int, val suggested: String) {
    override fun equals(other: Any?) = other is PendingCard && other.uid.contentEquals(uid)
    override fun hashCode() = uid.contentHashCode()
}

/** What the phone can offer for the Mac login step right now. */
enum class MacLink {
    /** Connected to a Mac: the step can be started from here. */
    CONNECTED,
    /** Not connected to a Mac (or connected to another PC). */
    NONE,
}

/** One checklist step's state (same four as the desktop's `StepState`). */
private enum class Step { DONE, TODO, PROBLEM, LOCKED }

/** Your cards: each card's checklist, plus add / rename / forget. */
@Composable
fun CardsScreen(
    cards: List<CardInfo>,
    mac: MacLink,
    /** Show the Mac login step at all (this phone has met a Mac). */
    showMacLogin: Boolean,
    /** Hex UID of the card whose Mac login is running on the Mac, if any. */
    macLoginBusy: String?,
    onAddCard: () -> Unit,
    onRename: (CardInfo, String) -> Unit,
    onForget: (CardInfo) -> Unit,
    onWake: (CardInfo, Boolean) -> Unit,
    onSetUpCerts: (CardInfo) -> Unit,
    onMacLogin: (CardInfo) -> Unit,
    onBack: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var renaming by remember { mutableStateOf<CardInfo?>(null) }
    var forgetting by remember { mutableStateOf<CardInfo?>(null) }
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
            Text("Your cards", style = MaterialTheme.typography.headlineSmall)
        }
        Spacer(Modifier.height(8.dp))
        Text(
            "Cards you use with tapbridge. They stay in sync with your computers.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Spacer(Modifier.height(16.dp))
        cards.forEach { card ->
            CardItem(
                card = card,
                mac = mac,
                showMacLogin = showMacLogin,
                macLoginBusy = macLoginBusy == Cards.hex(card.uid),
                onRename = { renaming = card },
                onForget = { forgetting = card },
                onWake = { onWake(card, it) },
                onSetUpCerts = { onSetUpCerts(card) },
                onMacLogin = { onMacLogin(card) },
            )
            Spacer(Modifier.height(12.dp))
        }
        if (cards.isEmpty()) {
            Text(
                "No cards yet. Add one, or just tap it on this phone.",
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(vertical = 24.dp),
            )
        }
        Button(
            onClick = onAddCard,
            modifier = Modifier
                .fillMaxWidth()
                .padding(top = 4.dp),
        ) {
            Icon(Icons.Filled.Add, contentDescription = null)
            Spacer(Modifier.width(8.dp))
            Text("Add a card")
        }
    }
    renaming?.let { card ->
        NameDialog(
            title = "Rename card",
            initial = card.label,
            confirm = "Save",
            onSave = { onRename(card, it); renaming = null },
            onDismiss = { renaming = null },
        )
    }
    forgetting?.let { card ->
        AlertDialog(
            onDismissRequest = { forgetting = null },
            title = { Text("Forget \u201C${card.label}\u201D?") },
            text = {
                Text(
                    "It's removed from this phone and your computers. Nothing on the card changes, " +
                        "and you can add it again any time.",
                )
            },
            confirmButton = {
                TextButton(onClick = { onForget(card); forgetting = null }) { Text("Forget") }
            },
            dismissButton = { TextButton(onClick = { forgetting = null }) { Text("Cancel") } },
        )
    }
}

@Composable
private fun CardItem(
    card: CardInfo,
    mac: MacLink,
    showMacLogin: Boolean,
    macLoginBusy: Boolean,
    onRename: () -> Unit,
    onForget: () -> Unit,
    onWake: (Boolean) -> Unit,
    onSetUpCerts: () -> Unit,
    onMacLogin: () -> Unit,
) {
    val certs = when {
        card.has(CardInfo.Caps.BLOCKED) -> Step.PROBLEM
        card.has(CardInfo.Caps.CERTS) -> Step.DONE
        card.has(CardInfo.Caps.PIV) -> Step.TODO
        else -> Step.LOCKED
    }
    val login = when {
        card.macLogin -> Step.DONE
        certs == Step.DONE -> Step.TODO
        else -> Step.LOCKED
    }
    Surface(
        shape = RoundedCornerShape(20.dp),
        tonalElevation = 2.dp,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(Modifier.padding(start = 16.dp, end = 4.dp, top = 8.dp, bottom = 12.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    card.label,
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.SemiBold,
                    modifier = Modifier.weight(1f),
                )
                IconButton(onClick = onRename) {
                    Icon(Icons.Filled.Edit, contentDescription = "Rename ${card.label}")
                }
                IconButton(onClick = onForget) {
                    Icon(Icons.Filled.Delete, contentDescription = "Forget ${card.label}")
                }
            }
            Column(Modifier.padding(end = 12.dp)) {
                StepRow(
                    if (card.has(CardInfo.Caps.FIDO)) Step.DONE else Step.LOCKED,
                    "Passkeys",
                    if (card.has(CardInfo.Caps.FIDO)) null else "Not seen on this card",
                )
                StepRow(
                    certs,
                    "Smart-card certificates",
                    when (certs) {
                        Step.PROBLEM -> "The smart-card applet blocks its keys over NFC"
                        Step.LOCKED -> "No smart-card applet seen"
                        else -> null
                    },
                ) {
                    if (certs == Step.TODO) TextButton(onClick = onSetUpCerts) { Text("Set up") }
                }
                if (showMacLogin) StepRow(
                    login,
                    "Mac login",
                    when {
                        macLoginBusy -> "Finish on your Mac"
                        login == Step.TODO && mac == MacLink.NONE -> "Connect to your Mac to set this up"
                        login == Step.LOCKED -> "Needs the certificates"
                        else -> null
                    },
                ) {
                    if (login == Step.TODO && mac == MacLink.CONNECTED && !macLoginBusy) {
                        TextButton(onClick = onMacLogin) { Text("Set up") }
                    }
                }
                Row(
                    Modifier
                        .fillMaxWidth()
                        .padding(top = 6.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        "Open tapbridge when tapped",
                        style = MaterialTheme.typography.bodyMedium,
                        modifier = Modifier.weight(1f),
                    )
                    Switch(checked = card.wake, onCheckedChange = onWake)
                }
            }
        }
    }
}

@Composable
private fun StepRow(
    step: Step,
    title: String,
    detail: String?,
    action: @Composable () -> Unit = {},
) {
    val (icon: ImageVector, tint) = when (step) {
        Step.DONE -> Icons.Filled.CheckCircle to MaterialTheme.colorScheme.primary
        Step.PROBLEM -> Icons.Filled.Warning to MaterialTheme.colorScheme.error
        Step.TODO -> Icons.Filled.RadioButtonUnchecked to MaterialTheme.colorScheme.onSurface
        Step.LOCKED -> Icons.Filled.RadioButtonUnchecked to MaterialTheme.colorScheme.outline
    }
    Row(
        Modifier
            .fillMaxWidth()
            .heightIn(min = 40.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(icon, contentDescription = null, tint = tint, modifier = Modifier.size(20.dp))
        Spacer(Modifier.width(12.dp))
        Column(Modifier.weight(1f)) {
            Text(
                title,
                style = MaterialTheme.typography.bodyLarge,
                color = if (step == Step.LOCKED) MaterialTheme.colorScheme.outline else MaterialTheme.colorScheme.onSurface,
            )
            if (detail != null) {
                Text(
                    detail,
                    style = MaterialTheme.typography.bodySmall,
                    color = if (step == Step.PROBLEM) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
        action()
    }
}

/** A one-line name, capped to the wire's label size. */
@Composable
private fun NameDialog(
    title: String,
    initial: String,
    confirm: String,
    onSave: (String) -> Unit,
    onDismiss: () -> Unit,
    text: String? = null,
) {
    var name by remember(initial) { mutableStateOf(initial) }
    val clean = Cards.cleanLabel(name)
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(title) },
        text = {
            Column {
                if (text != null) {
                    Text(text, modifier = Modifier.padding(bottom = 12.dp))
                }
                OutlinedTextField(
                    value = name,
                    onValueChange = { value -> if (value.toByteArray(Charsets.UTF_8).size <= Cards.MAX_LABEL) name = value },
                    placeholder = { Text("e.g. Blue key") },
                    singleLine = true,
                )
            }
        },
        confirmButton = {
            TextButton(enabled = clean != null, onClick = { clean?.let(onSave) }) { Text(confirm) }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

/** Add-card flow over any screen: tap prompt, then name the card. */
@Composable
fun AddCardDialogs(
    reading: Boolean,
    readError: String?,
    pending: PendingCard?,
    onCancelRead: () -> Unit,
    onSave: (PendingCard, String) -> Unit,
    onDiscard: () -> Unit,
) {
    if (reading && pending == null) {
        AlertDialog(
            onDismissRequest = onCancelRead,
            title = { Text("Add a card") },
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
    pending?.let { card ->
        NameDialog(
            title = "New card",
            initial = card.suggested,
            confirm = "Add",
            text = listOfNotNull(
                "passkeys".takeIf { card.caps and CardInfo.Caps.FIDO != 0 },
                "smart card".takeIf { card.caps and CardInfo.Caps.PIV != 0 },
            ).takeIf { it.isNotEmpty() }?.let { "This card has ${it.joinToString(" and ")}. Give it a name." }
                ?: "Give this card a name.",
            onSave = { onSave(card, it) },
            onDismiss = onDiscard,
        )
    }
}
