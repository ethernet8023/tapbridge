package computer.ethernet.tapbridge

/**
 * Paired PCs (DESIGN "Pairing and persistence", many-hosts-per-phone).
 *
 * A phone may be paired with many hosts at once; each host is an independent
 * peer with its own identity key and pairing capability, and each host still
 * pins exactly one phone. A record is:
 *
 *   host_id -> { name, cached_hint, paired_at, renamed, platform, piv_login }
 *
 * `name` is the host display name from the `Accepted` handshake (or a
 * user-chosen one; `renamed` freezes a user edit so a later `Accepted` cannot
 * overwrite it). `cached_hint` is the last wake/dial hint for that host.
 * `platform` (NativeBridge.PLATFORM_*) and `piv_login` (the PC can pair a
 * card with its OS login) come from the latest `Accepted`.
 *
 * Storage format: strict JSON in ONE SharedPreferences key (see
 * [IdentityStore]). The codec and every merge/routing rule live here as pure
 * functions with no Android dependencies, so they are unit-testable on the
 * JVM. Decode is strict, mirroring wire.rs: unknown fields, trailing bytes,
 * overlength or invalid values throw — corrupt state fails loudly and never
 * silently re-keys. A store from another format version is not migrated:
 * it reads as nothing paired.
 */
object PairedHosts {
    private const val VERSION = 2L
    private const val MAX_NAME_BYTES = 64
    private const val MAX_HINT_BYTES = 1024

    data class Record(
        /** Iroh EndpointId, 64 hex characters. */
        val hostId: String,
        /** Display name from Accepted, or the user's rename. 1..=64 UTF-8 bytes. */
        val name: String,
        /** Last known wake hint for this host (HOSTID[;r=..][;d=..]), or null. */
        val cachedHint: String?,
        /** Epoch millis of the first successful pairing with this host. */
        val pairedAt: Long,
        /** True once the user renamed the host; Accepted no longer overwrites. */
        val renamed: Boolean = false,
        /** The PC's OS (NativeBridge.PLATFORM_*), for wording. */
        val platform: Int = NativeBridge.PLATFORM_OTHER,
        /** The PC can pair a card's PIV identity with its OS login. */
        val pivLogin: Boolean = false,
    ) {
        /** "Mac", "Windows" or "PC", for "unlocks your …". */
        val osName get() = osName(platform)
    }

    fun osName(platform: Int) = when (platform) {
        NativeBridge.PLATFORM_MACOS -> "Mac"
        NativeBridge.PLATFORM_WINDOWS -> "Windows PC"
        else -> "PC"
    }

    /** A wake the phone should act on: which host, and which hint to dial. */
    data class Wake(val hostId: String, val hint: String)

    // ---------------------------------------------------------------- codec

    fun encode(records: List<Record>): String = buildString {
        append("{\"v\":").append(VERSION).append(",\"hosts\":{")
        records.forEachIndexed { index, record ->
            if (index > 0) append(',')
            append(jsonString(record.hostId))
            append(":{\"name\":").append(jsonString(record.name))
            append(",\"hint\":")
            append(record.cachedHint?.let { jsonString(it) } ?: "null")
            append(",\"at\":").append(record.pairedAt)
            append(",\"ren\":").append(record.renamed)
            append(",\"os\":").append(record.platform)
            append(",\"login\":").append(record.pivLogin)
            append('}')
        }
        append("}}")
    }

    fun decode(text: String): List<Record> {
        val value = try {
            JsonParser(text).parse()
        } catch (error: IllegalArgumentException) {
            throw IllegalStateException("corrupt paired-hosts store: ${error.message}", error)
        }
        fun fail(why: String): Nothing =
            throw IllegalStateException("corrupt paired-hosts store: $why")

        val root = value as? Map<*, *> ?: fail("root is not an object")
        if (root.keys != setOf("v", "hosts")) fail("unexpected top-level fields ${root.keys}")
        if (root["v"] != VERSION) return emptyList()
        val hosts = root["hosts"] as? Map<*, *> ?: fail("hosts is not an object")
        if (hosts.isEmpty()) return emptyList()
        val records = ArrayList<Record>(hosts.size)
        for ((key, raw) in hosts) {
            val hostId = key as? String ?: fail("host key is not a string")
            if (!validHostId(hostId)) fail("host id is not 64 hex characters")
            val fields = raw as? Map<*, *> ?: fail("record for $hostId is not an object")
            if (fields.keys != setOf("name", "hint", "at", "ren", "os", "login")) {
                fail("unexpected fields ${fields.keys} for $hostId")
            }
            val name = fields["name"] as? String ?: fail("name is not a string")
            if (!validName(name)) fail("name must be 1..=$MAX_NAME_BYTES UTF-8 bytes")
            val hint = when (val h = fields["hint"]) {
                null -> null
                is String ->
                    if (validHint(h)) h else fail("hint must be 1..=$MAX_HINT_BYTES UTF-8 bytes")
                else -> fail("hint is neither a string nor null")
            }
            val at = fields["at"] as? Long ?: fail("at is not an integer")
            val renamed = fields["ren"] as? Boolean ?: fail("ren is not a boolean")
            val platform = (fields["os"] as? Long)?.takeIf { it in 0..255 }?.toInt() ?: fail("os is not a small integer")
            val login = fields["login"] as? Boolean ?: fail("login is not a boolean")
            records.add(Record(hostId, name, hint, at, renamed, platform, login))
        }
        return records
    }

    // ---------------------------------------------------------------- routing

    /**
     * FCM wake routing. `{"t":"wake","host":ID,"hint":HINT}` acts only when
     * ID names a paired host; anything else is dropped (as mismatches always
     * were). A missing/blank hint falls back to the bare host id, which is a
     * valid hint.
     */
    fun routeForWake(records: List<Record>, host: String?, hint: String?): Wake? {
        if (host == null) return null
        val record = records.firstOrNull { it.hostId == host } ?: return null
        val dial = if (hint.isNullOrBlank()) record.hostId else hint
        return Wake(record.hostId, dial)
    }

    // ------------------------------------------------------------ pure merges

    /** Upsert after a successful connect/Accepted: keeps pairedAt and renames. */
    fun upsertAfterAccept(
        records: List<Record>,
        hostId: String,
        name: String,
        now: Long,
        platform: Int = NativeBridge.PLATFORM_OTHER,
        pivLogin: Boolean = false,
    ): List<Record> {
        require(validHostId(hostId)) { "host id is not 64 hex characters" }
        require(validName(name)) { "host name must be 1..=$MAX_NAME_BYTES UTF-8 bytes" }
        val existing = records.firstOrNull { it.hostId == hostId }
        val updated = when {
            existing == null -> Record(hostId, name, null, now, renamed = false)
            existing.renamed -> existing // user's name wins
            else -> existing.copy(name = name)
        }.copy(platform = platform, pivLogin = pivLogin)
        return records.filterNot { it.hostId == hostId } + updated
    }

    /** Rename one host; marks it renamed so Accepted cannot overwrite it. */
    fun withRenamed(records: List<Record>, hostId: String, name: String): List<Record> {
        require(validName(name)) { "host name must be 1..=$MAX_NAME_BYTES UTF-8 bytes" }
        require(records.any { it.hostId == hostId }) { "unknown host" }
        return records.map { if (it.hostId == hostId) it.copy(name = name, renamed = true) else it }
    }

    /** Drop one host ("forget this PC"). */
    fun withoutHost(records: List<Record>, hostId: String): List<Record> =
        records.filterNot { it.hostId == hostId }

    /**
     * The last-used host after forgetting `forgotten`: unchanged when a
     * different host was last used, otherwise the most recently paired
     * remaining host (null when none is left).
     */
    fun fallbackLastUsed(records: List<Record>, forgotten: String, currentLast: String?): String? =
        if (currentLast != null && currentLast != forgotten) currentLast
        else records.filterNot { it.hostId == forgotten }
            .maxByOrNull { it.pairedAt }?.hostId

    // ----------------------------------------------------------- validation

    fun validHostId(id: String): Boolean =
        id.length == 64 && id.all { it in '0'..'9' || it in 'a'..'f' || it in 'A'..'F' }

    fun validName(name: String): Boolean =
        name.isNotEmpty() && name.toByteArray(Charsets.UTF_8).size <= MAX_NAME_BYTES

    fun validHint(hint: String): Boolean {
        if (hint.isBlank()) return false
        return hint.toByteArray(Charsets.UTF_8).size <= MAX_HINT_BYTES
    }

    // ------------------------------------------------------- minimal strict JSON

    private fun jsonString(s: String): String = buildString {
        append('"')
        for (ch in s) when (ch) {
            '"' -> append("\\\"")
            '\\' -> append("\\\\")
            '\n' -> append("\\n")
            '\r' -> append("\\r")
            '\t' -> append("\\t")
            '\b' -> append("\\b")
            '\u000C' -> append("\\f")
            else -> if (ch < ' ') append("\\u").append("%04x".format(ch.code)) else append(ch)
        }
        append('"')
    }

    /**
     * Minimal strict JSON parser for the store format: objects, arrays,
     * strings, integers and true/false/null. Rejects trailing bytes, empty
     * input, fractional/exponent numbers and malformed escapes — the same
     * strictness as wire.rs. Returns LinkedHashMap (order-preserving) /
     * ArrayList / String / Long / Boolean / null.
     */
    private class JsonParser(private val text: String) {
        private var i = 0

        fun parse(): Any? {
            val value = value()
            whitespace()
            require(i == text.length) { "trailing bytes" }
            return value
        }

        private fun value(): Any? {
            whitespace()
            require(i < text.length) { "unexpected end of input" }
            return when (text[i]) {
                '{' -> obj()
                '[' -> array()
                '"' -> str()
                't' -> literal("true", true)
                'f' -> literal("false", false)
                'n' -> literal("null", null)
                else -> number()
            }
        }

        private fun obj(): Any? {
            i++ // '{'
            val map = LinkedHashMap<String, Any?>()
            whitespace()
            if (i < text.length && text[i] == '}') { i++; return map }
            while (true) {
                whitespace()
                require(i < text.length && text[i] == '"') { "expected object key" }
                val key = str() as String
                whitespace()
                require(i < text.length && text[i] == ':') { "expected ':'" }
                i++
                map[key] = value()
                whitespace()
                require(i < text.length) { "unexpected end of object" }
                when (text[i]) {
                    ',' -> i++
                    '}' -> { i++; return map }
                    else -> require(false) { "expected ',' or '}'" }
                }
            }
        }

        private fun array(): Any? {
            i++ // '['
            val list = ArrayList<Any?>()
            whitespace()
            if (i < text.length && text[i] == ']') { i++; return list }
            while (true) {
                list.add(value())
                whitespace()
                require(i < text.length) { "unexpected end of array" }
                when (text[i]) {
                    ',' -> i++
                    ']' -> { i++; return list }
                    else -> require(false) { "expected ',' or ']'" }
                }
            }
        }

        private fun str(): Any? {
            require(i < text.length && text[i] == '"') { "expected string" }
            i++
            val sb = StringBuilder()
            while (true) {
                require(i < text.length) { "unterminated string" }
                when (val ch = text[i]) {
                    '"' -> { i++; return sb.toString() }
                    '\\' -> {
                        i++
                        require(i < text.length) { "unterminated escape" }
                        when (val esc = text[i]) {
                            '"' -> sb.append('"')
                            '\\' -> sb.append('\\')
                            '/' -> sb.append('/')
                            'b' -> sb.append('\b')
                            'f' -> sb.append('\u000C')
                            'n' -> sb.append('\n')
                            'r' -> sb.append('\r')
                            't' -> sb.append('\t')
                            'u' -> {
                                require(i + 4 < text.length) { "truncated \\u escape" }
                                val hex = text.substring(i + 1, i + 5)
                                sb.append(hex.toIntOrNull(16)?.toChar() ?: error("invalid \\u escape"))
                                i += 4
                            }
                            else -> error("invalid escape \\$esc")
                        }
                        i++
                    }
                    else -> {
                        require(ch >= ' ') { "raw control character in string" }
                        sb.append(ch)
                        i++
                    }
                }
            }
        }

        private fun number(): Any {
            val start = i
            if (i < text.length && text[i] == '-') i++
            require(i < text.length && text[i].isDigit()) { "invalid number" }
            while (i < text.length && text[i].isDigit()) i++
            require(i >= text.length || (text[i] != '.' && text[i] != 'e' && text[i] != 'E')) {
                "fractional/exponent numbers are not allowed"
            }
            return text.substring(start, i).toLongOrNull() ?: error("integer out of range")
        }

        private fun literal(word: String, value: Any?): Any? {
            require(text.regionMatches(i, word, 0, word.length)) { "invalid literal" }
            i += word.length
            return value
        }

        private fun whitespace() {
            while (i < text.length && text[i].let { it == ' ' || it == '\t' || it == '\n' || it == '\r' }) i++
        }
    }
}
