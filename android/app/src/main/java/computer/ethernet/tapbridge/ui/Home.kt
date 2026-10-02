package computer.ethernet.tapbridge.ui

import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.Spring
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.keyframes
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.spring
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInVertically
import androidx.compose.animation.slideOutVertically
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.ElevatedCard
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.scale
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.rotate
import androidx.compose.ui.graphics.drawscope.scale
import androidx.compose.ui.graphics.drawscope.translate
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.util.lerp
import computer.ethernet.tapbridge.ui.theme.LocalReducedMotion
import kotlinx.coroutines.launch
import kotlin.math.PI
import kotlin.math.sin

/** Brand colors from the app icon; the hero art always carries the brand,
 *  whatever dynamic color the rest of the UI picks up. */
private val BrandLight = Color(0xFF7C5CFF)
private val BrandDeep = Color(0xFF2B1A80)
private val ChipLight = Color(0xFFF3DC9B)
private val ChipDeep = Color(0xFFC9A253)

enum class HomeStatus { UNPAIRED, CONNECTING, CONNECTED, ERROR }

/** Status card with an animated presence dot and cross-faded copy. */
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
        shape = RoundedCornerShape(28.dp),
    ) {
        AnimatedContent(
            targetState = status to (hostName ?: ""),
            transitionSpec = {
                (fadeIn(tween(260)) + slideInVertically(tween(260)) { it / 3 })
                    .togetherWith(fadeOut(tween(160)) + slideOutVertically(tween(160)) { -it / 3 })
            },
            label = "status",
        ) { (state, name) ->
            Row(
                Modifier.padding(horizontal = 20.dp, vertical = 18.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(14.dp),
            ) {
                PresenceDot(state)
                Column {
                    Text(
                        text = when (state) {
                            HomeStatus.UNPAIRED -> "Not paired"
                            HomeStatus.CONNECTING -> "Connecting…"
                            HomeStatus.CONNECTED -> if (name.isEmpty()) "Connected" else name
                            HomeStatus.ERROR -> "Couldn't connect"
                        },
                        style = MaterialTheme.typography.titleMedium,
                        fontWeight = FontWeight.SemiBold,
                    )
                    Text(
                        text = when (state) {
                            HomeStatus.UNPAIRED -> "Scan the code your PC shows to get started."
                            HomeStatus.CONNECTING -> if (name.isEmpty()) "Reaching your PC" else "Reaching $name"
                            HomeStatus.CONNECTED -> "Ready for your key"
                            HomeStatus.ERROR -> error ?: "Check that tapbridge is running on the PC."
                        },
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
    }
}

@Composable
private fun PresenceDot(status: HomeStatus) {
    val reducedMotion = LocalReducedMotion.current
    val color = when (status) {
        HomeStatus.CONNECTED -> Color(0xFF2FBF71)
        HomeStatus.CONNECTING -> MaterialTheme.colorScheme.primary
        HomeStatus.ERROR -> MaterialTheme.colorScheme.error
        HomeStatus.UNPAIRED -> MaterialTheme.colorScheme.outline
    }
    val breathing = status == HomeStatus.CONNECTING || status == HomeStatus.CONNECTED
    val pulse = if (reducedMotion || !breathing) 0f else rememberInfiniteTransition(label = "dot")
        .animateFloat(
            0f, 1f,
            infiniteRepeatable(tween(if (status == HomeStatus.CONNECTING) 900 else 2400, easing = LinearEasing)),
            label = "dot-pulse",
        ).value
    Box(Modifier.size(22.dp), contentAlignment = Alignment.Center) {
        Box(
            Modifier
                .size(22.dp)
                .scale(0.5f + pulse * 0.5f)
                .background(color.copy(alpha = (1f - pulse) * 0.35f), CircleShape),
        )
        Box(Modifier.size(10.dp).background(color, CircleShape))
    }
}

/** Phases of the tap hero. Only states the phone can actually observe:
 *  the phone never learns whether the PC's sign-in succeeded. */
enum class TapPhase {
    /** Paired, not connected: art at rest. */
    IDLE,
    /** Connected, no card: the key drifts toward the phone, inviting a tap. */
    WAITING,
    /** Card on the phone but the session is not up yet. */
    DETECTED,
    /** Card on the phone and the PC is talking to it. */
    ACTIVE,
    ERROR,
}

/**
 * The tap hero: a phone seen from above with NFC waves from its antenna
 * spot, and a brand security card that drifts in, docks with a spring, shows
 * orbiting activity while the PC talks to it, and shakes on errors. Static
 * under reduced motion.
 */
@Composable
fun TapHero(phase: TapPhase, modifier: Modifier = Modifier) {
    val reducedMotion = LocalReducedMotion.current
    val haptics = LocalHapticFeedback.current
    val scheme = MaterialTheme.colorScheme

    val infinite = rememberInfiniteTransition(label = "hero")
    val clock by infinite.animateFloat(
        0f, 1f, infiniteRepeatable(tween(2600, easing = LinearEasing)), label = "clock",
    )
    val drift by infinite.animateFloat(
        0f, 1f,
        infiniteRepeatable(
            keyframes {
                durationMillis = 4200
                0f at 0 using FastOutSlowInEasing
                0f at 900 using FastOutSlowInEasing
                0.82f at 2100 using LinearEasing
                0.82f at 2900 using FastOutSlowInEasing
                0f at 4200
            },
        ),
        label = "drift",
    )
    val dock = remember { Animatable(0f) }
    val shake = remember { Animatable(1f) }
    LaunchedEffect(phase) {
        when (phase) {
            TapPhase.DETECTED, TapPhase.ACTIVE -> {
                if (dock.value < 0.5f) haptics.performHapticFeedback(HapticFeedbackType.LongPress)
                dock.animateTo(1f, spring(dampingRatio = 0.55f, stiffness = Spring.StiffnessMediumLow))
            }
            TapPhase.ERROR -> {
                haptics.performHapticFeedback(HapticFeedbackType.LongPress)
                launch { dock.animateTo(0f, spring(stiffness = Spring.StiffnessLow)) }
                shake.snapTo(0f)
                shake.animateTo(1f, tween(650, easing = LinearEasing))
            }
            else -> dock.animateTo(0f, spring(dampingRatio = 0.8f, stiffness = Spring.StiffnessLow))
        }
    }

    val waveColor = if (phase == TapPhase.ERROR) scheme.error else scheme.primary
    val waveStrength = when (phase) {
        TapPhase.IDLE -> 0.25f
        TapPhase.WAITING -> 0.4f + 0.6f * drift
        TapPhase.DETECTED -> 0.55f
        TapPhase.ACTIVE -> 1f
        TapPhase.ERROR -> 0.7f
    }

    Canvas(
        modifier
            .fillMaxWidth()
            .aspectRatio(1f)
            .semantics {
                contentDescription = when (phase) {
                    TapPhase.IDLE -> "Waiting for your PC"
                    TapPhase.WAITING -> "Hold your key against the back of this phone"
                    TapPhase.DETECTED -> "Key detected"
                    TapPhase.ACTIVE -> "Your PC is talking to the key. Keep holding it."
                    TapPhase.ERROR -> "Key error. Try again."
                }
            },
    ) {
        val s = size.minDimension
        val phoneW = s * 0.40f
        val phoneH = s * 0.76f
        val phoneTopLeft = Offset(center.x - phoneW / 2, center.y - phoneH / 2 + s * 0.08f)
        val antenna = Offset(center.x, phoneTopLeft.y + phoneH * 0.40f)

        // Approach: 0 = hovering away, 1 = docked on the antenna.
        val approach = when {
            reducedMotion -> if (phase == TapPhase.DETECTED || phase == TapPhase.ACTIVE) 1f else 0.45f
            else -> lerp(if (phase == TapPhase.WAITING) drift else 0.15f, 1f, dock.value)
        }

        // 1. Waves from the antenna spot (behind everything else).
        if (!reducedMotion) {
            for (i in 0 until 3) {
                val t = (clock * (if (phase == TapPhase.ACTIVE) 1.6f else 1f) + i / 3f) % 1f
                drawCircle(
                    color = waveColor.copy(alpha = (1f - t) * (1f - t) * 0.55f * waveStrength),
                    radius = s * 0.10f + t * s * 0.40f,
                    center = antenna,
                    style = Stroke(width = s * 0.010f),
                )
            }
        }

        // 2. The phone.
        drawSoftShadow(phoneTopLeft + Offset(0f, s * 0.02f), Size(phoneW, phoneH), phoneW * 0.17f, 0.10f)
        drawRoundRect(
            brush = Brush.verticalGradient(
                listOf(scheme.surfaceContainerHighest, scheme.surfaceContainerHigh),
                startY = phoneTopLeft.y,
                endY = phoneTopLeft.y + phoneH,
            ),
            topLeft = phoneTopLeft,
            size = Size(phoneW, phoneH),
            cornerRadius = CornerRadius(phoneW * 0.17f),
        )
        drawRoundRect(
            color = scheme.outlineVariant,
            topLeft = phoneTopLeft,
            size = Size(phoneW, phoneH),
            cornerRadius = CornerRadius(phoneW * 0.17f),
            style = Stroke(width = s * 0.006f),
        )
        // Pixel-style camera bar.
        drawRoundRect(
            color = scheme.onSurface.copy(alpha = 0.16f),
            topLeft = phoneTopLeft + Offset(phoneW * 0.10f, phoneH * 0.07f),
            size = Size(phoneW * 0.80f, phoneH * 0.085f),
            cornerRadius = CornerRadius(phoneH * 0.045f),
        )
        // Antenna glow: brighter as the key gets close.
        val glowRadius = s * 0.17f
        drawCircle(
            brush = Brush.radialGradient(
                listOf(waveColor.copy(alpha = 0.10f + 0.30f * approach * waveStrength), Color.Transparent),
                center = antenna,
                radius = glowRadius,
            ),
            radius = glowRadius,
            center = antenna,
        )

        // 3. The security card, drifting in and docking.
        val cardW = s * 0.46f
        val cardH = cardW * 0.63f
        val far = antenna + Offset(s * 0.13f, -s * 0.33f)
        val cardCenter = Offset(lerp(far.x, antenna.x, approach), lerp(far.y, antenna.y, approach))
        val bob = if (reducedMotion || approach > 0.95f) 0f else sin(clock * 2 * PI.toFloat()) * s * 0.008f
        val shakeX = if (phase == TapPhase.ERROR && !reducedMotion) {
            sin(shake.value * PI.toFloat() * 7f) * (1f - shake.value) * s * 0.045f
        } else 0f
        val lift = 1f - approach // 1 = high above the phone
        val tilt = lerp(-6f, -15f, lift)
        val cardScale = lerp(1f, 1.10f, lift)
        val at = cardCenter + Offset(shakeX, bob)

        // Shadow on the phone: further and fainter while the card is lifted.
        drawSoftShadow(
            at - Offset(cardW / 2, cardH / 2) + Offset(s * 0.02f, s * 0.03f + lift * s * 0.06f),
            Size(cardW, cardH),
            cardW * 0.08f,
            0.16f * (1f - lift * 0.6f),
        )
        translate(at.x, at.y) {
            rotate(tilt, pivot = Offset.Zero) {
                scale(cardScale, cardScale, pivot = Offset.Zero) {
                    drawKeyCard(cardW, cardH, error = phase == TapPhase.ERROR, errorColor = scheme.error)
                }
            }
        }

        // 4. Activity: two arcs orbit the docked card while the PC talks to it.
        if (phase == TapPhase.ACTIVE || phase == TapPhase.DETECTED) {
            val radius = s * 0.33f
            val spin = if (reducedMotion) 0f else clock * 360f * 1.5f
            val alpha = if (phase == TapPhase.ACTIVE) 0.95f else 0.35f
            for (k in 0 until 2) {
                drawArc(
                    color = waveColor.copy(alpha = alpha),
                    startAngle = spin + k * 180f,
                    sweepAngle = 52f,
                    useCenter = false,
                    topLeft = antenna - Offset(radius, radius),
                    size = Size(radius * 2, radius * 2),
                    style = Stroke(width = s * 0.016f, cap = StrokeCap.Round),
                )
            }
        }
    }
}

/** A soft drop shadow faked with stacked, widening translucent rects. */
private fun DrawScope.drawSoftShadow(topLeft: Offset, size: Size, corner: Float, alpha: Float) {
    for (i in 1..4) {
        val grow = i * size.minDimension * 0.025f
        drawRoundRect(
            color = Color.Black.copy(alpha = alpha / (i * 1.6f)),
            topLeft = topLeft - Offset(grow, grow) * 0.5f,
            size = Size(size.width + grow, size.height + grow),
            cornerRadius = CornerRadius(corner + grow / 2),
        )
    }
}

/** The brand FIDO card, centered on the origin: gradient body, contact chip,
 *  the icon's wave glyph and two embossed lines. */
private fun DrawScope.drawKeyCard(w: Float, h: Float, error: Boolean, errorColor: Color) {
    val topLeft = Offset(-w / 2, -h / 2)
    val corner = CornerRadius(w * 0.075f)
    drawRoundRect(
        brush = Brush.linearGradient(listOf(BrandLight, BrandDeep), start = topLeft, end = topLeft + Offset(w, h)),
        topLeft = topLeft,
        size = Size(w, h),
        cornerRadius = corner,
    )
    // Sheen across the top edge.
    drawRoundRect(
        brush = Brush.verticalGradient(
            listOf(Color.White.copy(alpha = 0.22f), Color.Transparent),
            startY = topLeft.y,
            endY = topLeft.y + h * 0.5f,
        ),
        topLeft = topLeft,
        size = Size(w, h * 0.5f),
        cornerRadius = corner,
    )
    // Contact chip.
    val chipSize = Size(w * 0.18f, h * 0.24f)
    val chipAt = topLeft + Offset(w * 0.11f, h * 0.30f)
    drawRoundRect(
        brush = Brush.linearGradient(listOf(ChipLight, ChipDeep), start = chipAt, end = chipAt + Offset(chipSize.width, chipSize.height)),
        topLeft = chipAt,
        size = chipSize,
        cornerRadius = CornerRadius(chipSize.width * 0.18f),
    )
    drawLine(
        color = ChipDeep.copy(alpha = 0.7f),
        start = chipAt + Offset(0f, chipSize.height / 2),
        end = chipAt + Offset(chipSize.width, chipSize.height / 2),
        strokeWidth = w * 0.008f,
    )
    // Brand waves (the icon glyph), right side.
    val wavesAt = topLeft + Offset(w * 0.70f, h * 0.44f)
    for (k in 0 until 3) {
        val r = w * (0.06f + k * 0.06f)
        drawArc(
            color = Color.White.copy(alpha = 0.95f - k * 0.25f),
            startAngle = -50f,
            sweepAngle = 100f,
            useCenter = false,
            topLeft = wavesAt - Offset(r, r),
            size = Size(r * 2, r * 2),
            style = Stroke(width = w * 0.022f, cap = StrokeCap.Round),
        )
    }
    // Embossed lines.
    for ((k, length) in listOf(0.46f, 0.28f).withIndex()) {
        drawLine(
            color = Color.White.copy(alpha = 0.35f),
            start = topLeft + Offset(w * 0.11f, h * (0.74f + k * 0.11f)),
            end = topLeft + Offset(w * (0.11f + length), h * (0.74f + k * 0.11f)),
            strokeWidth = h * 0.045f,
            cap = StrokeCap.Round,
        )
    }
    if (error) {
        drawRoundRect(color = errorColor.copy(alpha = 0.35f), topLeft = topLeft, size = Size(w, h), cornerRadius = corner)
    }
}

/**
 * Pairing art: a monitor showing a QR code and a phone held up to it, with
 * a camera beam and viewfinder brackets that lock on in a loop.
 */
@Composable
fun PairHero(modifier: Modifier = Modifier) {
    val reducedMotion = LocalReducedMotion.current
    val scheme = MaterialTheme.colorScheme
    val infinite = rememberInfiniteTransition(label = "pair")
    val clock by infinite.animateFloat(
        0f, 1f, infiniteRepeatable(tween(3200, easing = LinearEasing)), label = "pair-clock",
    )
    // Brackets close in on the code, hold, release.
    val lock by infinite.animateFloat(
        0f, 1f,
        infiniteRepeatable(
            keyframes {
                durationMillis = 3200
                0f at 0 using FastOutSlowInEasing
                1f at 1100 using LinearEasing
                1f at 2300 using FastOutSlowInEasing
                0f at 3200
            },
        ),
        label = "pair-lock",
    )
    val t = if (reducedMotion) 0.25f else clock
    val l = if (reducedMotion) 1f else lock

    Canvas(
        modifier
            .fillMaxWidth()
            .aspectRatio(1.1f)
            .semantics { contentDescription = "Point your phone at the pairing code on your PC" },
    ) {
        val s = size.minDimension
        // Monitor.
        val monW = s * 0.86f
        val monH = monW * 0.60f
        val monTopLeft = Offset(center.x - monW / 2 - s * 0.06f, s * 0.04f)
        drawSoftShadow(monTopLeft + Offset(0f, s * 0.015f), Size(monW, monH), s * 0.05f, 0.08f)
        drawRoundRect(
            color = scheme.surfaceContainerHighest,
            topLeft = monTopLeft,
            size = Size(monW, monH),
            cornerRadius = CornerRadius(s * 0.05f),
        )
        val screenInset = s * 0.025f
        val screenTopLeft = monTopLeft + Offset(screenInset, screenInset)
        val screenSize = Size(monW - 2 * screenInset, monH - 2 * screenInset)
        drawRoundRect(
            brush = Brush.linearGradient(
                listOf(Color(0xFF1C1530), Color(0xFF0F0B1A)),
                start = screenTopLeft,
                end = screenTopLeft + Offset(screenSize.width, screenSize.height),
            ),
            topLeft = screenTopLeft,
            size = screenSize,
            cornerRadius = CornerRadius(s * 0.03f),
        )
        // Stand.
        drawRoundRect(
            color = scheme.surfaceContainerHigh,
            topLeft = Offset(monTopLeft.x + monW * 0.42f, monTopLeft.y + monH),
            size = Size(monW * 0.16f, s * 0.07f),
        )
        drawRoundRect(
            color = scheme.surfaceContainerHighest,
            topLeft = Offset(monTopLeft.x + monW * 0.30f, monTopLeft.y + monH + s * 0.065f),
            size = Size(monW * 0.40f, s * 0.025f),
            cornerRadius = CornerRadius(s * 0.0125f),
        )
        // The QR on a white tile.
        val qrSide = screenSize.height * 0.72f
        val qrTopLeft = Offset(
            screenTopLeft.x + (screenSize.width - qrSide) / 2,
            screenTopLeft.y + (screenSize.height - qrSide) / 2,
        )
        drawRoundRect(
            color = Color.White,
            topLeft = qrTopLeft - Offset(qrSide * 0.06f, qrSide * 0.06f),
            size = Size(qrSide * 1.12f, qrSide * 1.12f),
            cornerRadius = CornerRadius(qrSide * 0.06f),
        )
        drawQrPattern(qrTopLeft, qrSide)
        // Breathing accent ring on the monitor, like the desktop window.
        drawRoundRect(
            color = BrandLight.copy(alpha = 0.35f + 0.45f * (0.5f - 0.5f * kotlin.math.cos(t * 2 * PI.toFloat()))),
            topLeft = qrTopLeft - Offset(qrSide * 0.13f, qrSide * 0.13f),
            size = Size(qrSide * 1.26f, qrSide * 1.26f),
            cornerRadius = CornerRadius(qrSide * 0.10f),
            style = Stroke(width = s * 0.008f),
        )

        // The phone, held up to the monitor: we see its screen.
        val bob = sin(t * 2 * PI.toFloat()) * s * 0.008f
        val phoneW = s * 0.30f
        val phoneH = phoneW * 1.95f
        val phoneCenter = Offset(center.x + s * 0.26f, size.height - phoneH * 0.48f + bob)
        val qrCenter = qrTopLeft + Offset(qrSide / 2, qrSide / 2)

        // Camera beam from the back of the phone (its origin is hidden
        // behind the phone body, drawn next).
        val beam = androidx.compose.ui.graphics.Path().apply {
            moveTo(phoneCenter.x - phoneW * 0.18f, phoneCenter.y - phoneH * 0.20f)
            lineTo(qrCenter.x - qrSide * 0.62f, qrCenter.y + qrSide * 0.62f)
            lineTo(qrCenter.x + qrSide * 0.62f, qrCenter.y - qrSide * 0.62f)
            lineTo(phoneCenter.x + phoneW * 0.18f, phoneCenter.y - phoneH * 0.20f)
            close()
        }
        drawPath(
            beam,
            brush = Brush.linearGradient(
                listOf(BrandLight.copy(alpha = 0.28f + 0.14f * l), Color.Transparent),
                start = phoneCenter,
                end = qrCenter,
            ),
        )

        rotate(-10f, pivot = phoneCenter) {
            val topLeft = phoneCenter - Offset(phoneW / 2, phoneH / 2)
            val corner = CornerRadius(phoneW * 0.17f)
            drawSoftShadow(topLeft + Offset(s * 0.01f, s * 0.02f), Size(phoneW, phoneH), phoneW * 0.17f, 0.16f)
            // Frame and bezel.
            drawRoundRect(
                brush = Brush.verticalGradient(
                    listOf(Color(0xFF3A3352), Color(0xFF1E1930)),
                    startY = topLeft.y,
                    endY = topLeft.y + phoneH,
                ),
                topLeft = topLeft,
                size = Size(phoneW, phoneH),
                cornerRadius = corner,
            )
            val bezel = phoneW * 0.045f
            val screenTopLeft = topLeft + Offset(bezel, bezel)
            val screenSize = Size(phoneW - 2 * bezel, phoneH - 2 * bezel)
            drawRoundRect(
                color = Color(0xFF0B0814),
                topLeft = screenTopLeft,
                size = screenSize,
                cornerRadius = CornerRadius(corner.x - bezel),
            )
            // Camera app on screen: the code in the viewfinder.
            val tileSide = screenSize.width * 0.62f
            val tileTopLeft = Offset(
                screenTopLeft.x + (screenSize.width - tileSide) / 2,
                screenTopLeft.y + screenSize.height * 0.30f,
            )
            drawRoundRect(
                color = Color.White,
                topLeft = tileTopLeft,
                size = Size(tileSide, tileSide),
                cornerRadius = CornerRadius(tileSide * 0.06f),
            )
            drawQrPattern(tileTopLeft + Offset(tileSide * 0.08f, tileSide * 0.08f), tileSide * 0.84f)
            // Viewfinder brackets lock onto the code.
            val tileCenter = tileTopLeft + Offset(tileSide / 2, tileSide / 2)
            val inset = lerp(tileSide * 0.30f, tileSide * 0.08f, l)
            val arm = tileSide * 0.18f
            val bracket = if (l > 0.98f) Color(0xFF2FBF71) else BrandLight
            val width = phoneW * 0.035f
            listOf(-1f to -1f, 1f to -1f, -1f to 1f, 1f to 1f).forEach { (dx, dy) ->
                val c = tileCenter + Offset(dx * (tileSide / 2 + inset), dy * (tileSide / 2 + inset))
                drawLine(bracket, c, c + Offset(-dx * arm, 0f), strokeWidth = width, cap = StrokeCap.Round)
                drawLine(bracket, c, c + Offset(0f, -dy * arm), strokeWidth = width, cap = StrokeCap.Round)
            }
            // Shutter button and hole-punch selfie camera.
            drawCircle(
                color = Color.White.copy(alpha = 0.85f),
                radius = phoneW * 0.075f,
                center = Offset(screenTopLeft.x + screenSize.width / 2, screenTopLeft.y + screenSize.height * 0.86f),
                style = Stroke(width = phoneW * 0.02f),
            )
            drawCircle(
                color = Color.Black,
                radius = phoneW * 0.035f,
                center = Offset(screenTopLeft.x + screenSize.width / 2, screenTopLeft.y + screenSize.height * 0.04f),
            )
        }
    }
}

/** A fixed, plausible-looking QR: three finder patterns and seeded modules. */
private fun DrawScope.drawQrPattern(topLeft: Offset, side: Float) {
    val n = 21
    val m = side / n
    val dark = Color(0xFF14101F)
    fun finder(cx: Int, cy: Int) {
        drawRect(dark, topLeft + Offset(cx * m, cy * m), Size(7 * m, 7 * m))
        drawRect(Color.White, topLeft + Offset((cx + 1) * m, (cy + 1) * m), Size(5 * m, 5 * m))
        drawRect(dark, topLeft + Offset((cx + 2) * m, (cy + 2) * m), Size(3 * m, 3 * m))
    }
    finder(0, 0); finder(n - 7, 0); finder(0, n - 7)
    var seed = 0x2F6A1B3
    for (y in 0 until n) for (x in 0 until n) {
        val inFinder = (x < 8 && y < 8) || (x >= n - 8 && y < 8) || (x < 8 && y >= n - 8)
        seed = seed * 1103515245 + 12345
        if (!inFinder && (seed ushr 16) and 1 == 0) {
            drawRect(dark, topLeft + Offset(x * m, y * m), Size(m, m))
        }
    }
}
