/*
 * tapbridge IFDHandler v3-ABI shim (pcscd / com.apple.ifdreader plugin).
 *
 * Loaded from /usr/local/libexec/SmartCardServices/drivers/tapbridge.bundle
 * (Info.plist claims USB VID 0x1209 / PID 0x7A71, matching the Windows
 * usbip gadget). Forwards every reader call to the tapbridge daemon over
 * the Unix-domain socket /var/tmp/tapbridge-ifd.sock using the length-
 * prefixed protocol implemented in crates/host/src/ifd.rs.
 *
 * The reader is always "present": every IFDH* call succeeds as long as the
 * daemon is reachable; a card is present exactly when the daemon says so
 * (a live phone session with an NFC tag). vpcd's ifd-vpcd.c is the pattern.
 *
 * Apple's PCSC headers define the types used here (wintypes.h: DWORD is
 * uint32_t, RESPONSECODE int32_t; ifdhandler.h v3 symbols). No PCSC
 * headers are included: the shim is self-contained so it builds without
 * requiring the SDK framework headers in the search path.
 */
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

/* ---- PC/SC IFDHandler types (Apple wintypes.h / ifdhandler.h) ---- */
typedef uint32_t DWORD;
typedef uint8_t UCHAR;
typedef unsigned char *PUCHAR;
typedef DWORD *PDWORD;
typedef int32_t RESPONSECODE;
typedef char *LPSTR;

typedef struct _SCARD_IO_HEADER {
    DWORD Protocol;
    DWORD Length;
} SCARD_IO_HEADER, *PSCARD_IO_HEADER;

#define IFD_SUCCESS 0L
#define IFD_COMMUNICATION_ERROR 612L
#define IFD_RESPONSE_TIMEOUT 613L
#define IFD_NOT_SUPPORTED 614L
#define IFD_ICC_PRESENT 615L
#define IFD_ICC_NOT_PRESENT 616L
#define IFD_NO_SUCH_DEVICE 617L
#define IFD_ERROR_INSUFFICIENT_BUFFER 618L
#define IFD_ERROR_TAG 600L
#define IFD_ERROR_POWER_ACTION 608L

#define IFD_POWER_UP 500L
#define IFD_POWER_DOWN 501L
#define IFD_RESET 502L

#define TAG_IFD_ATR 0x0303
#define TAG_IFD_SLOT_THREAD_SAFE 0x0FAC
#define TAG_IFD_THREAD_SAFE 0x0FAD
#define TAG_IFD_SLOTS_NUMBER 0x0FAE
#define TAG_IFD_SIMULTANEOUS_ACCESS 0x0FAF

#define MAX_ATR_SIZE 33

/* ---- tapbridge IFD socket protocol (crates/host/src/ifd.rs) ---- */
#define TB_SOCKET_PATH "/var/tmp/tapbridge-ifd.sock"
#define TB_CMD_PRESENCE 1
#define TB_CMD_ATR 2
#define TB_CMD_POWER_ON 3
#define TB_CMD_POWER_OFF 4
#define TB_CMD_TRANSMIT 5
#define TB_OK 0
#define TB_E_NO_CARD 1
#define TB_E_NO_SESSION 2
#define TB_E_BUSY 3
#define TB_E_COMM 4
#define TB_E_BAD 5
#define TB_MAX_BODY (4 + 65536)

/* Writes a full buffer; returns 0 on success. */
static int write_all(int fd, const unsigned char *buf, size_t len)
{
    while (len > 0) {
        ssize_t n = write(fd, buf, len);
        if (n < 0) {
            if (errno == EINTR)
                continue;
            return -1;
        }
        buf += n;
        len -= (size_t)n;
    }
    return 0;
}

/* Reads a full buffer; returns 0 on success. */
static int read_all(int fd, unsigned char *buf, size_t len)
{
    while (len > 0) {
        ssize_t n = read(fd, buf, len);
        if (n < 0) {
            if (errno == EINTR)
                continue;
            return -1;
        }
        if (n == 0)
            return -1;
        buf += n;
        len -= (size_t)n;
    }
    return 0;
}

/* Connects to the daemon socket; -1 on failure. */
static int tb_connect(void)
{
    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    struct sockaddr_un addr;
    if (fd < 0)
        return -1;
    memset(&addr, 0, sizeof(addr));
    addr.sun_family = AF_UNIX;
    strncpy(addr.sun_path, TB_SOCKET_PATH, sizeof(addr.sun_path) - 1);
    if (connect(fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        close(fd);
        return -1;
    }
    return fd;
}

/*
 * One request/response round trip: send `[len u32 BE][cmd body]`, receive
 * `[len u32 BE][status byte][data]`. Writes data into `out` (cap
 * `out_cap`), sets *out_len. Returns 0 on success; the protocol status
 * byte is returned through *status.
 */
static int tb_roundtrip(unsigned char cmd, const unsigned char *payload,
                        uint32_t payload_len, unsigned char *out,
                        uint32_t out_cap, uint32_t *out_len, unsigned char *status)
{
    int fd;
    uint32_t body_len, frame_len, i;
    unsigned char frame[4 + 1 + TB_MAX_BODY];
    unsigned char lenbuf[4];

    if (payload_len + 1 > TB_MAX_BODY)
        return -1;
    fd = tb_connect();
    if (fd < 0)
        return -1;

    body_len = payload_len + 1;
    frame_len = 4 + body_len;
    lenbuf[0] = (unsigned char)(body_len >> 24);
    lenbuf[1] = (unsigned char)(body_len >> 16);
    lenbuf[2] = (unsigned char)(body_len >> 8);
    lenbuf[3] = (unsigned char)(body_len);
    memcpy(frame, lenbuf, 4);
    frame[4] = cmd;
    if (payload_len)
        memcpy(frame + 5, payload, payload_len);

    if (write_all(fd, frame, frame_len) < 0) {
        close(fd);
        return -1;
    }
    if (read_all(fd, lenbuf, 4) < 0) {
        close(fd);
        return -1;
    }
    body_len = ((uint32_t)lenbuf[0] << 24) | ((uint32_t)lenbuf[1] << 16) |
               ((uint32_t)lenbuf[2] << 8) | (uint32_t)lenbuf[3];
    if (body_len < 1 || body_len > TB_MAX_BODY) {
        close(fd);
        return -1;
    }
    unsigned char *body = malloc(body_len);
    if (!body) {
        close(fd);
        return -1;
    }
    if (read_all(fd, body, body_len) < 0) {
        free(body);
        close(fd);
        return -1;
    }
    close(fd);
    *status = body[0];
    *out_len = 0;
    for (i = 1; i < body_len && *out_len < out_cap; i++)
        out[(*out_len)++] = body[i];
    free(body);
    return 0;
}

/* Simple syslog-free logging hook is unnecessary; failures are silent. */

/* ---- IFDHandler v3 entry points ---- */

RESPONSECODE IFDHCreateChannel(DWORD Lun, DWORD Channel)
{
    (void)Lun;
    (void)Channel;
    return IFD_SUCCESS;
}

RESPONSECODE IFDHCreateChannelByName(DWORD Lun, LPSTR DeviceName)
{
    (void)Lun;
    (void)DeviceName;
    return IFD_SUCCESS;
}

RESPONSECODE IFDHCloseChannel(DWORD Lun)
{
    (void)Lun;
    return IFD_SUCCESS;
}

RESPONSECODE IFDHGetCapabilities(DWORD Lun, DWORD Tag, PDWORD Length, PUCHAR Value)
{
    (void)Lun;
    if (!Length || !Value)
        return IFD_COMMUNICATION_ERROR;
    switch (Tag) {
    case TAG_IFD_ATR: {
        unsigned char atr[MAX_ATR_SIZE];
        uint32_t atr_len = 0;
        unsigned char status;
        if (tb_roundtrip(TB_CMD_ATR, NULL, 0, atr, sizeof(atr), &atr_len, &status) < 0)
            return IFD_COMMUNICATION_ERROR;
        if (status != TB_OK || atr_len == 0)
            return IFD_ICC_NOT_PRESENT;
        /* Apple's pcscd passes the buffer size unreliably; cap to MAX_ATR_SIZE
         * like vpcd does on __APPLE__. */
        if (atr_len > *Length && atr_len > MAX_ATR_SIZE)
            return IFD_ERROR_INSUFFICIENT_BUFFER;
        memcpy(Value, atr, atr_len);
        *Length = atr_len;
        return IFD_SUCCESS;
    }
    case TAG_IFD_SLOT_THREAD_SAFE:
        if (*Length < 1)
            return IFD_ERROR_INSUFFICIENT_BUFFER;
        Value[0] = 0; /* one slot, serial access; not thread safe */
        *Length = 1;
        return IFD_SUCCESS;
    case TAG_IFD_THREAD_SAFE:
        if (*Length < 1)
            return IFD_ERROR_INSUFFICIENT_BUFFER;
        Value[0] = 0;
        *Length = 1;
        return IFD_SUCCESS;
    case TAG_IFD_SLOTS_NUMBER:
        if (*Length < 1)
            return IFD_ERROR_INSUFFICIENT_BUFFER;
        Value[0] = 1;
        *Length = 1;
        return IFD_SUCCESS;
    case TAG_IFD_SIMULTANEOUS_ACCESS:
        if (*Length < 1)
            return IFD_ERROR_INSUFFICIENT_BUFFER;
        Value[0] = 1;
        *Length = 1;
        return IFD_SUCCESS;
    default:
        *Length = 0;
        return IFD_ERROR_TAG;
    }
}

RESPONSECODE IFDHSetCapabilities(DWORD Lun, DWORD Tag, DWORD Length, PUCHAR Value)
{
    (void)Lun;
    (void)Tag;
    (void)Length;
    (void)Value;
    return IFD_NOT_SUPPORTED;
}

RESPONSECODE IFDHSetProtocolParameters(DWORD Lun, DWORD Protocol, UCHAR Flags,
                                       UCHAR PTS1, UCHAR PTS2, UCHAR PTS3)
{
    (void)Lun;
    (void)Protocol;
    (void)Flags;
    (void)PTS1;
    (void)PTS2;
    (void)PTS3;
    /* The phone card answers T=1; PTS negotiation is not needed. */
    return IFD_SUCCESS;
}

RESPONSECODE IFDHPowerICC(DWORD Lun, DWORD Action, PUCHAR Atr, PDWORD AtrLength)
{
    unsigned char atr[MAX_ATR_SIZE];
    uint32_t atr_len = 0, out_len = 0;
    unsigned char status;
    int rc;

    (void)Lun;
    if (!Atr || !AtrLength)
        return IFD_COMMUNICATION_ERROR;
    switch (Action) {
    case IFD_POWER_UP:
        rc = tb_roundtrip(TB_CMD_POWER_ON, NULL, 0, atr, sizeof(atr), &atr_len, &status);
        break;
    case IFD_RESET:
        /* A cold reset on the phone path is a fresh power on. */
        rc = tb_roundtrip(TB_CMD_POWER_ON, NULL, 0, atr, sizeof(atr), &atr_len, &status);
        break;
    case IFD_POWER_DOWN:
        rc = tb_roundtrip(TB_CMD_POWER_OFF, NULL, 0, NULL, 0, &out_len, &status);
        *AtrLength = 0;
        if (rc < 0)
            return IFD_COMMUNICATION_ERROR;
        /* POWER_OFF replies OK even with no card (slot deactivation). */
        return IFD_SUCCESS;
    default:
        *AtrLength = 0;
        return IFD_NOT_SUPPORTED;
    }
    if (rc < 0) {
        *AtrLength = 0;
        return IFD_COMMUNICATION_ERROR;
    }
    if (status == TB_E_NO_CARD || status == TB_E_NO_SESSION) {
        /* No card in the reader: power up fails per PC/SC, but the wake
         * request has already fired on the daemon side. */
        *AtrLength = 0;
        return IFD_ERROR_POWER_ACTION;
    }
    if (status != TB_OK) {
        *AtrLength = 0;
        return IFD_COMMUNICATION_ERROR;
    }
    /* Apple's ifdreader passes *AtrLength unreliably (often 0), so trusting
     * it truncated the ATR to nothing and macOS logged "failed to reset the
     * card". Like vpcd on __APPLE__: the buffer is always MAX_ATR_SIZE. */
    if (atr_len > MAX_ATR_SIZE)
        atr_len = MAX_ATR_SIZE;
    memcpy(Atr, atr, atr_len);
    *AtrLength = atr_len;
    return IFD_SUCCESS;
}

RESPONSECODE IFDHTransmitToICC(DWORD Lun, SCARD_IO_HEADER SendPci, PUCHAR TxBuffer,
                               DWORD TxLength, PUCHAR RxBuffer, PDWORD RxLength,
                               PSCARD_IO_HEADER RecvPci)
{
    unsigned char status;
    uint32_t rx_len = 0;
    int rc;

    (void)Lun;
    if (!RxLength || !RecvPci || !TxBuffer)
        return IFD_COMMUNICATION_ERROR;
    (void)SendPci;
    if (TxLength > TB_MAX_BODY)
        return IFD_COMMUNICATION_ERROR;
    rc = tb_roundtrip(TB_CMD_TRANSMIT, TxBuffer, TxLength, RxBuffer, *RxLength, &rx_len, &status);
    if (rc < 0) {
        *RxLength = 0;
        return IFD_COMMUNICATION_ERROR;
    }
    if (status != TB_OK) {
        *RxLength = 0;
        if (status == TB_E_NO_CARD)
            return IFD_ICC_NOT_PRESENT;
        if (status == TB_E_BUSY)
            return IFD_COMMUNICATION_ERROR;
        return IFD_COMMUNICATION_ERROR;
    }
    *RxLength = rx_len;
    RecvPci->Protocol = 1; /* T=1 */
    return IFD_SUCCESS;
}

RESPONSECODE IFDHICCPresence(DWORD Lun)
{
    unsigned char status, data[1];
    uint32_t data_len = 0;
    (void)Lun;
    if (tb_roundtrip(TB_CMD_PRESENCE, NULL, 0, data, sizeof(data), &data_len, &status) < 0)
        return IFD_COMMUNICATION_ERROR;
    if (status == TB_OK)
        return IFD_ICC_PRESENT;
    return IFD_ICC_NOT_PRESENT;
}

/*
 * macOS pcscd (com.apple.ifdreader) calls IFDHControl with the v3
 * signature. We only need CM_IOCTL_GET_FEATURE_REQUEST-like handling for
 * the drivers pcscd probes; report no extra features.
 */
RESPONSECODE IFDHControl(DWORD Lun, DWORD dwControlCode, PUCHAR TxBuffer,
                         DWORD TxLength, PUCHAR RxBuffer, DWORD RxLength,
                         PDWORD pdwBytesReturned)
{
    (void)Lun;
    (void)dwControlCode;
    (void)TxBuffer;
    (void)TxLength;
    (void)RxBuffer;
    (void)RxLength;
    if (!pdwBytesReturned)
        return IFD_COMMUNICATION_ERROR;
    *pdwBytesReturned = 0;
    return IFD_NOT_SUPPORTED;
}
