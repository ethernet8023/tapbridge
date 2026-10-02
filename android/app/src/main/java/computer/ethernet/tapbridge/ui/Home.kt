package computer.ethernet.tapbridge.ui

import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.core.*
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.scaleIn
import androidx.compose.animation.scaleOut
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutVertically
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.rotate
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import computer.ethernet.tapbridge.ui.theme.LocalReducedMotion
import kotlin.math.min

/**
 * Connection state shown as a big animated status card plus the tap hero:
 * pulsing NFC rings around a card illustration (Compose Canvas), with a
 * card-detected morph, success check and error shake. Static under
 * reduced motion (ANIMATOR_DURATION_SCALE == 0).
 */
@Composable
fun StatusCard(status: HomeStatus, hostName: String?, error: String?, modifier: Modifier = Modifier) {
    val description = when (status) {
        HomeStatus.UNPAIRED -> "Not paired yet"
        HomeStatus.CONNECTING -> "Connecting to the PC"
        HomeStatus.CONNECTED -> "Connected to the PC"
        HomeStatus.ERROR -> "Connection failed"
    }
    ElevatedCard(
        modifier = modifier
            .fillMaxWidth()
            .semantics { contentDescription = description },
        shape = RoundedCornerShape(24.dp),
    ) {
        AnimatedContent(
            targetState = status to (hostName ?: ""),
            transitionSpec = {
                (fadeIn(animationSpec = tween(220)) + slideInVertically(tween(220)) { it / 4 })
                    .togetherWith(fadeOut(tween(180)) + slideOutVertically(tween(180)) { -it / 4 })
            },
            label = "status",
        ) { (state, name) ->
            Column(Modifier.padding(20.dp)) {
                Text(
                    text = when (state) {
                        HomeStatus.UNPAIRED -> "Pair your PC"
                        HomeStatus.CONNECTING -> "Connecting…"
                        HomeStatus.CONNECTED -> "Connected"
                        HomeStatus.ERROR -> "Couldn't connect"
                    },
                    style = MaterialTheme.typography.headlineSmall,
                    fontWeight = FontWeight.SemiBold,
                )
                val sub = when (state) {
                    HomeStatus.UNPAIRED -> "Scan the QR code shown by the TapBridge app on your PC."
                    HomeStatus.CONNECTING -> if (name.isEmpty()) "Looking for your PC…" else "Reaching $name…"
                    HomeStatus.CONNECTED -> if (name.isEmpty()) "Your PC is listening" else "Your PC \u201C$name\u201D is listening"
                    HomeStatus.ERROR -> error ?: "Try again, and check the PC app."
                }
                Text(
                    text = sub,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

enum class HomeStatus { UNPAIRED, CONNECTING, CONNECTED, ERROR }

/** Phases of the tap hero animation. */
enum class TapPhase { WAITING, DETECTED, SUCCESS, ERROR }

@Composable
fun TapHero(phase: TapPhase, modifier: Modifier = Modifier) {
    val reducedMotion = LocalReducedMotion.current
    val haptics = LocalHapticFeedback.current
    LaunchedEffect(phase) {
        when (phase) {
            TapPhase.DETECTED, TapPhase.SUCCESS ->
                haptics.performHapticFeedback(HapticFeedbackType.LongPress)
            else -> Unit
        }
    }

    val transition = updateTransition(targetState = phase, label = "tap-hero")
    val progress by transition.animateFloat(
        transitionSpec = { spring(dampingRatio = Spring.DampingRatioMediumBouncy, stiffness = Spring.StiffnessLow) },
        label = "progress",
    ) { phase ->
        when (phase) {
            TapPhase.DETECTED -> 0.6f
            TapPhase.SUCCESS -> 1f
            else -> 0f
        }
    }
    val shake by transition.animateFloat(
        transitionSpec = { tween(400) },
        label = "shake",
    ) { phase -> if (phase == TapPhase.ERROR) 1f else 0f }

    val pulse = if (reducedMotion) 0.5f else rememberInfiniteTransition(label = "pulse")
        .animateFloat(
            initialValue = 0f,
            targetValue = 1f,
            animationSpec = infiniteRepeatable(tween(2200, easing = LinearEasing)),
            label = "pulse",
        ).value

    val ringColor = MaterialTheme.colorScheme.primary
    val cardColor = MaterialTheme.colorScheme.primaryContainer
    val onCard = MaterialTheme.colorScheme.onPrimaryContainer
    val successColor = MaterialTheme.colorScheme.secondary
    val errorColor = MaterialTheme.colorScheme.error

    Box(
        modifier
            .size(280.dp)
            .semantics {
                contentDescription = when (phase) {
                    TapPhase.WAITING -> "Waiting for your key. Tap it on the back of this phone."
                    TapPhase.DETECTED -> "Card detected"
                    TapPhase.SUCCESS -> "Done"
                    TapPhase.ERROR -> "Card error. Try again."
                }
            },
        contentAlignment = Alignment.Center,
    ) {
        Canvas(Modifier.fillMaxSize()) {
            val center = Offset(size.width / 2, size.height / 2)
            val baseRadius = min(size.width, size.height) * 0.28f
            // Pulsing NFC rings: three arcs expanding outward, fading.
            if (phase != TapPhase.SUCCESS) {
                for (i in 0 until 3) {
                    val t = ((pulse + i / 3f) % 1f)
                    val radius = baseRadius + t * baseRadius * 0.9f
                    drawArc(
                        color = ringColor.copy(alpha = (1f - t) * 0.5f),
                        startAngle = -90f,
                        sweepAngle = 360f,
                        useCenter = false,
                        topLeft = Offset(center.x - radius, center.y - radius),
                        size = androidx.compose.ui.geometry.Size(radius * 2, radius * 2),
                        style = Stroke(width = 5.dp.toPx(), cap = StrokeCap.Round),
                    )
                }
            }
            val scaleX = 1f + shake * 0.02f * kotlin.math.sin(shake * 40f * 6.28f)
            drawContext.transform.scale(scaleX, 1f, center)
            // Card illustration, tilted like the brand glyph.
            drawContext.transform.rotate(-8f, center)
            val w = baseRadius * 1.15f
            val h = w * 1.5f
            val lift = progress * baseRadius * 0.12f
            drawRoundRect(
                color = cardColor,
                topLeft = Offset(center.x - w / 2, center.y - h / 2 - lift),
                size = androidx.compose.ui.geometry.Size(w, h),
                cornerRadius = androidx.compose.ui.geometry.CornerRadius(w * 0.16f),
            )
            // Card "chip": morphs into a check when success.
            drawRoundRect(
                color = onCard.copy(alpha = 1f - progress * 0.4f),
                topLeft = Offset(center.x - w * 0.26f, center.y - h * 0.06f - lift),
                size = androidx.compose.ui.geometry.Size(w * 0.52f, h * 0.18f),
                cornerRadius = androidx.compose.ui.geometry.CornerRadius(w * 0.08f),
            )
            if (progress > 0.8f) {
                // Success check
                drawLine(
                    color = successColor,
                    start = Offset(center.x - w * 0.15f, center.y - lift),
                    end = Offset(center.x - w * 0.02f, center.y + h * 0.12f - lift),
                    strokeWidth = 7.dp.toPx(),
                    cap = StrokeCap.Round,
                )
                drawLine(
                    color = successColor,
                    start = Offset(center.x - w * 0.02f, center.y + h * 0.12f - lift),
                    end = Offset(center.x + w * 0.22f, center.y - h * 0.12f - lift),
                    strokeWidth = 7.dp.toPx(),
                    cap = StrokeCap.Round,
                )
            }
            if (phase == TapPhase.ERROR) {
                drawArc(
                    color = errorColor,
                    startAngle = -110f,
                    sweepAngle = 320f,
                    useCenter = false,
                    topLeft = Offset(center.x - baseRadius, center.y - baseRadius),
                    size = androidx.compose.ui.geometry.Size(baseRadius * 2, baseRadius * 2),
                    style = Stroke(width = 4.dp.toPx(), cap = StrokeCap.Round),
                )
            }
        }
    }
}
