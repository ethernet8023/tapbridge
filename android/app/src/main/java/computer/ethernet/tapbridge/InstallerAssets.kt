package computer.ethernet.tapbridge

import android.content.ActivityNotFoundException
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.res.AssetManager
import android.provider.Settings
import java.io.IOException
import java.io.InputStream
import java.net.Inet4Address
import java.net.NetworkInterface

/**
 * Android side of "Install tapbridge on a PC": the installers CI bundled
 * into assets/installer/ (absent in local builds, which hides the feature),
 * this phone's addresses, and the tethering settings shortcut.
 */
object InstallerAssets {
    const val DIR = "installer"

    /** Bundled installer names; empty when the APK carries none. */
    fun names(context: Context): List<String> =
        InstallerHttp.installers(runCatching { context.assets.list(DIR)?.toList() }.getOrNull().orEmpty())

    fun source(context: Context): InstallerServer.Source = AssetSource(context.applicationContext.assets, names(context))

    private class AssetSource(private val assets: AssetManager, override val names: List<String>) : InstallerServer.Source {
        private val lengths = java.util.concurrent.ConcurrentHashMap<String, Long>()

        override fun length(name: String): Long = lengths.getOrPut(name) {
            // Stored uncompressed (noCompress "exe"), so openFd knows the
            // size; count the bytes once if a build ever compresses it.
            try {
                assets.openFd("$DIR/$name").use { it.length }
            } catch (_: IOException) {
                open(name).use { s ->
                    val buf = ByteArray(64 * 1024)
                    var total = 0L
                    while (true) {
                        val n = s.read(buf)
                        if (n < 0) break
                        total += n
                    }
                    total
                }
            }
        }

        override fun open(name: String): InputStream {
            if (name !in names) throw IOException("not bundled")
            return assets.open("$DIR/$name", AssetManager.ACCESS_STREAMING)
        }
    }

    /** URLs for the PC to open: USB tethering first, then Wi-Fi. */
    fun urls(port: Int): List<InstallerAddresses.Url> {
        val candidates = runCatching {
            NetworkInterface.getNetworkInterfaces()?.toList().orEmpty()
                .filter { runCatching { it.isUp && !it.isLoopback }.getOrDefault(false) }
                .flatMap { nif ->
                    nif.inetAddresses.toList().filterIsInstance<Inet4Address>().mapNotNull { a ->
                        a.hostAddress?.let { InstallerAddresses.Candidate(nif.name, it) }
                    }
                }
        }.getOrDefault(emptyList())
        return InstallerAddresses.select(candidates, port)
    }

    /** Opens the tethering page, or the network settings where it isn't reachable directly. */
    fun openTetherSettings(context: Context) {
        val direct = Intent().setComponent(ComponentName("com.android.settings", "com.android.settings.TetherSettings"))
        val fallback = Intent(Settings.ACTION_WIRELESS_SETTINGS)
        for (intent in listOf(direct, fallback)) {
            try {
                context.startActivity(intent)
                return
            } catch (_: ActivityNotFoundException) {
            } catch (_: SecurityException) {
            }
        }
    }
}
