package computer.ethernet.tapbridge

import android.app.Activity
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.nfc.NfcAdapter
import android.nfc.Tag
import android.nfc.tech.IsoDep
import android.os.Bundle
import java.io.IOException

/**
 * Translucent no-UI activity behind the disabled `TapGate` activity-alias.
 * Enabled only during a wake window; on dispatch it identifies the card
 * against registered keys and forwards a matched Tag to MainActivity
 * (singleTask, so no new task stack), or finishes silently otherwise.
 *
 * Matching happens here, not in the session: a foreign card must never reach
 * the bridge path.
 */
class TapGateActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        handle(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handle(intent)
    }

    private fun handle(intent: Intent?) {
        val tag = intent?.getParcelableExtra<Tag>(NfcAdapter.EXTRA_TAG)
        if (tag == null) {
            finish()
            return
        }
        // IsoDep I/O blocks; never on the main thread.
        Thread({
            val matched = try {
                identifyAndForward(tag)
            } catch (_: IOException) {
                false
            }
            runOnUiThread {
                if (matched && !isFinishing) {
                    // singleTask MainActivity comes to the foreground with the Tag.
                    startActivity(
                        Intent(this, MainActivity::class.java)
                            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
                            .putExtra(MainActivity.EXTRA_FORWARDED_TAG, tag),
                    )
                }
                finish()
            }
        }, "tapbridge-gate").start()
    }

    /** True when the card matches a registered key; closes IsoDep either way. */
    private fun identifyAndForward(tag: Tag): Boolean {
        val iso = IsoDep.get(tag) ?: return false
        val uid = tag.id
        var aaguid: ByteArray? = null
        try {
            iso.connect()
            iso.timeout = 5000
            if (Ctap2.selectFido(iso)) {
                val info = Ctap2.getInfoResponse(iso)
                if (info != null) aaguid = Ctap2.aaguidFromInfo(info)
            }
        } catch (_: Exception) {
            aaguid = null
        } finally {
            // Close IsoDep to release the card; never replay APDUs later.
            runCatching { iso.close() }
        }
        val records = RegisteredKeys.list(this)
        return RegisteredKeys.anyMatch(records, uid.takeIf { RegisteredKeys.uidStable(it) }, aaguid) != null
    }

    companion object {
        /** Enable the alias during a wake window. */
        fun setGateEnabled(context: Context, enabled: Boolean) {
            val pm = context.packageManager
            pm.setComponentEnabledSetting(
                ComponentName(context, TapGateActivity::class.java.`package`!!.getName() + ".TapGate"),
                if (enabled) {
                    android.content.pm.PackageManager.COMPONENT_ENABLED_STATE_ENABLED
                } else {
                    android.content.pm.PackageManager.COMPONENT_ENABLED_STATE_DISABLED
                },
                android.content.pm.PackageManager.DONT_KILL_APP,
            )
        }
    }
}
