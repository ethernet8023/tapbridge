package computer.ethernet.tapbridge

import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.ByteArrayInputStream
import java.io.InputStream
import java.net.InetSocketAddress
import java.net.Socket

/** Installer serving (USB plan, Track I): parsing, routing, whitelist, IP pick, and the live server. */
class InstallerServeTest {
    private val x64 = "tapbridge-0.2.0-x64-setup.exe"
    private val arm = "tapbridge-0.2.0-arm64-setup.exe"
    private val both = listOf(x64, arm)

    // ------------------------------------------------------------ whitelist

    @Test fun acceptsBundledNames() {
        assertTrue(InstallerHttp.isInstallerName(x64))
        assertTrue(InstallerHttp.isInstallerName(arm))
    }

    @Test fun rejectsEverythingElse() {
        listOf(
            "", "setup.exe", "../tapbridge-x64-setup.exe", "a/b-setup.exe", "a\\b-setup.exe",
            "..-setup.exe", "a..b-setup.exe", ".hidden-setup.exe", "tapbridge.exe", "x-setup.exe.txt",
            "x-setup.EXE", "x y-setup.exe", "x%2f-setup.exe", "x" + "a".repeat(200) + "-setup.exe",
        ).forEach { assertFalse(it, InstallerHttp.isInstallerName(it)) }
    }

    @Test fun installerListingFiltersAndSorts() {
        assertEquals(listOf(arm, x64), InstallerHttp.installers(listOf(x64, "README", arm, x64, "x.so")))
        assertTrue(InstallerHttp.installers(emptyList()).isEmpty())
    }

    @Test fun archLabels() {
        assertEquals(InstallerHttp.Arch.X64, InstallerHttp.archOf(x64))
        assertEquals(InstallerHttp.Arch.ARM64, InstallerHttp.archOf(arm))
        assertNull(InstallerHttp.archOf("tapbridge-setup.exe"))
    }

    // -------------------------------------------------------------- parsing

    private fun ok(head: String) = (InstallerHttp.parseHead(head) as InstallerHttp.Parse.Ok).request

    @Test fun parsesRequestLine() {
        assertEquals(InstallerHttp.Request("GET", "/"), ok("GET / HTTP/1.1\r\nHost: x\r\n\r\n"))
        assertEquals(InstallerHttp.Request("GET", "/installer/$x64"), ok("GET /installer/$x64?x=1 HTTP/1.0\r\n\r\n"))
        assertEquals(InstallerHttp.Request("POST", "/"), ok("POST / HTTP/1.1\r\n\r\n"))
    }

    @Test fun rejectsMalformedRequestLines() {
        listOf(
            "GET /\r\n\r\n", "GET  / HTTP/1.1\r\n\r\n", "get / HTTP/1.1\r\n\r\n", "GET x HTTP/1.1\r\n\r\n",
            "GET / HTTP/2\r\n\r\n", "GET http://evil/ HTTP/1.1\r\n\r\n", "GET /\u00e9 HTTP/1.1\r\n\r\n", "",
        ).forEach { assertEquals(it, InstallerHttp.Parse.Bad, InstallerHttp.parseHead(it)) }
    }

    @Test fun readsHeadUpToBlankLineAndCapsSize() {
        val r = InstallerHttp.readHead(ByteArrayInputStream("GET / HTTP/1.1\r\nA: b\r\n\r\nBODY".toByteArray()))
        assertEquals(InstallerHttp.HeadRead.Ok("GET / HTTP/1.1\r\nA: b\r\n\r\n"), r)
        val lf = InstallerHttp.readHead(ByteArrayInputStream("GET / HTTP/1.1\n\n".toByteArray()))
        assertTrue(lf is InstallerHttp.HeadRead.Ok)
        val big = "GET / HTTP/1.1\r\nX: " + "a".repeat(10_000) + "\r\n\r\n"
        assertEquals(InstallerHttp.HeadRead.TooLarge, InstallerHttp.readHead(ByteArrayInputStream(big.toByteArray())))
        assertEquals(InstallerHttp.HeadRead.Eof, InstallerHttp.readHead(ByteArrayInputStream("GET / HT".toByteArray())))
    }

    // -------------------------------------------------------------- routing

    private fun route(method: String, path: String) = InstallerHttp.route(InstallerHttp.Request(method, path), both)

    @Test fun routes() {
        assertEquals(InstallerHttp.Route.Index, route("GET", "/"))
        assertEquals(InstallerHttp.Route.File(x64), route("GET", "/installer/$x64"))
        assertEquals(InstallerHttp.Route.MethodNotAllowed, route("POST", "/"))
        assertEquals(InstallerHttp.Route.MethodNotAllowed, route("HEAD", "/installer/$x64"))
        listOf(
            "/index.html", "/installer/", "/installer", "/installer/other-setup.exe",
            "/installer/../$x64", "/installer/%2e%2e/$x64", "/installer/x/$x64", "/installer//$x64",
            "/installer/..%2f$x64", "//", "/installer/$x64/",
        ).forEach { assertEquals(it, InstallerHttp.Route.NotFound, route("GET", it)) }
    }

    @Test fun indexListsInstallersEscaped() {
        val html = InstallerHttp.indexHtml(both)
        assertTrue(html.contains("href=\"/installer/$x64\""))
        assertTrue(html.contains("Windows x64"))
        assertTrue(html.contains("arm64"))
        assertTrue(html.contains("Not sure which one"))
        assertFalse(InstallerHttp.indexHtml(listOf(x64)).contains("Not sure which one"))
    }

    // ------------------------------------------------------------ addresses

    private fun c(n: String, ip: String) = InstallerAddresses.Candidate(n, ip)

    @Test fun usbBeforeWifiAndNoMobileData() {
        val urls = InstallerAddresses.select(
            listOf(
                c("wlan0", "192.168.1.20"), c("rmnet_data0", "10.1.2.3"), c("lo", "127.0.0.1"),
                c("rndis0", "192.168.42.129"), c("ncm0", "192.168.50.1"), c("usb0", "169.254.3.4"),
                c("dummy0", "10.0.0.1"),
            ),
            8099,
        )
        assertEquals(
            listOf(
                InstallerAddresses.Url(InstallerAddresses.Kind.USB, "http://192.168.50.1:8099/"),
                InstallerAddresses.Url(InstallerAddresses.Kind.USB, "http://192.168.42.129:8099/"),
                InstallerAddresses.Url(InstallerAddresses.Kind.WIFI, "http://192.168.1.20:8099/"),
            ),
            urls,
        )
    }

    @Test fun interfaceKinds() {
        assertEquals(InstallerAddresses.Kind.USB, InstallerAddresses.kindOf("rndis0"))
        assertEquals(InstallerAddresses.Kind.USB, InstallerAddresses.kindOf("usb0"))
        assertEquals(InstallerAddresses.Kind.USB, InstallerAddresses.kindOf("ncm1"))
        assertEquals(InstallerAddresses.Kind.WIFI, InstallerAddresses.kindOf("wlan1"))
        assertEquals(InstallerAddresses.Kind.WIFI, InstallerAddresses.kindOf("swlan0"))
        assertNull(InstallerAddresses.kindOf("rmnet0"))
        assertNull(InstallerAddresses.kindOf("tun0"))
        assertTrue(InstallerAddresses.select(listOf(c("wlan0", "not-an-ip")), 1).isEmpty())
    }

    // ---------------------------------------------------------- live server

    private val payload = ByteArray(300_000) { (it * 31 + 7).toByte() }
    private var server: InstallerServer? = null
    private val counts = mutableListOf<Int>()

    private fun start(): Int {
        val source = object : InstallerServer.Source {
            override val names = both + "README.txt"
            override fun length(name: String) = payload.size.toLong()
            override fun open(name: String): InputStream = ByteArrayInputStream(payload)
        }
        val s = InstallerServer(source) { synchronized(counts) { counts += it } }
        server = s
        return s.start(listOf(0))
    }

    @After fun stop() { server?.stop() }

    private class Response(val status: Int, val headers: Map<String, String>, val body: ByteArray)

    private fun send(port: Int, raw: ByteArray): Response {
        Socket().use { s ->
            s.connect(InetSocketAddress("127.0.0.1", port), 2000)
            s.soTimeout = 5000
            s.getOutputStream().write(raw)
            s.getOutputStream().flush()
            val all = s.getInputStream().readBytes()
            val split = (0 until all.size - 3).first {
                all[it] == '\r'.code.toByte() && all[it + 1] == '\n'.code.toByte() &&
                    all[it + 2] == '\r'.code.toByte() && all[it + 3] == '\n'.code.toByte()
            }
            val lines = String(all, 0, split, Charsets.ISO_8859_1).split("\r\n")
            val status = lines[0].split(' ')[1].toInt()
            val headers = lines.drop(1).associate { it.substringBefore(": ").lowercase() to it.substringAfter(": ") }
            return Response(status, headers, all.copyOfRange(split + 4, all.size))
        }
    }

    private fun get(port: Int, path: String, method: String = "GET") =
        send(port, "$method $path HTTP/1.1\r\nHost: phone\r\n\r\n".toByteArray())

    @Test fun servesIndexAndInstaller() {
        val port = start()
        val index = get(port, "/")
        assertEquals(200, index.status)
        assertTrue(index.headers["content-type"]!!.startsWith("text/html"))
        val html = String(index.body)
        assertTrue(html.contains(x64) && html.contains(arm))
        assertFalse(html.contains("README"))

        val file = get(port, "/installer/$arm")
        assertEquals(200, file.status)
        assertEquals(InstallerHttp.CONTENT_TYPE_EXE, file.headers["content-type"])
        assertEquals("attachment; filename=\"$arm\"", file.headers["content-disposition"])
        assertEquals(payload.size.toString(), file.headers["content-length"])
        assertArrayEquals(payload, file.body)
        assertEquals(1, server!!.downloadCount)
        synchronized(counts) { assertEquals(listOf(1), counts) }
    }

    @Test fun errorsForEverythingElse() {
        val port = start()
        assertEquals(404, get(port, "/installer/README.txt").status)
        assertEquals(404, get(port, "/installer/../$x64").status)
        assertEquals(404, get(port, "/nope").status)
        val post = get(port, "/", "POST")
        assertEquals(405, post.status)
        assertEquals("GET", post.headers["allow"])
        assertEquals(400, send(port, "garbage\r\n\r\n".toByteArray()).status)
        val huge = "GET / HTTP/1.1\r\nX: " + "a".repeat(20_000) + "\r\n\r\n"
        assertEquals(431, send(port, huge.toByteArray()).status)
        assertEquals(0, server!!.downloadCount)
    }

    @Test fun stopClosesTheListener() {
        val port = start()
        server!!.stop()
        val refused = runCatching {
            Socket().use { it.connect(InetSocketAddress("127.0.0.1", port), 500) }
        }.isFailure
        assertTrue(refused)
    }
}
