package computer.ethernet.tapbridge

/**
 * JNI bridge. All methods are implemented by the Rust `tapbridge-phone`
 * cdylib (libtapbridge_phone.so). Pointer/handle safety is enforced on the
 * Kotlin side with NativeGate's ReentrantReadWriteLock: every native call
 * takes the read lock, release() takes the write lock and must wait until no
 * calls are in flight.
 *
 * Wire contract v4: connect dials a wake hint (HOSTID[;r=..][;d=..]) and sends
 * Hello { name, push } first; the host replies Accepted { name, platform, piv_login }.
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

    /** Host OS from the Accepted reply ([PLATFORM_MACOS] etc.; -1 on error). For wording only. */
    @JvmStatic external fun hostPlatform(handle: Long): Int

    /** The host can pair a card's PIV identity with its OS login (Accepted.piv_login). */
    @JvmStatic external fun hostPivLogin(handle: Long): Boolean

    /** Hint text actually used for the dial (for caching reconnect addresses). */
    @JvmStatic external fun usedHint(handle: Long): String?

    /**
     * Blocking read of ONE host frame. Invokes callbacks on the calling
     * thread. Returns true when a frame was dispatched, false on EOF; throws
     * java.io.IOException on error.
     */
    @JvmStatic external fun read(handle: Long, callbacks: Any): Boolean

    /**
     * Report a newly discovered card to the host. [uid] is the tag's stable
     * NFC UID (Cards.wireUid), empty when absent or random.
     */
    @JvmStatic external fun card(handle: Long, generation: Long, atr: ByteArray, maxApdu: Int, uid: ByteArray)

    /** Send one synced card record ([CardInfo]) to the host. */
    @JvmStatic external fun cardInfo(
        handle: Long, uid: ByteArray, label: String, labelAt: Long,
        wake: Boolean, wakeAt: Long, caps: Int, capsAt: Long,
        login: Boolean, loginAt: Long, deletedAt: Long,
    )

    /**
     * Ask the host to do [action] for a card ([done] false), or report how
     * an action the host asked for ended ([done] true, [ok]).
     */
    @JvmStatic external fun cardAction(handle: Long, uid: ByteArray, action: Int, done: Boolean, ok: Boolean)

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
        /** The PC's copy of a card record (merge it; reply if ours is newer). */
        fun cardInfo(
            uid: ByteArray, label: String, labelAt: Long,
            wake: Boolean, wakeAt: Long, caps: Int, capsAt: Long,
            login: Boolean, loginAt: Long, deletedAt: Long,
        )
        /** The PC asks the phone to do [action] for a card. */
        fun cardAction(uid: ByteArray, action: Int)
        /** An action the phone asked the PC for ended. */
        fun cardActionDone(uid: ByteArray, action: Int, ok: Boolean)
    }

    /** wire::platform */
    const val PLATFORM_OTHER = 0
    const val PLATFORM_MACOS = 1
    const val PLATFORM_WINDOWS = 2

    /** wire::action */
    const val ACTION_SETUP_CERTS = 1
    const val ACTION_SETUP_LOGIN = 2
}
