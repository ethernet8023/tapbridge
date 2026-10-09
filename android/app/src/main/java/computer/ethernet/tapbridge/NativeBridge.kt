package computer.ethernet.tapbridge

/**
 * JNI bridge. All methods are implemented by the Rust `tapbridge-phone`
 * cdylib (libtapbridge_phone.so). Pointer/handle safety is enforced on the
 * Kotlin side with NativeGate's ReentrantReadWriteLock: every native call
 * takes the read lock, release() takes the write lock and must wait until no
 * calls are in flight.
 *
 * Wire contract v2: connect dials a wake hint (HOSTID[;r=..][;d=..]) and sends
 * Hello { name, push } first; the host replies Accepted { name }.
 */
object NativeBridge {
    init {
        System.loadLibrary("tapbridge_phone")
    }

    @JvmStatic external fun initialize(context: android.content.Context)

    /** Returns a fresh 32-byte identity key. */
    @JvmStatic external fun generateKey(): ByteArray

    /**
     * Blocking connect. [hint] is a wake hint text: HOSTID[;r=RELAY][;d=IP:PORT]*.
     * A bare host id is a valid hint. [invitation] may be empty to reconnect to
     * the previously paired host; [pushToken] may be empty for none. Returns a
     * native session handle (> 0) only after the host has accepted the
     * connection.
     */
    @JvmStatic external fun connect(
        key: ByteArray,
        hint: String,
        invitation: String,
        phoneName: String,
        pushToken: String,
    ): Long

    /** Host display name from the Accepted reply (null on error). */
    @JvmStatic external fun hostName(handle: Long): String?

    /** Hint text actually used for the dial (for caching reconnect addresses). */
    @JvmStatic external fun usedHint(handle: Long): String?

    /**
     * Blocking read of ONE host frame. Invokes callbacks on the calling
     * thread. Returns true when a frame was dispatched, false on EOF; throws
     * java.io.IOException on error.
     */
    @JvmStatic external fun read(handle: Long, callbacks: Any): Boolean

    /** Report a newly discovered card to the host. */
    @JvmStatic external fun card(handle: Long, generation: Long, atr: ByteArray, maxApdu: Int)

    /** Send an APDU response to the host. */
    @JvmStatic external fun response(handle: Long, id: Long, generation: Long, data: ByteArray)

    /** Tell the host an APDU failed / card disappeared. */
    @JvmStatic external fun failed(handle: Long, id: Long, generation: Long)

    /** Interrupt the blocking read (and any in-flight network work). */
    @JvmStatic external fun close(handle: Long)

    /** Free the native handle. Only after read/writer calls have finished. */
    @JvmStatic external fun release(handle: Long)

    /** Methods the native code invokes on the callbacks object passed to read(). */
    interface Callbacks {
        fun request(id: Long, generation: Long, data: ByteArray)
        fun cancel(id: Long)
        fun readCard()
        /** Host's sign-in state (wire `auth` constants: 0 idle, 1 working,
         *  2 success, 3 card removed mid-auth, 4 PIN entry, 5 failed). */
        fun authState(state: Int)
    }
}
