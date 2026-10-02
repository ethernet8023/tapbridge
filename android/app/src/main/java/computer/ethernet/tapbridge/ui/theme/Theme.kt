package computer.ethernet.tapbridge.ui.theme

import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.ColorScheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalView

/** Brand colors from assets/brand/icon.svg (gradient #7C5CFF -> #2B1A80). */
val BrandViolet = Color(0xFF7C5CFF)
val BrandDeep = Color(0xFF2B1A80)
val BrandDeepLight = Color(0xFFB8A8FF)
val BrandSurfaceDark = Color(0xFF161030)
val BrandSurfaceLight = Color(0xFFF4F1FF)

/** Reduced-motion: ANIMATOR_DURATION_SCALE == 0 means render statically. */
val LocalReducedMotion = staticCompositionLocalOf { false }

private val LightColors: ColorScheme = lightColorScheme(
    primary = Color(0xFF5B3DD8),
    onPrimary = Color.White,
    primaryContainer = Color(0xFFE6DEFF),
    onPrimaryContainer = Color(0xFF210A66),
    secondary = Color(0xFF00B29B),
    onSecondary = Color.White,
    secondaryContainer = Color(0xFFCFF5EC),
    onSecondaryContainer = Color(0xFF00382F),
    tertiary = Color(0xFFB25400),
    onTertiary = Color.White,
    background = BrandSurfaceLight,
    onBackground = Color(0xFF1B1730),
    surface = BrandSurfaceLight,
    onSurface = Color(0xFF1B1730),
    surfaceVariant = Color(0xFFE7E0FF),
    onSurfaceVariant = Color(0xFF4A4462),
    error = Color(0xFFBA1A1A),
    errorContainer = Color(0xFFFFDAD6),
    onErrorContainer = Color(0xFF410002),
)

private val DarkColors: ColorScheme = darkColorScheme(
    primary = BrandDeepLight,
    onPrimary = Color(0xFF321A9E),
    primaryContainer = Color(0xFF4B32C4),
    onPrimaryContainer = Color(0xFFE6DEFF),
    secondary = Color(0xFF54DBC2),
    onSecondary = Color(0xFF00382F),
    secondaryContainer = Color(0xFF005044),
    onSecondaryContainer = Color(0xFFCFF5EC),
    tertiary = Color(0xFFFFB868),
    onTertiary = Color(0xFF4A2800),
    background = BrandSurfaceDark,
    onBackground = Color(0xFFE5DEFF),
    surface = BrandSurfaceDark,
    onSurface = Color(0xFFE5DEFF),
    surfaceVariant = Color(0xFF3A2F6E),
    onSurfaceVariant = Color(0xFFC7BEE6),
    error = Color(0xFFFFB4AB),
    errorContainer = Color(0xFF93000A),
    onErrorContainer = Color(0xFFFFDAD6),
)

@Composable
fun TapBridgeTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    // Dynamic color on Android 12+ keeps the system palette but the brand
    // fallback scheme carries the icon gradient when unavailable.
    dynamicColor: Boolean = true,
    content: @Composable () -> Unit,
) {
    val context = LocalContext.current
    val colors = when {
        dynamicColor && Build.VERSION.SDK_INT >= 31 ->
            if (darkTheme) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
        darkTheme -> DarkColors
        else -> LightColors
    }
    val view = LocalView.current
    androidx.compose.runtime.SideEffect {
        val window = (view.context as? android.app.Activity)?.window ?: return@SideEffect
        androidx.core.view.WindowCompat.getInsetsController(window, view)
            .isAppearanceLightStatusBars = !darkTheme
    }
    MaterialTheme(
        colorScheme = colors,
        typography = AppTypography,
        content = content,
    )
}

val AppTypography = androidx.compose.material3.Typography()
