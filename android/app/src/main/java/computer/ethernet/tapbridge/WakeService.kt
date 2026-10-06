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
 * `{"t":"wake","host":ID,"hint":HINT}`. Anything without t=wake, or whose
 * host is not one of the paired PCs, is dropped. On match: start the
 * connectedDevice foreground service with the wake host id + hint, post a
 * heads-up "Tap your key for <PC>" notification naming the requesting PC,
 * and enable the tap gate for 120 s (the service manages the window).
 *
 * The phone may hold many paired hosts; each wake names exactly one, and the
 * service dials that host's record (hint fallback: pushed hint, cached hint,
 * bare host id).
 */
class WakeService : FirebaseMessagingService() {

    override fun onMessageReceived(message: RemoteMessage) {
        val data = message.data
        if (data["t"] != "wake") return
        val host = data["host"] ?: return
        val hint = data["hint"]
        val wake = PairedHosts.routeForWake(IdentityStore.pairedHosts(this), host, hint)
            ?: return // unknown host: dropped, as mismatched hosts always were

        // Cache the newest hint for THIS host so app-open reconnects dial
        // fresh addresses.
        IdentityStore.saveHint(this, wake.hostId, wake.hint)
        IdentityStore.setLastUsedHost(this, wake.hostId)

        val service = Intent(this, TapBridgeService::class.java)
            .setAction(TapBridgeService.ACTION_WAKE)
            .putExtra(TapBridgeService.EXTRA_HOST, wake.hostId)
            .putExtra(TapBridgeService.EXTRA_HINT, wake.hint)
        try {
            // The service's foreground notification is the tap request.
            startForegroundService(service)
        } catch (_: IllegalStateException) {
            // ForegroundServiceStartNotAllowedException (API 31+): FCM
            // delivered the message without its high-priority exemption.
            // Post the same tap request directly; tapping it opens the app,
            // which connects from the foreground.
            notifyTapRequest(wake.hostId)
        }
    }

    /** Heads-up fallback naming the requesting PC. */
    private fun notifyTapRequest(hostId: String) {
        if (Build.VERSION.SDK_INT >= 33 &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) return
        val pc = IdentityStore.hostById(this, hostId)?.name
        TapBridgeService.ensureChannels(this)
        val launch = PendingIntent.getActivity(
            this, 0,
            Intent(this, MainActivity::class.java)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_IMMUTABLE,
        )
        val notification = NotificationCompat.Builder(this, TapBridgeService.CHANNEL_TAP_REQUEST)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(getString(R.string.wake_title))
            .setContentText(
                pc?.let { getString(R.string.wake_body_named, it) }
                    ?: getString(R.string.wake_body),
            )
            .setPriority(NotificationCompat.PRIORITY_MAX)
            .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
            .setContentIntent(launch)
            .setAutoCancel(true)
            .setTimeoutAfter(WAKE_WINDOW_MS)
            .build()
        getSystemService(NotificationManager::class.java).notify(WAKE_NOTIFICATION_ID, notification)
    }

    override fun onNewToken(token: String) {
        IdentityStore.savePushToken(this, token)
    }

    companion object {
        const val WAKE_NOTIFICATION_ID = 2
        const val WAKE_WINDOW_MS = 120_000L
    }
}
