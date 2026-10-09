// Spike B: same virtual FIDO HID device via the older IOKit API
// (IOHIDUserDeviceCreateWithProperties), to see whether it needs the
// CoreHID entitlement too. Prints whether creation succeeded.
// Build: clang -framework IOKit -framework CoreFoundation -o iokit-spike iokit.c
#include <CoreFoundation/CoreFoundation.h>
#include <IOKit/hid/IOHIDKeys.h>
#include <IOKit/hidsystem/IOHIDUserDevice.h>
#include <stdio.h>

static const uint8_t descriptor[] = {
    0x06, 0xD0, 0xF1, 0x09, 0x01, 0xA1, 0x01,
    0x09, 0x20, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x75, 0x08, 0x95, 0x40, 0x81, 0x02,
    0x09, 0x21, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x75, 0x08, 0x95, 0x40, 0x91, 0x02,
    0xC0,
};

static void put_int(CFMutableDictionaryRef d, CFStringRef k, int v) {
    CFNumberRef n = CFNumberCreate(NULL, kCFNumberIntType, &v);
    CFDictionarySetValue(d, k, n);
    CFRelease(n);
}

int main(void) {
    CFMutableDictionaryRef props = CFDictionaryCreateMutable(NULL, 0, &kCFTypeDictionaryKeyCallBacks,
                                                             &kCFTypeDictionaryValueCallBacks);
    CFDataRef desc = CFDataCreate(NULL, descriptor, sizeof descriptor);
    CFDictionarySetValue(props, CFSTR(kIOHIDReportDescriptorKey), desc);
    put_int(props, CFSTR(kIOHIDVendorIDKey), 0x1209);
    put_int(props, CFSTR(kIOHIDProductIDKey), 0x7A72);
    CFDictionarySetValue(props, CFSTR(kIOHIDProductKey), CFSTR("tapbridge security key (iokit spike)"));
    CFDictionarySetValue(props, CFSTR(kIOHIDTransportKey), CFSTR("USB"));
    IOHIDUserDeviceRef dev = IOHIDUserDeviceCreateWithProperties(kCFAllocatorDefault, props, 0);
    printf("IOHIDUserDeviceCreateWithProperties -> %s\n", dev ? "CREATED" : "NULL (refused)");
    if (dev) { sleep(3); CFRelease(dev); }
    return dev ? 0 : 1;
}
