//! USB host stack — protocol layer.
//!
//! References:
//!   * USB 1.1 Specification (1998) — packet formats, descriptors,
//!     standard requests.
//!   * USB 2.0 Specification §9 — for the modern descriptor formats
//!     UHCI/USB-1 hardware also speaks at low speed.
//!   * HID 1.11 Specification — boot protocol (keyboard + mouse).
//!
//! What's here:
//!   * Standard USB descriptor structs (Device / Configuration /
//!     Interface / Endpoint / HID).
//!   * Standard request constants.
//!   * Speed enum.
//!
//! What's *not* here:
//!   * Host controller — see `drivers::usb::uhci`.
//!   * HID class driver — see `drivers::usb::hid`.

pub mod uhci;
pub mod hid;

// ---------------------------------------------------------------------------
// Descriptor types (USB 2.0 §9.4 Table 9-5)
// ---------------------------------------------------------------------------

pub const USB_DT_DEVICE:        u8 = 0x01;
pub const USB_DT_CONFIGURATION: u8 = 0x02;
pub const USB_DT_STRING:        u8 = 0x03;
pub const USB_DT_INTERFACE:     u8 = 0x04;
pub const USB_DT_ENDPOINT:      u8 = 0x05;
pub const USB_DT_HID:           u8 = 0x21;
pub const USB_DT_HID_REPORT:    u8 = 0x22;

// ---------------------------------------------------------------------------
// Standard requests (USB 2.0 §9.4 Table 9-3)
// ---------------------------------------------------------------------------

pub const USB_REQ_GET_STATUS:        u8 = 0x00;
pub const USB_REQ_CLEAR_FEATURE:     u8 = 0x01;
pub const USB_REQ_SET_FEATURE:       u8 = 0x03;
pub const USB_REQ_SET_ADDRESS:       u8 = 0x05;
pub const USB_REQ_GET_DESCRIPTOR:    u8 = 0x06;
pub const USB_REQ_SET_DESCRIPTOR:    u8 = 0x07;
pub const USB_REQ_GET_CONFIGURATION: u8 = 0x08;
pub const USB_REQ_SET_CONFIGURATION: u8 = 0x09;
pub const USB_REQ_GET_INTERFACE:     u8 = 0x0A;
pub const USB_REQ_SET_INTERFACE:     u8 = 0x0B;

// ---------------------------------------------------------------------------
// HID class requests (HID 1.11 §7.2)
// ---------------------------------------------------------------------------

pub const HID_REQ_GET_REPORT:   u8 = 0x01;
pub const HID_REQ_GET_IDLE:     u8 = 0x02;
pub const HID_REQ_GET_PROTOCOL: u8 = 0x03;
pub const HID_REQ_SET_REPORT:   u8 = 0x09;
pub const HID_REQ_SET_IDLE:     u8 = 0x0A;
pub const HID_REQ_SET_PROTOCOL: u8 = 0x0B;

pub const HID_PROTO_BOOT:   u16 = 0;
pub const HID_PROTO_REPORT: u16 = 1;

// ---------------------------------------------------------------------------
// bmRequestType bits (USB 2.0 §9.3.1)
// ---------------------------------------------------------------------------

pub const USB_DIR_OUT:        u8 = 0 << 7;
pub const USB_DIR_IN:         u8 = 1 << 7;
pub const USB_TYPE_STANDARD:  u8 = 0 << 5;
pub const USB_TYPE_CLASS:     u8 = 1 << 5;
pub const USB_TYPE_VENDOR:    u8 = 2 << 5;
pub const USB_RECIP_DEVICE:    u8 = 0;
pub const USB_RECIP_INTERFACE: u8 = 1;
pub const USB_RECIP_ENDPOINT:  u8 = 2;

// ---------------------------------------------------------------------------
// Descriptor structs — packed for direct mapping over wire bytes.
// ---------------------------------------------------------------------------

/// 18-byte device descriptor (USB 2.0 §9.6.1, Table 9-8).
#[repr(C, packed)]
#[derive(Clone, Copy, Debug)]
pub struct DeviceDescriptor {
    pub b_length: u8,
    pub b_descriptor_type: u8,
    pub bcd_usb: u16,
    pub b_device_class: u8,
    pub b_device_sub_class: u8,
    pub b_device_protocol: u8,
    pub b_max_packet_size0: u8,
    pub id_vendor: u16,
    pub id_product: u16,
    pub bcd_device: u16,
    pub i_manufacturer: u8,
    pub i_product: u8,
    pub i_serial_number: u8,
    pub b_num_configurations: u8,
}

impl DeviceDescriptor {
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < 18 || b[0] < 18 || b[1] != USB_DT_DEVICE { return None; }
        Some(Self {
            b_length: b[0],
            b_descriptor_type: b[1],
            bcd_usb: u16::from_le_bytes([b[2], b[3]]),
            b_device_class: b[4],
            b_device_sub_class: b[5],
            b_device_protocol: b[6],
            b_max_packet_size0: b[7],
            id_vendor: u16::from_le_bytes([b[8], b[9]]),
            id_product: u16::from_le_bytes([b[10], b[11]]),
            bcd_device: u16::from_le_bytes([b[12], b[13]]),
            i_manufacturer: b[14],
            i_product: b[15],
            i_serial_number: b[16],
            b_num_configurations: b[17],
        })
    }
}

/// 9-byte configuration descriptor (USB 2.0 §9.6.3, Table 9-10).
#[repr(C, packed)]
#[derive(Clone, Copy, Debug)]
pub struct ConfigurationDescriptor {
    pub b_length: u8,
    pub b_descriptor_type: u8,
    pub w_total_length: u16,
    pub b_num_interfaces: u8,
    pub b_configuration_value: u8,
    pub i_configuration: u8,
    pub bm_attributes: u8,
    pub b_max_power: u8,
}

impl ConfigurationDescriptor {
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < 9 || b[0] < 9 || b[1] != USB_DT_CONFIGURATION { return None; }
        Some(Self {
            b_length: b[0],
            b_descriptor_type: b[1],
            w_total_length: u16::from_le_bytes([b[2], b[3]]),
            b_num_interfaces: b[4],
            b_configuration_value: b[5],
            i_configuration: b[6],
            bm_attributes: b[7],
            b_max_power: b[8],
        })
    }
}

/// 9-byte interface descriptor (USB 2.0 §9.6.5, Table 9-12).
#[repr(C, packed)]
#[derive(Clone, Copy, Debug)]
pub struct InterfaceDescriptor {
    pub b_length: u8,
    pub b_descriptor_type: u8,
    pub b_interface_number: u8,
    pub b_alternate_setting: u8,
    pub b_num_endpoints: u8,
    pub b_interface_class: u8,
    pub b_interface_sub_class: u8,
    pub b_interface_protocol: u8,
    pub i_interface: u8,
}

impl InterfaceDescriptor {
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < 9 || b[0] < 9 || b[1] != USB_DT_INTERFACE { return None; }
        Some(Self {
            b_length: b[0],
            b_descriptor_type: b[1],
            b_interface_number: b[2],
            b_alternate_setting: b[3],
            b_num_endpoints: b[4],
            b_interface_class: b[5],
            b_interface_sub_class: b[6],
            b_interface_protocol: b[7],
            i_interface: b[8],
        })
    }
}

/// 7-byte endpoint descriptor (USB 2.0 §9.6.6, Table 9-13).
#[repr(C, packed)]
#[derive(Clone, Copy, Debug)]
pub struct EndpointDescriptor {
    pub b_length: u8,
    pub b_descriptor_type: u8,
    pub b_endpoint_address: u8,    // bit 7 = IN; bits 0-3 = endpoint number
    pub bm_attributes: u8,         // bits 0-1: 00 control, 01 isoc, 10 bulk, 11 interrupt
    pub w_max_packet_size: u16,
    pub b_interval: u8,
}

impl EndpointDescriptor {
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < 7 || b[0] < 7 || b[1] != USB_DT_ENDPOINT { return None; }
        Some(Self {
            b_length: b[0],
            b_descriptor_type: b[1],
            b_endpoint_address: b[2],
            bm_attributes: b[3],
            w_max_packet_size: u16::from_le_bytes([b[4], b[5]]),
            b_interval: b[6],
        })
    }
    pub fn endpoint_number(&self) -> u8 { self.b_endpoint_address & 0x0F }
    pub fn is_in(&self) -> bool { (self.b_endpoint_address & 0x80) != 0 }
    pub fn transfer_type(&self) -> u8 { self.bm_attributes & 0x03 }
}

/// USB device speed.  UHCI handles low-speed (1.5 Mbit/s) and full-speed
/// (12 Mbit/s) devices; high-speed (480 Mbit/s) needs EHCI/xHCI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speed {
    Low,    // 1.5 Mbit/s — keyboards, mice
    Full,   // 12 Mbit/s — most devices on USB 1.1
}

// ---------------------------------------------------------------------------
// Class codes (USB-IF base class codes, www.usb.org/defined-class-codes)
// ---------------------------------------------------------------------------

pub const USB_CLASS_HID: u8 = 0x03;
/// HID subclass for boot-protocol devices (set when the device supports
/// the simplified 8-byte/3-byte fixed reports).
pub const USB_HID_SUBCLASS_BOOT: u8 = 0x01;
/// HID protocol code: 1=keyboard, 2=mouse (within the boot subclass).
pub const USB_HID_PROTO_KEYBOARD: u8 = 0x01;
pub const USB_HID_PROTO_MOUSE:    u8 = 0x02;

/// 8-byte SETUP packet (USB 2.0 §9.3, Table 9-2).
#[repr(C, packed)]
#[derive(Clone, Copy, Debug)]
pub struct SetupPacket {
    pub bm_request_type: u8,
    pub b_request: u8,
    pub w_value: u16,
    pub w_index: u16,
    pub w_length: u16,
}

impl SetupPacket {
    pub fn get_descriptor(desc_type: u8, desc_index: u8, lang: u16, len: u16) -> Self {
        Self {
            bm_request_type: USB_DIR_IN | USB_TYPE_STANDARD | USB_RECIP_DEVICE,
            b_request: USB_REQ_GET_DESCRIPTOR,
            w_value: ((desc_type as u16) << 8) | desc_index as u16,
            w_index: lang,
            w_length: len,
        }
    }
    pub fn set_address(addr: u8) -> Self {
        Self {
            bm_request_type: USB_DIR_OUT | USB_TYPE_STANDARD | USB_RECIP_DEVICE,
            b_request: USB_REQ_SET_ADDRESS,
            w_value: addr as u16,
            w_index: 0,
            w_length: 0,
        }
    }
    pub fn set_configuration(cfg: u8) -> Self {
        Self {
            bm_request_type: USB_DIR_OUT | USB_TYPE_STANDARD | USB_RECIP_DEVICE,
            b_request: USB_REQ_SET_CONFIGURATION,
            w_value: cfg as u16,
            w_index: 0,
            w_length: 0,
        }
    }
    pub fn set_protocol_boot(iface: u16) -> Self {
        Self {
            bm_request_type: USB_DIR_OUT | USB_TYPE_CLASS | USB_RECIP_INTERFACE,
            b_request: HID_REQ_SET_PROTOCOL,
            w_value: HID_PROTO_BOOT,
            w_index: iface,
            w_length: 0,
        }
    }

    pub fn as_bytes(&self) -> [u8; 8] {
        [
            self.bm_request_type,
            self.b_request,
            self.w_value as u8, (self.w_value >> 8) as u8,
            self.w_index as u8, (self.w_index >> 8) as u8,
            self.w_length as u8, (self.w_length >> 8) as u8,
        ]
    }
}
