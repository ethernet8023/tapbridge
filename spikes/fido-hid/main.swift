// Spike: can an app create a virtual FIDO (CTAPHID) device with CoreHID, and
// do browsers talk to it? Answers CTAPHID INIT + CBOR GetInfo with a fixed
// reply and logs every packet; anything else gets ERR_INVALID_CMD.
//
// Build: swiftc -O -o fido-hid-spike main.swift
// Run:   ./fido-hid-spike     (then open a WebAuthn demo in a browser)
import CoreHID
import Foundation

// FIDO HID report descriptor (FIDO CTAP §11.2.8.1): usage page 0xF1D0,
// 64-byte input and output reports, no report IDs.
let descriptor = Data([
    0x06, 0xD0, 0xF1,  // Usage Page (FIDO Alliance)
    0x09, 0x01,        // Usage (U2F Authenticator Device)
    0xA1, 0x01,        // Collection (Application)
    0x09, 0x20,        //   Usage (Input Report Data)
    0x15, 0x00,        //   Logical Minimum (0)
    0x26, 0xFF, 0x00,  //   Logical Maximum (255)
    0x75, 0x08,        //   Report Size (8)
    0x95, 0x40,        //   Report Count (64)
    0x81, 0x02,        //   Input (Data, Var, Abs)
    0x09, 0x21,        //   Usage (Output Report Data)
    0x15, 0x00,
    0x26, 0xFF, 0x00,
    0x75, 0x08,
    0x95, 0x40,
    0x91, 0x02,        //   Output (Data, Var, Abs)
    0xC0,              // End Collection
])

func hex(_ data: Data) -> String { data.map { String(format: "%02x", $0) }.joined() }
func log(_ s: String) {
    let t = ISO8601DateFormatter().string(from: Date())
    FileHandle.standardError.write("\(t) \(s)\n".data(using: .utf8)!)
}

// CTAPHID constants.
let CMD_INIT: UInt8 = 0x86, CMD_CBOR: UInt8 = 0x90, CMD_ERROR: UInt8 = 0xBF
let CMD_PING: UInt8 = 0x81, CMD_CANCEL: UInt8 = 0x91, CMD_WINK: UInt8 = 0x88

// Minimal authenticatorGetInfo reply: versions ["FIDO_2_0"], aaguid zeros.
// CBOR: A2 01 81 68 "FIDO_2_0" 03 50 <16 zero bytes>
let getInfo: Data = {
    var d = Data([0x00, 0xA2, 0x01, 0x81, 0x68])  // status OK + map(2) + key 1 + array(1) + text(8)
    d.append("FIDO_2_0".data(using: .ascii)!)
    d.append(contentsOf: [0x03, 0x50])
    d.append(Data(count: 16))
    return d
}()

final class Fido: HIDVirtualDeviceDelegate, @unchecked Sendable {
    var device: HIDVirtualDevice?
    var nextCid: UInt32 = 0x0100_0000

    func hidVirtualDevice(_ device: HIDVirtualDevice, receivedSetReportRequestOfType type: HIDReportType,
                          id: HIDReportID?, data: Data) throws {
        log("OUT \(type) \(hex(data))")
        guard data.count >= 7 else { return }
        let cid = data.prefix(4)
        let cmd = data[data.startIndex + 4]
        let len = Int(data[data.startIndex + 5]) << 8 | Int(data[data.startIndex + 6])
        let payload = data.dropFirst(7).prefix(len)
        switch cmd {
        case CMD_INIT:
            // nonce(8) | new cid(4) | proto 2 | major minor build | caps (WINK|CBOR = 0x05)
            nextCid += 1
            var reply = Data(payload.prefix(8))
            withUnsafeBytes(of: nextCid.bigEndian) { reply.append(contentsOf: $0) }
            reply.append(contentsOf: [0x02, 0x00, 0x01, 0x00, 0x05])
            send(cid: cid, cmd: CMD_INIT, payload: reply)
        case CMD_PING:
            send(cid: cid, cmd: CMD_PING, payload: Data(payload))
        case CMD_WINK:
            send(cid: cid, cmd: CMD_WINK, payload: Data())
        case CMD_CBOR:
            let ctap = payload.first ?? 0
            log("  CTAP2 command 0x\(String(format: "%02x", ctap)) (\(payload.count) bytes)")
            if ctap == 0x04 { send(cid: cid, cmd: CMD_CBOR, payload: getInfo) }
            else { send(cid: cid, cmd: CMD_CBOR, payload: Data([0x2E])) }  // CTAP2_ERR_NO_CREDENTIALS
        case CMD_CANCEL:
            break
        default:
            send(cid: cid, cmd: CMD_ERROR, payload: Data([0x01]))  // ERR_INVALID_CMD
        }
    }

    func hidVirtualDevice(_ device: HIDVirtualDevice, receivedGetReportRequestOfType type: HIDReportType,
                          id: HIDReportID?, maxSize: size_t) throws -> Data {
        log("GET \(type)")
        return Data(count: 64)
    }

    /// Single-packet replies only (spike): payload must fit 57 bytes.
    func send(cid: Data, cmd: UInt8, payload: Data) {
        var packet = Data(cid)
        packet.append(cmd)
        packet.append(UInt8(payload.count >> 8)); packet.append(UInt8(payload.count & 0xFF))
        packet.append(payload)
        packet.append(Data(count: max(0, 64 - packet.count)))
        let device = self.device!
        Task {
            do {
                try await device.dispatchInputReport(data: packet, timestamp: SuspendingClock.now)
                log("IN  \(hex(packet.prefix(7 + payload.count)))")
            } catch { log("dispatch failed: \(error)") }
        }
    }
}

@main
struct Main {
    static func main() async {
        let properties = HIDVirtualDevice.Properties(
            descriptor: descriptor,
            vendorID: 0x1209, productID: 0x7A72,
            transport: .usb,
            product: "tapbridge security key (spike)",
            manufacturer: "tapbridge",
            serialNumber: "spike-0001"
        )
        guard let device = HIDVirtualDevice(properties: properties) else {
            log("HIDVirtualDevice(properties:) returned nil (missing entitlement?)")
            exit(1)
        }
        let fido = Fido()
        fido.device = device
        await device.activate(delegate: fido)
        log("virtual FIDO HID device active: \(device.deviceReference)")
        while true { try? await Task.sleep(for: .seconds(3600)) }
    }
}
