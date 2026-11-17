//! Serial ports as a session type.
//!
//! The `serialport` crate (without its `libudev` feature, so Linux needs no
//! system library: ports are found by scanning sysfs) opens the device; the
//! bytes then travel through [`TermSession`](crate::terminal::TermSession)
//! like any terminal's, which is what gives serial sessions scrollback, the
//! session log and shell integration.
//!
//! Permissions per platform:
//!
//! * Linux: `/dev/tty*` belong to a group (`dialout` on Debian, Ubuntu and
//!   Fedora; `uucp` on Arch, `tty` on some others). A user outside it gets
//!   `EACCES`, which is reported with the fix.
//! * macOS: `/dev/cu.*` are open to every user; nothing to configure.
//! * Windows: `COMn` need no permission. A port another program holds open is
//!   reported as busy.

use easyssh_core::{SerialFlowControl, SerialParity, SerialSettings};
use serialport::{DataBits, ErrorKind, FlowControl, Parity, SerialPort, StopBits};
use std::io;
use std::time::Duration;

/// How long a read waits. Short, so closing a session is noticed quickly.
pub const READ_TIMEOUT: Duration = Duration::from_millis(100);

/// A failure to open a port, with a stable code for the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenError {
    pub code: &'static str,
    pub message: String,
}

/// A serial port as listed for the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortEntry {
    pub path: String,
    pub description: Option<String>,
}

fn data_bits(bits: u8) -> DataBits {
    match bits {
        5 => DataBits::Five,
        6 => DataBits::Six,
        7 => DataBits::Seven,
        _ => DataBits::Eight,
    }
}

fn parity(parity: SerialParity) -> Parity {
    match parity {
        SerialParity::None => Parity::None,
        SerialParity::Odd => Parity::Odd,
        SerialParity::Even => Parity::Even,
    }
}

fn stop_bits(bits: u8) -> StopBits {
    if bits >= 2 {
        StopBits::Two
    } else {
        StopBits::One
    }
}

fn flow_control(flow: SerialFlowControl) -> FlowControl {
    match flow {
        SerialFlowControl::None => FlowControl::None,
        SerialFlowControl::Software => FlowControl::Software,
        SerialFlowControl::Hardware => FlowControl::Hardware,
    }
}

/// Opens the port with the host's settings and a short read timeout.
pub fn open(settings: &SerialSettings) -> Result<Box<dyn SerialPort>, OpenError> {
    serialport::new(&settings.port, settings.baud)
        .data_bits(data_bits(settings.data_bits))
        .parity(parity(settings.parity))
        .stop_bits(stop_bits(settings.stop_bits))
        .flow_control(flow_control(settings.flow_control))
        .timeout(READ_TIMEOUT)
        .open()
        .map_err(|error| open_error(&settings.port, &error))
}

/// Turns the OS's failure into a message that says what to do about it.
pub fn open_error(port: &str, error: &serialport::Error) -> OpenError {
    match error.kind() {
        ErrorKind::Io(io::ErrorKind::PermissionDenied) => OpenError {
            code: "serial_permission_denied",
            message: permission_message(port),
        },
        ErrorKind::Io(io::ErrorKind::NotFound) => OpenError {
            code: "serial_not_found",
            message: format!("{port} does not exist. Is the device plugged in?"),
        },
        ErrorKind::NoDevice => OpenError {
            code: "serial_busy",
            message: format!("{port} is in use by another program or was unplugged."),
        },
        _ => OpenError {
            code: "serial_open_failed",
            message: format!("cannot open {port}: {}", error.description),
        },
    }
}

fn permission_message(port: &str) -> String {
    if cfg!(target_os = "linux") {
        format!(
            "Permission denied opening {port}. Add your user to the group that owns the \
             port (`dialout` on Debian, Ubuntu and Fedora, `uucp` on Arch), for example \
             `sudo usermod -aG dialout $USER`, then log out and back in."
        )
    } else {
        format!("Permission denied opening {port}.")
    }
}

/// The ports the OS lists, sorted by path.
pub fn list_ports() -> Result<Vec<PortEntry>, String> {
    let mut ports: Vec<PortEntry> = serialport::available_ports()
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|info| PortEntry {
            description: describe(&info.port_type),
            path: info.port_name,
        })
        .collect();
    ports.sort_by(|a, b| a.path.cmp(&b.path));
    ports.dedup_by(|a, b| a.path == b.path);
    Ok(ports)
}

fn describe(kind: &serialport::SerialPortType) -> Option<String> {
    use serialport::SerialPortType::*;
    match kind {
        UsbPort(usb) => {
            let name: Vec<&str> = [usb.manufacturer.as_deref(), usb.product.as_deref()]
                .into_iter()
                .flatten()
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .collect();
            let ids = format!("USB {:04x}:{:04x}", usb.vid, usb.pid);
            Some(if name.is_empty() {
                ids
            } else {
                format!("{} ({ids})", name.join(" "))
            })
        }
        PciPort => Some("PCI".to_owned()),
        BluetoothPort => Some("Bluetooth".to_owned()),
        Unknown => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(kind: ErrorKind) -> serialport::Error {
        serialport::Error::new(kind, "os says no")
    }

    #[test]
    fn eacces_names_the_group_to_join_on_linux() {
        let failure = open_error(
            "/dev/ttyUSB0",
            &error(ErrorKind::Io(io::ErrorKind::PermissionDenied)),
        );
        assert_eq!(failure.code, "serial_permission_denied");
        assert!(failure.message.contains("/dev/ttyUSB0"));
        if cfg!(target_os = "linux") {
            assert!(failure.message.contains("dialout"));
            assert!(failure.message.contains("usermod"));
        }
    }

    #[test]
    fn other_failures_get_their_own_codes() {
        assert_eq!(
            open_error("COM9", &error(ErrorKind::Io(io::ErrorKind::NotFound))).code,
            "serial_not_found"
        );
        assert_eq!(
            open_error("COM9", &error(ErrorKind::NoDevice)).code,
            "serial_busy"
        );
        let other = open_error("COM9", &error(ErrorKind::Unknown));
        assert_eq!(other.code, "serial_open_failed");
        assert!(other.message.contains("os says no"));
    }

    #[test]
    fn settings_map_onto_the_crates_types() {
        assert_eq!(data_bits(5), DataBits::Five);
        assert_eq!(data_bits(8), DataBits::Eight);
        assert_eq!(stop_bits(1), StopBits::One);
        assert_eq!(stop_bits(2), StopBits::Two);
        assert_eq!(parity(SerialParity::Even), Parity::Even);
        assert_eq!(
            flow_control(SerialFlowControl::Hardware),
            FlowControl::Hardware
        );
    }

    #[test]
    fn a_missing_device_cannot_be_opened() {
        let Err(failure) = open(&SerialSettings::new("/dev/definitely-not-a-serial-port")) else {
            panic!("a device that does not exist was opened");
        };
        assert!(!failure.message.is_empty());
        assert_ne!(failure.code, "serial_permission_denied");
    }

    #[test]
    fn listing_ports_does_not_fail_on_a_machine_without_any() {
        assert!(list_ports().is_ok());
    }

    #[test]
    fn usb_ports_are_described_by_product_and_ids() {
        let usb = serialport::SerialPortType::UsbPort(serialport::UsbPortInfo {
            vid: 0x0403,
            pid: 0x6001,
            serial_number: None,
            manufacturer: Some("FTDI".into()),
            product: Some("FT232R USB UART".into()),
        });
        assert_eq!(
            describe(&usb).as_deref(),
            Some("FTDI FT232R USB UART (USB 0403:6001)")
        );
        assert_eq!(describe(&serialport::SerialPortType::Unknown), None);
    }
}
