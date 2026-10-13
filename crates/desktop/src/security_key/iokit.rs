//! The IOKit virtual HID device (`IOHIDUserDevice`), FIDO report
//! descriptor, 64-byte reports both ways.

use block2::RcBlock;
use core_foundation::{
    base::{CFType, TCFType},
    data::CFData,
    dictionary::CFDictionary,
    number::CFNumber,
    string::CFString,
};
use dispatch2::{DispatchQueue, DispatchRetained};
use std::ffi::c_void;

/// FIDO HID report descriptor (CTAP 2.2 §11.2.8.1): usage page 0xF1D0,
/// 64-byte input and output reports, no report IDs.
const DESCRIPTOR: [u8; 34] = [
    0x06, 0xD0, 0xF1, 0x09, 0x01, 0xA1, 0x01, // FIDO page, U2F device, app collection
    0x09, 0x20, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x75, 0x08, 0x95, 0x40, 0x81, 0x02, // input
    0x09, 0x21, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x75, 0x08, 0x95, 0x40, 0x91,
    0x02, // output
    0xC0,
];
/// pid.codes test VID; distinct PID from the IFD reader's.
const VENDOR_ID: i32 = 0x1209;
const PRODUCT_ID: i32 = 0x7A72;
const OUTPUT_REPORT: u32 = 1; // kIOHIDReportTypeOutput

type Ref = *mut c_void;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDUserDeviceCreateWithProperties(
        allocator: *const c_void,
        properties: *const c_void,
        options: u32,
    ) -> Ref;
    fn IOHIDUserDeviceRegisterSetReportBlock(device: Ref, block: *mut c_void);
    fn IOHIDUserDeviceSetDispatchQueue(device: Ref, queue: *mut c_void);
    fn IOHIDUserDeviceActivate(device: Ref);
    fn IOHIDUserDeviceHandleReportWithTimeStamp(
        device: Ref,
        timestamp: u64,
        report: *const u8,
        length: isize,
    ) -> i32;
}
unsafe extern "C" {
    fn mach_absolute_time() -> u64;
}

pub struct Device {
    raw: Ref,
    _queue: DispatchRetained<DispatchQueue>,
    _block: RcBlock<dyn Fn(u32, u32, *const u8, isize) -> i32>,
}

// SAFETY: IOHIDUserDevice is a CF object; handle-report calls are
// thread-safe per IOKit, and the set-report block runs on our queue.
unsafe impl Send for Device {}

impl Device {
    /// Creates and activates the device; `on_output` receives each
    /// output report (on a private dispatch queue). Err when macOS
    /// refuses (no entitlement). IOKit reports no open/close of the
    /// device, so `_on_release` is never called here.
    pub fn create(
        on_output: impl Fn(&[u8]) + 'static,
        _on_release: impl Fn() + 'static,
    ) -> Result<Self, String> {
        let number = |n: i32| CFNumber::from(n).as_CFType();
        let text = |s: &str| CFString::new(s).as_CFType();
        let properties: CFDictionary<CFString, CFType> = CFDictionary::from_CFType_pairs(&[
            (
                CFString::new("ReportDescriptor"),
                CFData::from_buffer(&DESCRIPTOR).as_CFType(),
            ),
            (CFString::new("VendorID"), number(VENDOR_ID)),
            (CFString::new("ProductID"), number(PRODUCT_ID)),
            (CFString::new("Transport"), text("USB")),
            (CFString::new("Product"), text("tapbridge security key")),
            (CFString::new("Manufacturer"), text("tapbridge")),
            (CFString::new("SerialNumber"), text("tapbridge-1")),
        ]);
        // SAFETY: valid CFDictionary; returns a +1 ref or null.
        let raw = unsafe {
            IOHIDUserDeviceCreateWithProperties(
                std::ptr::null(),
                properties.as_concrete_TypeRef() as *const c_void,
                0,
            )
        };
        if raw.is_null() {
            return Err("macOS refused the virtual HID device (missing entitlement?)".into());
        }
        let block = RcBlock::new(
            move |kind: u32, _id: u32, report: *const u8, length: isize| -> i32 {
                if kind == OUTPUT_REPORT && !report.is_null() && length > 0 {
                    // SAFETY: IOKit passes a buffer of `length` bytes.
                    on_output(unsafe { std::slice::from_raw_parts(report, length as usize) });
                }
                0 // kIOReturnSuccess
            },
        );
        let queue = DispatchQueue::new("computer.ethernet.tapbridge.security-key", None);
        // SAFETY: registration before activation, as the header requires;
        // the block and queue outlive the device (owned by `Device`).
        unsafe {
            IOHIDUserDeviceRegisterSetReportBlock(raw, RcBlock::as_ptr(&block).cast());
            IOHIDUserDeviceSetDispatchQueue(
                raw,
                (&*queue as *const DispatchQueue).cast_mut().cast(),
            );
            IOHIDUserDeviceActivate(raw);
        }
        Ok(Self {
            raw,
            _queue: queue,
            _block: block,
        })
    }

    /// Delivers one input report to the host.
    pub fn send(&self, report: &[u8]) {
        // SAFETY: live device; the report buffer is valid for the call.
        unsafe {
            IOHIDUserDeviceHandleReportWithTimeStamp(
                self.raw,
                mach_absolute_time(),
                report.as_ptr(),
                report.len() as isize,
            );
        }
    }
}

impl super::Sink for Device {
    fn send(&self, report: &[u8]) {
        Device::send(self, report);
    }
}
