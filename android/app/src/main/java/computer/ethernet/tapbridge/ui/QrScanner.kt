package computer.ethernet.tapbridge.ui

import android.Manifest
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.LocalLifecycleOwner
import com.google.mlkit.vision.barcode.BarcodeScannerOptions
import com.google.mlkit.vision.barcode.BarcodeScanning
import com.google.mlkit.vision.barcode.common.Barcode
import com.google.mlkit.vision.common.InputImage
import java.util.concurrent.Executors

/**
 * QR pairing scanner: CameraX + ML Kit barcode-scanning (bundled model, no
 * Play-services-dependent scanner) inside an animated viewfinder.
 */
@androidx.annotation.OptIn(androidx.camera.core.ExperimentalGetImage::class)
@Composable
fun QrScanner(onCode: (String) -> Unit, modifier: Modifier = Modifier) {
    val context = LocalContext.current
    var hasPermission by remember {
        mutableStateOf(
            ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) ==
                PackageManager.PERMISSION_GRANTED
        )
    }
    val permissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted -> hasPermission = granted }

    LaunchedEffect(Unit) {
        if (!hasPermission) permissionLauncher.launch(Manifest.permission.CAMERA)
    }

    if (!hasPermission) {
        Text(
            "Camera permission is needed to scan the pairing QR",
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        return
    }

    val scanner = remember {
        BarcodeScanning.getClient(
            BarcodeScannerOptions.Builder()
                .setBarcodeFormats(Barcode.FORMAT_QR_CODE)
                .build()
        )
    }
    val executor = remember { Executors.newSingleThreadExecutor() }
    val lifecycleOwner = LocalLifecycleOwner.current
    var scanning by remember { mutableStateOf(true) }

    Box(modifier, contentAlignment = Alignment.Center) {
        AndroidView(
            modifier = Modifier.fillMaxSize(),
            factory = { viewContext ->
                val previewView = PreviewView(viewContext)
                val preview = Preview.Builder().build().also {
                    it.setSurfaceProvider(previewView.surfaceProvider)
                }
                val analysis = ImageAnalysis.Builder()
                    .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                    .build()
                analysis.setAnalyzer(executor) { proxy ->
                    if (!scanning) { proxy.close(); return@setAnalyzer }
                    fun analyze() {
                        val media = proxy.image ?: run { proxy.close(); return }
                        val image = InputImage.fromMediaImage(
                            media,
                            proxy.imageInfo.rotationDegrees,
                        )
                        scanner.process(image)
                            .addOnSuccessListener { barcodes ->
                                barcodes.firstOrNull()?.rawValue?.let { value ->
                                    if (scanning) {
                                        scanning = false
                                        onCode(value)
                                    }
                                }
                            }
                            .addOnCompleteListener { proxy.close() }
                    }
                    analyze()
                }
                val providerFuture = ProcessCameraProvider.getInstance(viewContext)
                providerFuture.addListener({
                    try {
                        val provider = providerFuture.get()
                        provider.unbindAll()
                        provider.bindToLifecycle(
                            lifecycleOwner,
                            CameraSelector.DEFAULT_BACK_CAMERA,
                            preview,
                            analysis,
                        )
                    } catch (_: Exception) {
                    }
                }, ContextCompat.getMainExecutor(viewContext))
                previewView
            },
        )
        ViewfinderOverlay(Modifier.size(260.dp))
    }
}

/** Animated viewfinder corners that breathe around the scanned area. */
@Composable
private fun ViewfinderOverlay(modifier: Modifier = Modifier) {
    val color = MaterialTheme.colorScheme.primary
    val breathe = rememberInfiniteTransition(label = "viewfinder").animateFloat(
        initialValue = 0f,
        targetValue = 1f,
        animationSpec = infiniteRepeatable(tween(1200, easing = LinearEasing)),
        label = "breathe",
    ).value
    Canvas(modifier.padding(8.dp)) {
        val inset = 6f + breathe * 3f
        val corner = 46f
        val w = size.width
        val h = size.height
        val stroke = 8f
        // Four corners, each an L of two rounded-cap lines.
        drawLine(color, Offset(inset, inset + corner), Offset(inset, inset), stroke, StrokeCap.Round)
        drawLine(color, Offset(inset, inset), Offset(inset + corner, inset), stroke, StrokeCap.Round)
        drawLine(color, Offset(w - inset, inset + corner), Offset(w - inset, inset), stroke, StrokeCap.Round)
        drawLine(color, Offset(w - inset, inset), Offset(w - inset - corner, inset), stroke, StrokeCap.Round)
        drawLine(color, Offset(inset, h - inset - corner), Offset(inset, h - inset), stroke, StrokeCap.Round)
        drawLine(color, Offset(inset, h - inset), Offset(inset + corner, h - inset), stroke, StrokeCap.Round)
        drawLine(color, Offset(w - inset, h - inset - corner), Offset(w - inset, h - inset), stroke, StrokeCap.Round)
        drawLine(color, Offset(w - inset, h - inset), Offset(w - inset - corner, h - inset), stroke, StrokeCap.Round)
    }
}
