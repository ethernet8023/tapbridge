package computer.ethernet.tapbridge

import android.Manifest
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.nfc.NfcAdapter
import android.os.Build
import androidx.core.app.NotificationCompat
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage

/**
 * FCM wake receiver (DESIGN "Nothing runs at rest"). Data message:
 * `{"t":"wake","host":ID,"hint":HINT}`. Anything without t=wake, or for a
 * host other than the paired one, is dropped. On match: start the
 * connectedDevice foreground service with the hint, post a heads-up
 * "Tap your key for <PC>" notification and enable the tap gate for 120 s
 * (the service manages the window).
 */
class WakeService : FirebaseMessagingService() {

    override fun onMessageReceived(message: RemoteMessage) {
        val data = message.data
        if (data["t"] != "wake") return
        val host = data["host"] ?: return
        val hint = data["hint"]
        val paired = IdentityStore.pairedHost(this) ?: return
        if (host != paired) return

        // Cache the newest hint so app-open reconnects dial fresh addresses.
        if (!hint.isNullOrBlank()) IdentityStore.saveHint(this, hint)

        val service = Intent(this, TapBridgeService::class.java)
            .setAction(TapBridgeService.ACTION_WAKE)
            .putExtra(TapBridgeService.EXTRA_HINT, hint ?: paired)
        try {
            startForegroundService(service)
        } catch (_: IllegalStateException) {
            // ForegroundServiceStartNotAllowedException (API 31+): FCM
            // delivered the message without its high-priority exemption
            // (e.g. the app exceeded its quota). The notification below still
            // opens the app, which connects from the foreground.
        }

        notifyHeadsUp()
    }

    private fun notifyHeadsUp() {
        if (Build.VERSION.SDK_INT >= 33 &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            // Without the runtime permission there is no heads-up; the FGS
            // notification still shows and the service still connects.
            return
        }
        val manager = getSystemService(NOTIFICATION_SERVICE) as NotificationManager
        manager.createNotificationChannel(
            NotificationChannel(
                CHANNEL_WAKE, "Tap requests",
                NotificationManager.IMPORTANCE_HIGH,
            ).apply { description = "Wake the phone to tap your security key" }
        )
        val launch = PendingIntent.getActivity(
            this, 0,
            Intent(this, MainActivity::class.java)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_IMMUTABLE,
        )
        val notification = NotificationCompat.Builder(this, CHANNEL_WAKE)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(getString(R.string.wake_title))
            .setContentText(
                IdentityStore.hostName(this)
                    ?.let { getString(R.string.wake_body_named, it) }
                    ?: getString(R.string.wake_body),
            )
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setContentIntent(launch)
            .setAutoCancel(true)
            .setTimeoutAfter(WAKE_WINDOW_MS)
            .build()
        manager.notify(WAKE_NOTIFICATION_ID, notification)
    }

    override fun onNewToken(token: String) {
        IdentityStore.savePushToken(this, token)
    }

    companion object {
        const val CHANNEL_WAKE = "tapbridge_wake"
        const val WAKE_NOTIFICATION_ID = 2
        const val WAKE_WINDOW_MS = 120_000L
    }
}
