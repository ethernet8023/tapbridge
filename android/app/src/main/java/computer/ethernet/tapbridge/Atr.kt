package computer.ethernet.tapbridge

object Atr {
    fun fromHistorical(historical: ByteArray): ByteArray {
        require(historical.size <= 15) { "ISO-DEP historical bytes exceed ATR limit" }
        val body = byteArrayOf(0x3b, (0x80 or historical.size).toByte(), 0x80.toByte(), 1) + historical
        var check = 0
        for (index in 1 until body.size) check = check xor (body[index].toInt() and 255)
        return body + check.toByte()
    }
}
