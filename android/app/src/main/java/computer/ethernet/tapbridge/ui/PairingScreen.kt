package computer.ethernet.tapbridge.ui

import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.ErrorOutline
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import computer.ethernet.tapbridge.PairingFlow

/**
 * After a scan: stays up until the PC accepts the phone ("Paired with …"),
 * or the quiet retries give up (the error, Try again / Cancel). [pairing]
 * is null until the service has picked the code up, which reads as pairing.
 */
@Composable
fun PairingScreen(
    pairing: PairingFlow.View?,
    onRetry: () -> Unit,
    onClose: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val phase = pairing?.phase ?: PairingFlow.Phase.PAIRING
    val haptics = LocalHapticFeedback.current
    LaunchedEffect(phase) {
        when (phase) {
            PairingFlow.Phase.PAIRED -> haptics.performHapticFeedback(HapticFeedbackType.Confirm)
            PairingFlow.Phase.FAILED -> haptics.performHapticFeedback(HapticFeedbackType.Reject)
            PairingFlow.Phase.PAIRING -> {}
        }
    }
    Column(
        modifier.padding(horizontal = 24.dp, vertical = 20.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Spacer(Modifier.weight(1f))
        AnimatedContent(
            targetState = phase,
            transitionSpec = { fadeIn(tween(260)).togetherWith(fadeOut(tween(160))) },
            label = "pairing",
        ) { current ->
            Column(
                Modifier.fillMaxWidth().semantics { liveRegion = LiveRegionMode.Polite },
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                Box(Modifier.size(96.dp), contentAlignment = Alignment.Center) {
                    when (current) {
                        PairingFlow.Phase.PAIRING ->
                            CircularProgressIndicator(Modifier.size(72.dp), strokeWidth = 6.dp)
                        PairingFlow.Phase.PAIRED -> Icon(
                            Icons.Filled.CheckCircle, contentDescription = null,
                            tint = Color(0xFF2FBF71), modifier = Modifier.size(96.dp),
                        )
                        PairingFlow.Phase.FAILED -> Icon(
                            Icons.Filled.ErrorOutline, contentDescription = null,
                            tint = MaterialTheme.colorScheme.error, modifier = Modifier.size(96.dp),
                        )
                    }
                }
                Spacer(Modifier.height(28.dp))
                Text(
                    when (current) {
                        PairingFlow.Phase.PAIRING -> "Pairing with your PC…"
                        PairingFlow.Phase.PAIRED -> "Paired with ${pairing?.hostName ?: "your PC"}"
                        PairingFlow.Phase.FAILED -> "Couldn't pair"
                    },
                    style = MaterialTheme.typography.headlineSmall,
                    fontWeight = FontWeight.SemiBold,
                    textAlign = TextAlign.Center,
                )
                Spacer(Modifier.height(12.dp))
                Text(
                    when (current) {
                        PairingFlow.Phase.PAIRING ->
                            "Keep the pairing code open on your PC until this finishes."
                        PairingFlow.Phase.PAIRED ->
                            "${pairing?.hostName ?: "Your PC"} can now ask this phone for your cards."
                        PairingFlow.Phase.FAILED ->
                            "Check that tapbridge is running on the PC and its pairing code is still showing, then try again."
                    },
                    style = MaterialTheme.typography.bodyLarge,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center,
                )
                // While retrying: say why the last attempt failed. On
                // failure: the final reason.
                val detail = pairing?.error?.takeIf { current != PairingFlow.Phase.PAIRED }
                if (detail != null) {
                    Spacer(Modifier.height(12.dp))
                    Text(
                        if (current == PairingFlow.Phase.PAIRING) "Still trying: $detail" else detail,
                        style = MaterialTheme.typography.bodySmall,
                        color = if (current == PairingFlow.Phase.FAILED) MaterialTheme.colorScheme.error
                        else MaterialTheme.colorScheme.onSurfaceVariant,
                        textAlign = TextAlign.Center,
                    )
                }
            }
        }
        Spacer(Modifier.weight(1f))
        when (phase) {
            PairingFlow.Phase.PAIRING -> OutlinedButton(onClick = onClose, Modifier.fillMaxWidth()) {
                Text("Cancel")
            }
            PairingFlow.Phase.PAIRED -> Button(
                onClick = onClose,
                contentPadding = PaddingValues(vertical = 16.dp),
                modifier = Modifier.fillMaxWidth(),
            ) { Text("Done", style = MaterialTheme.typography.titleMedium) }
            PairingFlow.Phase.FAILED -> {
                Button(
                    onClick = onRetry,
                    contentPadding = PaddingValues(vertical = 16.dp),
                    modifier = Modifier.fillMaxWidth(),
                ) { Text("Try again", style = MaterialTheme.typography.titleMedium) }
                Spacer(Modifier.height(8.dp))
                OutlinedButton(onClick = onClose, Modifier.fillMaxWidth()) { Text("Cancel") }
            }
        }
    }
}
