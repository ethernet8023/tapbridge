package dev.tapbridge.app

import android.app.PendingIntent
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.nfc.NfcAdapter
import android.nfc.Tag
import android.nfc.tech.IsoDep
import android.nfc.tech.NfcA
import android.nfc.tech.NfcB
import android.os.Build
import android.os.Bundle
import android.os.IBinder
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.TextView
import android.widget.Toast
import androidx.appcompat.app.AppCompatActivity

/**
 * Plain material-ish UI: status text, invitation paste field, connect button,
 * start/stop service. NFC reader-mode is enabled only while resumed.
 */
class MainActivity : AppCompatActivity() {
    private var service: TapBridgeService? = null
    private var bound = false
    private var resumed = false
    private var nfc: NfcAdapter? = null

    private lateinit var statusView: TextView
    private lateinit var inviteInput: EditText
    private lateinit var connectBtn: Button
    private lateinit var startBtn: Button
    private lateinit var stopBtn: Button
    private lateinit var cardView: TextView

    private val conn = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName?, binder: IBinder?) {
            service = (binder as TapBridgeService.LocalBinder).service
            bound = true
            if (resumed) service?.resumeTagDiscovery() else service?.pauseTagDiscovery()
            service?.listener = object : TapBridgeService.Listener {
                override fun onStatusChanged(status: TapBridgeService.Status, error: String?) {
                    runOnUiThread { renderStatus(status, error) }
                }

                override fun onCardPresent(present: Boolean) {
                    runOnUiThread {
                        cardView.text =
                            if (present) "Card present — exchange in progress"
                            else "Open the app to tap"
                    }
                }
            }
            renderStatus(service!!.status, service?.lastError)
        }

        override fun onServiceDisconnected(name: ComponentName?) {
            service = null
            bound = false
        }
    }

    private val readerCallback = object : NfcAdapter.ReaderCallback {
        override fun onTagDiscovered(tag: Tag) {
            service?.onTagDiscovered(tag)
                ?: runOnUiThread {
                    Toast.makeText(this@MainActivity, "Start the service first", Toast.LENGTH_SHORT).show()
                }
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        NativeBridge.initialize(applicationContext)

        nfc = NfcAdapter.getDefaultAdapter(this)

        statusView = TextView(this).apply { textSize = 18f }
        cardView = TextView(this)
        inviteInput = EditText(this).apply {
            hint = "Paste invitation (tapbridge:1:…)"
            inputType = (android.text.InputType.TYPE_CLASS_TEXT
                or android.text.InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS)
        }
        connectBtn = Button(this).apply {
            text = "Connect"
            setOnClickListener { onConnectClicked() }
        }
        startBtn = Button(this).apply {
            text = "Start service"
            setOnClickListener { startTapService() }
        }
        stopBtn = Button(this).apply {
            text = "Stop service"
            setOnClickListener {
                sendStop()
            }
        }

        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(48, 64, 48, 48)
            addView(statusView)
            addView(cardView)
            addView(inviteInput)
            addView(connectBtn)
            addView(startBtn)
            addView(stopBtn)
        }
        setContentView(root)

        if (Build.VERSION.SDK_INT >= 33) {
            requestPermissions(arrayOf(android.Manifest.permission.POST_NOTIFICATIONS), 1)
        }

        bindService(Intent(this, TapBridgeService::class.java), conn, Context.BIND_AUTO_CREATE)
    }

    private fun onConnectClicked() {
        val text = inviteInput.text.toString().trim()
        val parts = Invitation.parse(text)
        if (parts == null) {
            Toast.makeText(this, "Invalid invitation format", Toast.LENGTH_LONG).show()
            return
        }
        // Keep key+host candidate so a lost ack can retry with the same host.
        PairingState.setPendingInvitation(text)
        startTapService()
    }

    private fun startTapService() {
        val i = Intent(this, TapBridgeService::class.java)
            .setAction(TapBridgeService.ACTION_START)
        if (Build.VERSION.SDK_INT >= 26) startForegroundService(i) else startService(i)
    }

    private fun sendStop() {
        startService(Intent(this, TapBridgeService::class.java).setAction(TapBridgeService.ACTION_STOP))
    }

    private fun renderStatus(s: TapBridgeService.Status, err: String?) {
        statusView.text = when (s) {
            TapBridgeService.Status.IDLE -> "Idle"
            TapBridgeService.Status.CONNECTING -> "Connecting…"
            TapBridgeService.Status.CONNECTED -> "Connected"
            TapBridgeService.Status.ERROR -> "Error: ${err ?: "unknown"}"
        }
    }

    override fun onResume() {
        super.onResume()
        resumed = true
        service?.resumeTagDiscovery()
        // Enable reader-mode only while resumed; foreground dispatch with
        nfc?.enableReaderMode(
            this, readerCallback,
            NfcAdapter.FLAG_READER_NFC_A
                or NfcAdapter.FLAG_READER_NFC_B
                or NfcAdapter.FLAG_READER_SKIP_NDEF_CHECK,
            null,
        )
    }

    override fun onPause() {
        // ALWAYS retire/close tag when leaving the foreground.
        resumed = false
        service?.pauseTagDiscovery()
        try {
            nfc?.disableReaderMode(this)
        } catch (e: IllegalStateException) { /* adapter already off */ }
        super.onPause()
    }

    override fun onDestroy() {
        if (bound) {
            service?.listener = null
            unbindService(conn)
            bound = false
        }
        super.onDestroy()
    }
}
