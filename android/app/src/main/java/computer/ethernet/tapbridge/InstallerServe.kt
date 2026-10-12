package computer.ethernet.tapbridge

import java.io.BufferedInputStream
import java.io.BufferedOutputStream
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.net.Socket
import java.net.SocketException
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

/**
 * "Install tapbridge on a PC" (USB plan, Track I): the phone serves the
 * Windows installers that CI bundled into assets/installer/, over USB
 * tethering or Wi-Fi, while that screen is open. Plain JVM code (no Android
 * types) so the parsing, routing and the server itself run in unit tests.
 */
object InstallerHttp {
    /** Request head cap: request line plus headers, CRLFCRLF included. */
    const val MAX_HEAD_BYTES = 8 * 1024
    const val PATH_PREFIX = "/installer/"
    const val CONTENT_TYPE_EXE = "application/vnd.microsoft.portable-executable"

    /** Bundled installer names (Inno Setup's tapbridge-<ver>-<arch>-setup.exe). */
    private val NAME = Regex("^[A-Za-z0-9][A-Za-z0-9._-]{0,127}-setup\\.exe$")

    /** Only these names are ever served or listed: no separators, no "..". */
    fun isInstallerName(name: String): Boolean = NAME.matches(name) && !name.contains("..")

    /** The installers among an asset listing, sorted for a stable page. */
    fun installers(assetNames: Collection<String>): List<String> =
        assetNames.filter(::isInstallerName).distinct().sorted()

    enum class Arch(val label: String, val hint: String) {
        X64("Windows x64", "Most Windows PCs (Intel or AMD processor)."),
        ARM64("Windows on Arm (arm64)", "PCs with a Snapdragon or other Arm processor."),
    }

    fun archOf(name: String): Arch? {
        val n = name.lowercase()
        return when {
            n.contains("arm64") || n.contains("aarch64") -> Arch.ARM64
            n.contains("x64") || n.contains("x86_64") || n.contains("amd64") -> Arch.X64
            else -> null
        }
    }

    data class Request(val method: String, val path: String)

    sealed class Parse {
        data class Ok(val request: Request) : Parse()
        object Bad : Parse()
    }

    /** Parses the request line; headers are read but ignored. */
    fun parseHead(head: String): Parse {
        val line = head.substringBefore("\r\n").substringBefore('\n')
        val parts = line.split(' ')
        if (parts.size != 3) return Parse.Bad
        val (method, target, version) = parts
        if (method.isEmpty() || !method.all { it in 'A'..'Z' }) return Parse.Bad
        if (version != "HTTP/1.1" && version != "HTTP/1.0") return Parse.Bad
        if (!target.startsWith("/") || target.any { it.code <= 0x20 || it.code >= 0x7f }) return Parse.Bad
        return Parse.Ok(Request(method, target.substringBefore('?').substringBefore('#')))
    }

    sealed class Route {
        object Index : Route()
        data class File(val name: String) : Route()
        object NotFound : Route()
        object MethodNotAllowed : Route()
    }

    /**
     * Fixed paths only: "/" and "/installer/<exact bundled name>". The
     * name must be in [available] verbatim, so encoded or traversal
     * paths ("..", "%2e", "/", "\\") never match anything.
     */
    fun route(request: Request, available: Collection<String>): Route {
        if (request.method != "GET") return Route.MethodNotAllowed
        if (request.path == "/") return Route.Index
        if (request.path.startsWith(PATH_PREFIX)) {
            val name = request.path.removePrefix(PATH_PREFIX)
            if (isInstallerName(name) && name in available) return Route.File(name)
        }
        return Route.NotFound
    }

    private fun esc(s: String) = s
        .replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
        .replace("\"", "&quot;").replace("'", "&#39;")

    fun indexHtml(installers: List<String>): String {
        val items = installers.joinToString("\n") { name ->
            val arch = archOf(name)
            val label = arch?.label ?: "Windows installer"
            val hint = arch?.hint?.let { "<br><small>${esc(it)}</small>" } ?: ""
            "<li><a href=\"$PATH_PREFIX${esc(name)}\">${esc(label)}</a> " +
                "<code>${esc(name)}</code>$hint</li>"
        }
        val help = if (installers.size > 1) {
            "<p>Not sure which one? Open <b>Settings &rarr; System &rarr; About</b> on the PC. " +
                "&ldquo;x64-based processor&rdquo; means x64; &ldquo;ARM-based processor&rdquo; means arm64.</p>"
        } else ""
        return """<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Install tapbridge</title>
<style>body{font-family:system-ui,sans-serif;max-width:40em;margin:2em auto;padding:0 1em;line-height:1.5}
li{margin:.8em 0}a{font-weight:600}code{font-size:.85em;color:#555}</style>
</head><body>
<h1>Install tapbridge on this PC</h1>
<p>Download the installer for this PC, then run it.</p>
<ul>
$items
</ul>
$help
<p><small>Served by the tapbridge app on your phone. Close that screen on the phone when you're done.</small></p>
</body></html>
"""
    }

    fun status(code: Int): String = when (code) {
        200 -> "OK"
        400 -> "Bad Request"
        404 -> "Not Found"
        405 -> "Method Not Allowed"
        408 -> "Request Timeout"
        431 -> "Request Header Fields Too Large"
        500 -> "Internal Server Error"
        else -> "Error"
    }

    /** Response head; every response closes the connection. */
    fun head(code: Int, headers: List<Pair<String, String>>): ByteArray {
        val sb = StringBuilder("HTTP/1.1 $code ${status(code)}\r\n")
        for ((k, v) in headers) sb.append(k).append(": ").append(v).append("\r\n")
        sb.append("Connection: close\r\n")
        sb.append("Cache-Control: no-store\r\n")
        sb.append("X-Content-Type-Options: nosniff\r\n\r\n")
        return sb.toString().toByteArray(Charsets.ISO_8859_1)
    }

    sealed class HeadRead {
        data class Ok(val head: String) : HeadRead()
        object TooLarge : HeadRead()
        object Eof : HeadRead()
    }

    /** Reads up to the blank line (CRLFCRLF, or bare LFLF), at most [max] bytes. */
    fun readHead(input: InputStream, max: Int = MAX_HEAD_BYTES): HeadRead {
        val buf = java.io.ByteArrayOutputStream()
        var tail = 0 // last four bytes, newest in the low byte
        while (true) {
            val b = input.read()
            if (b < 0) return HeadRead.Eof
            if (buf.size() >= max) return HeadRead.TooLarge
            buf.write(b)
            tail = (tail shl 8) or b
            if (tail == 0x0d0a0d0a || (tail and 0xffff) == 0x0a0a) {
                return HeadRead.Ok(buf.toString(Charsets.ISO_8859_1.name()))
            }
        }
    }
}

/** Which URL to show the user for each network the phone is on. */
object InstallerAddresses {
    enum class Kind { USB, WIFI }

    data class Candidate(val interfaceName: String, val ipv4: String)
    data class Url(val kind: Kind, val url: String)

    private val USB = listOf("rndis", "ncm", "usb")
    // wlan: Wi-Fi client; ap/swlan/softap: the phone's own hotspot.
    private val WIFI = listOf("wlan", "swlan", "ap", "softap")

    fun kindOf(interfaceName: String): Kind? {
        val n = interfaceName.lowercase()
        return when {
            USB.any { n.startsWith(it) } -> Kind.USB
            WIFI.any { n.startsWith(it) } -> Kind.WIFI
            else -> null
        }
    }

    private fun usableIpv4(ip: String): Boolean {
        val parts = ip.split('.')
        if (parts.size != 4) return false
        val o = parts.map { it.toIntOrNull() ?: return false }
        if (o.any { it !in 0..255 }) return false
        return o[0] != 127 && o[0] != 0 && !(o[0] == 169 && o[1] == 254)
    }

    /** USB tethering first, then Wi-Fi; mobile data and loopback never. */
    fun select(candidates: List<Candidate>, port: Int): List<Url> =
        candidates
            .mapNotNull { c -> kindOf(c.interfaceName)?.takeIf { usableIpv4(c.ipv4) }?.let { it to c } }
            .sortedWith(compareBy({ it.first.ordinal }, { it.second.interfaceName }, { it.second.ipv4 }))
            .map { (kind, c) -> Url(kind, "http://${c.ipv4}:$port/") }
            .distinct()
}

/**
 * Tiny HTTP/1.1 server for the bundled installers. One accept thread plus
 * a small pool; every connection is one request, then closed. Logs
 * nothing; the only thing it reports is a count of finished downloads.
 */
class InstallerServer(
    private val source: Source,
    private val onDownload: (Int) -> Unit = {},
) {
    interface Source {
        /** Installer file names (already whitelisted by the caller or not; filtered again here). */
        val names: List<String>
        fun length(name: String): Long
        fun open(name: String): InputStream
    }

    companion object {
        const val DEFAULT_PORT = 8099
        val FALLBACK_PORTS = (8100..8109).toList()
        const val READ_TIMEOUT_MS = 10_000
        const val THREADS = 4
    }

    private val names = InstallerHttp.installers(source.names)
    private val downloads = AtomicInteger(0)
    @Volatile private var socket: ServerSocket? = null
    private var pool: ThreadPoolExecutor? = null
    private var acceptThread: Thread? = null
    /** Open client sockets, closed on [stop] (a blocked write ignores interrupts). */
    private val clients = java.util.Collections.newSetFromMap(java.util.concurrent.ConcurrentHashMap<Socket, Boolean>())

    val downloadCount: Int get() = downloads.get()

    /** Binds (8099, then 8100..8109, then any free port) and returns the port. */
    @Synchronized
    fun start(ports: List<Int> = listOf(DEFAULT_PORT) + FALLBACK_PORTS + 0): Int {
        socket?.let { return it.localPort }
        var bound: ServerSocket? = null
        for (p in ports) {
            val s = ServerSocket()
            try {
                s.reuseAddress = true
                s.bind(InetSocketAddress(p), 16)
                bound = s
                break
            } catch (_: IOException) {
                runCatching { s.close() }
            }
        }
        val server = bound ?: throw IOException("no free port")
        val workers = ThreadPoolExecutor(
            THREADS, THREADS, 30, TimeUnit.SECONDS, ArrayBlockingQueue(8),
        ) { r -> Thread(r, "installer-http").apply { isDaemon = true } }
        socket = server
        pool = workers
        acceptThread = Thread({ acceptLoop(server, workers) }, "installer-accept").apply {
            isDaemon = true
            start()
        }
        return server.localPort
    }

    @Synchronized
    fun stop() {
        runCatching { socket?.close() }
        socket = null
        pool?.shutdownNow()
        pool = null
        acceptThread?.interrupt()
        acceptThread = null
        clients.forEach { runCatching { it.close() } }
        clients.clear()
    }

    private fun acceptLoop(server: ServerSocket, workers: ThreadPoolExecutor) {
        while (!server.isClosed) {
            val client = try {
                server.accept()
            } catch (_: IOException) {
                break
            }
            try {
                workers.execute { handle(client) }
            } catch (_: RejectedExecutionException) {
                runCatching { client.close() }
            }
        }
    }

    private fun handle(client: Socket) {
        clients.add(client)
        if (socket == null) {
            clients.remove(client)
            runCatching { client.close() }
            return
        }
        client.use { s ->
            try {
                s.soTimeout = READ_TIMEOUT_MS
                s.tcpNoDelay = true
                val input = BufferedInputStream(s.getInputStream())
                val out = BufferedOutputStream(s.getOutputStream(), 64 * 1024)
                serve(input, out)
                out.flush()
                // Lingering close: an early error reply (431, 400) can leave
                // request bytes unread, and closing on them sends a reset
                // that may eat the reply. Drain a little first.
                s.shutdownOutput()
                runCatching {
                    s.soTimeout = 1000
                    val sink = ByteArray(4096)
                    var drained = 0
                    while (drained < 64 * 1024) {
                        val n = input.read(sink)
                        if (n < 0) break
                        drained += n
                    }
                }
            } catch (_: java.net.SocketTimeoutException) {
                runCatching {
                    s.getOutputStream().write(InstallerHttp.head(408, listOf("Content-Length" to "0")))
                }
            } catch (_: IOException) {
                // Client went away mid-transfer: nothing to report.
            } finally {
                clients.remove(s)
            }
        }
    }

    private fun simple(out: OutputStream, code: Int, extra: List<Pair<String, String>> = emptyList()) {
        val body = "${code} ${InstallerHttp.status(code)}\n".toByteArray()
        out.write(
            InstallerHttp.head(
                code,
                extra + listOf("Content-Type" to "text/plain; charset=utf-8", "Content-Length" to body.size.toString()),
            ),
        )
        out.write(body)
    }

    private fun serve(input: InputStream, out: OutputStream) {
        val head = when (val r = InstallerHttp.readHead(input)) {
            is InstallerHttp.HeadRead.Ok -> r.head
            InstallerHttp.HeadRead.TooLarge -> return simple(out, 431)
            InstallerHttp.HeadRead.Eof -> return
        }
        val request = when (val p = InstallerHttp.parseHead(head)) {
            is InstallerHttp.Parse.Ok -> p.request
            InstallerHttp.Parse.Bad -> return simple(out, 400)
        }
        when (val route = InstallerHttp.route(request, names)) {
            InstallerHttp.Route.MethodNotAllowed -> simple(out, 405, listOf("Allow" to "GET"))
            InstallerHttp.Route.NotFound -> simple(out, 404)
            InstallerHttp.Route.Index -> {
                val body = InstallerHttp.indexHtml(names).toByteArray(Charsets.UTF_8)
                out.write(
                    InstallerHttp.head(
                        200,
                        listOf(
                            "Content-Type" to "text/html; charset=utf-8",
                            "Content-Length" to body.size.toString(),
                        ),
                    ),
                )
                out.write(body)
            }
            is InstallerHttp.Route.File -> sendFile(out, route.name)
        }
    }

    private fun sendFile(out: OutputStream, name: String) {
        val length = try {
            source.length(name)
        } catch (_: IOException) {
            return simple(out, 500)
        }
        val stream = try {
            source.open(name)
        } catch (_: IOException) {
            return simple(out, 500)
        }
        stream.use { file ->
            out.write(
                InstallerHttp.head(
                    200,
                    listOf(
                        "Content-Type" to InstallerHttp.CONTENT_TYPE_EXE,
                        "Content-Disposition" to "attachment; filename=\"$name\"",
                        "Content-Length" to length.toString(),
                    ),
                ),
            )
            val buf = ByteArray(64 * 1024)
            var sent = 0L
            while (sent < length) {
                val n = file.read(buf, 0, minOf(buf.size.toLong(), length - sent).toInt())
                if (n < 0) break
                out.write(buf, 0, n)
                sent += n
            }
            out.flush()
            // A short asset would break Content-Length: drop the connection
            // rather than count it.
            if (sent != length) throw SocketException("short asset")
        }
        onDownload(downloads.incrementAndGet())
    }
}
