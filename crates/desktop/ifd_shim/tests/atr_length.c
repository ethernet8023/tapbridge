/* Calls the shim like Apple's ifdreader: IFDHPowerICC with *AtrLength = 0
 * (it passes the buffer size unreliably). The ATR must still come back in
 * full. Needs the tapbridge daemon running with a card on the phone.
 * Build: clang -o atr_length tests/atr_length.c <dylib>; run: ./atr_length */
#include <stdio.h>
#include <stdint.h>
typedef uint32_t DWORD, *PDWORD;
typedef unsigned char UCHAR, *PUCHAR;
typedef int32_t RESPONSECODE;
RESPONSECODE IFDHPowerICC(DWORD Lun, DWORD Action, PUCHAR Atr, PDWORD AtrLength);
int main(void) {
    unsigned char atr[33];
    DWORD len = 0; /* what ifdreader hands us */
    RESPONSECODE rc = IFDHPowerICC(0, 500 /* IFD_POWER_UP */, atr, &len);
    printf("rc=%d atr_len=%u atr=", rc, len);
    for (DWORD i = 0; i < len; i++) printf("%02X", atr[i]);
    printf("\n");
    return rc == 0 && len >= 2 ? 0 : 1;
}
