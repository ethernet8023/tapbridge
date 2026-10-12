package computer.ethernet.tapbridge.ui

import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Key
import androidx.compose.material.icons.filled.QrCodeScanner
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import computer.ethernet.tapbridge.IdentityStore
import computer.ethernet.tapbridge.MainActivity
import computer.ethernet.tapbridge.R
import computer.ethernet.tapbridge.TapBridgeService

/** Home: status card, the hero art for the current state, one clear action. */
@Composable
fun HomeScreen(
    ui: MainActivity.UiState,
    onStartPair: () -> Unit,
    onKeys: () -> Unit,
    onSettings: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val context = androidx.compose.ui.platform.LocalContext.current
    val hosts = IdentityStore.pairedHosts(context)
    val paired = hosts.isNotEmpty()
    // The home card names the last-used PC; wakes name their own.
    val hostName = hosts.firstOrNull { it.hostId == IdentityStore.lastUsedHost(context) }?.name
    val status = when {
        !paired -> HomeStatus.UNPAIRED
        ui.status == TapBridgeService.Status.CONNECTED -> HomeStatus.CONNECTED
        ui.status == TapBridgeService.Status.ERROR -> HomeStatus.ERROR
        else -> HomeStatus.CONNECTING
    }
    // The live session's PC names the sign-in; else the last-used one.
    val pcName = ui.connectedHost?.let { id -> hosts.firstOrNull { it.hostId == id }?.name }
        ?: hostName ?: "your PC"
    // The animation only plays while a PC is asking for the card or has just
    // reported a result; a merely connected phone rests (see tapPhase).
    val phase = tapPhase(ui.status, ui.cardPresent, ui.tapRequested, ui.authState)
    Column(
        modifier
            .fillMaxSize()
            .padding(horizontal = 20.dp, vertical = 12.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Icon(
                painterResource(R.drawable.ic_notification),
                contentDescription = null,
                tint = MaterialTheme.colorScheme.primary,
            )
            Spacer(Modifier.width(10.dp))
            Text("tapbridge", style = MaterialTheme.typography.titleLarge, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.weight(1f))
            if (paired) {
                IconButton(onClick = onKeys) {
                    Icon(Icons.Filled.Key, contentDescription = "Your cards")
                }
            }
            IconButton(onClick = onSettings) {
                Icon(Icons.Filled.Settings, contentDescription = "Settings")
            }
        }
        Spacer(Modifier.height(12.dp))
        StatusCard(status, if (ui.viaUsb) pcName else hostName, ui.error, viaUsb = ui.viaUsb)
        Box(
            Modifier
                .weight(1f)
                .fillMaxWidth(),
            contentAlignment = Alignment.Center,
        ) {
            AnimatedContent(
                targetState = paired,
                transitionSpec = { fadeIn(tween(320)).togetherWith(fadeOut(tween(200))) },
                label = "hero",
            ) { isPaired ->
                if (isPaired) TapHero(phase, ui.cardPresent, pcName, Modifier.widthIn(max = 380.dp))
                else PairHero(Modifier.widthIn(max = 400.dp))
            }
        }
        if (!paired) {
            Button(
                onClick = onStartPair,
                contentPadding = PaddingValues(vertical = 16.dp),
                modifier = Modifier.fillMaxWidth(),
            ) {
                Icon(Icons.Filled.QrCodeScanner, contentDescription = null)
                Spacer(Modifier.width(10.dp))
                Text("Scan pairing code", style = MaterialTheme.typography.titleMedium)
            }
        }
        if (paired) {
            Spacer(Modifier.height(12.dp))
            AnimatedContent(
                targetState = phase to ui.cardPresent,
                transitionSpec = { fadeIn(tween(220)).togetherWith(fadeOut(tween(150))) },
                label = "hint",
            ) { (current, present) ->
                Text(
                    tapHint(current, present, pcName),
                    style = MaterialTheme.typography.titleMedium,
                    textAlign = TextAlign.Center,
                    modifier = Modifier.fillMaxWidth(),
                )
            }
        }
        if (!ui.notificationsGranted && paired) {
            Spacer(Modifier.height(8.dp))
            Text(
                "Your PC can't wake this phone until notifications are allowed (Settings → Permissions).",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
            )
        }
        Spacer(Modifier.height(8.dp))
    }
}

/** The line under the tap hero for [phase]; [pcName] is the paired PC. */
internal fun tapHint(phase: TapPhase, cardPresent: Boolean, pcName: String): String = when (phase) {
    TapPhase.IDLE -> "Your PC will ask for your key when it needs it."
    TapPhase.WAITING -> "Hold your key flat against the back of this phone."
    TapPhase.DETECTED -> "Key found. Keep holding it."
    TapPhase.ACTIVE -> "Signing in on $pcName. Keep holding your key."
    TapPhase.SUCCESS -> "Signed in on $pcName"
    TapPhase.REMOVED -> "Card removed before sign-in finished. Tap your key again."
    TapPhase.PIN -> "Enter your key's PIN on $pcName" + if (cardPresent) ". Keep holding your key." else ""
    TapPhase.FAILED -> "Sign-in didn't finish. Try again on $pcName."
    TapPhase.ERROR -> "Something went wrong. Lift the key and try again."
}
