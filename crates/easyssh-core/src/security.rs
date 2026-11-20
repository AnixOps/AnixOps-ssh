use crate::domain::{Connection, ConnectionTarget, SerialSettings};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ValidationError {
    #[error("{field} is required")]
    Required { field: &'static str },
    #[error("{field} contains an unsafe value")]
    Unsafe { field: &'static str },
    #[error("port must be between 1 and 65535")]
    Port,
    #[error("baud rate must be between 50 and 4000000")]
    Baud,
    #[error("data bits must be 5, 6, 7 or 8, and stop bits 1 or 2")]
    SerialFraming,
    #[error("a serial port has no SSH connection")]
    NotSsh,
}

pub fn validate_connection(connection: &Connection) -> Result<(), ValidationError> {
    validate_text(&connection.name, "name")?;
    match &connection.target {
        ConnectionTarget::Alias { alias } => validate_alias(alias),
        ConnectionTarget::Endpoint {
            hostname,
            username,
            port,
        } => {
            validate_host(hostname)?;
            if let Some(username) = username {
                validate_username(username)?;
            }
            if *port == 0 {
                return Err(ValidationError::Port);
            }
            Ok(())
        }
        ConnectionTarget::Serial(serial) => validate_serial(serial),
    }
}

/// The settings of a serial port. The path is handed to the OS as is, so it
/// only has to be a non-empty single line without NUL.
pub fn validate_serial(serial: &SerialSettings) -> Result<(), ValidationError> {
    validate_path(&serial.port, "serial port")?;
    if serial.port.chars().any(char::is_control) {
        return Err(ValidationError::Unsafe {
            field: "serial port",
        });
    }
    if !(50..=4_000_000).contains(&serial.baud) {
        return Err(ValidationError::Baud);
    }
    if !(5..=8).contains(&serial.data_bits) || !(1..=2).contains(&serial.stop_bits) {
        return Err(ValidationError::SerialFraming);
    }
    Ok(())
}

pub fn validate_alias(value: &str) -> Result<(), ValidationError> {
    validate_token(value, "alias", true)
}
/// A user name: letters, digits, `_`, `-`, `.`, and `@`. The last two exist
/// for gateways that carry the target in the user name (JumpServer KoKo:
/// `jms_user@account@10.0.0.5`); ssh splits `user@host` at the last `@`, so
/// the host is still the one after it. No leading `-`, nothing else special.
pub fn validate_username(value: &str) -> Result<(), ValidationError> {
    if value.is_empty() {
        return Err(ValidationError::Required { field: "username" });
    }
    if value.starts_with('-')
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '@'))
    {
        return Err(ValidationError::Unsafe { field: "username" });
    }
    Ok(())
}
pub fn validate_host(value: &str) -> Result<(), ValidationError> {
    if value.is_empty() {
        return Err(ValidationError::Required { field: "hostname" });
    }
    if value.starts_with('-')
        || value.chars().any(|c| {
            c.is_control() || c.is_whitespace() || matches!(c, ';' | '&' | '|' | '`' | '$')
        })
    {
        return Err(ValidationError::Unsafe { field: "hostname" });
    }
    Ok(())
}

pub fn validate_path(value: &str, field: &'static str) -> Result<(), ValidationError> {
    if value.is_empty() {
        return Err(ValidationError::Required { field });
    }
    if value.chars().any(|c| c == '\0') {
        return Err(ValidationError::Unsafe { field });
    }
    Ok(())
}

fn validate_text(value: &str, field: &'static str) -> Result<(), ValidationError> {
    if value.trim().is_empty() {
        return Err(ValidationError::Required { field });
    }
    if value.chars().any(char::is_control) {
        return Err(ValidationError::Unsafe { field });
    }
    Ok(())
}

fn validate_token(
    value: &str,
    field: &'static str,
    allow_dot: bool,
) -> Result<(), ValidationError> {
    if value.is_empty() {
        return Err(ValidationError::Required { field });
    }
    if value.starts_with('-')
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || (allow_dot && c == '.'))
    {
        return Err(ValidationError::Unsafe { field });
    }
    Ok(())
}

pub fn serialized_has_sensitive_keys(value: &serde_json::Value) -> bool {
    const FORBIDDEN: [&str; 7] = [
        "password",
        "passphrase",
        "private_key",
        "secret",
        "token",
        "vault",
        "credential",
    ];
    match value {
        serde_json::Value::Object(map) => map.iter().any(|(key, value)| {
            FORBIDDEN
                .iter()
                .any(|word| key.to_ascii_lowercase().contains(word))
                || serialized_has_sensitive_keys(value)
        }),
        serde_json::Value::Array(values) => values.iter().any(serialized_has_sensitive_keys),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gateway_style_user_names_are_accepted_but_not_options() {
        assert!(validate_username("jms@root@10.0.0.5").is_ok());
        assert!(validate_username("first.last").is_ok());
        for bad in ["", "-oProxyCommand=x", "a b", "a;b", "a$b", "a\nb"] {
            assert!(validate_username(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn rejects_argument_injection() {
        assert!(validate_host("-oProxyCommand=x").is_err());
        assert!(validate_alias("host;id").is_err());
        assert!(validate_path("-target", "local path").is_ok());
    }
}

#[cfg(test)]
mod serial_tests {
    use super::*;
    use crate::domain::{Connection, SerialSettings};

    fn serial(settings: SerialSettings) -> Connection {
        let mut connection = Connection::alias("console", "x");
        connection.target = ConnectionTarget::Serial(settings);
        connection
    }

    #[test]
    fn a_default_serial_port_is_valid() {
        validate_connection(&serial(SerialSettings::new("/dev/ttyUSB0"))).unwrap();
        validate_connection(&serial(SerialSettings::new("COM3"))).unwrap();
    }

    #[test]
    fn bad_serial_settings_are_refused() {
        let mut s = SerialSettings::new("");
        assert_eq!(
            validate_connection(&serial(s.clone())),
            Err(ValidationError::Required {
                field: "serial port"
            })
        );
        s.port = "/dev/tty\nUSB".into();
        assert!(validate_connection(&serial(s.clone())).is_err());
        s.port = "COM1".into();
        s.baud = 10;
        assert_eq!(
            validate_connection(&serial(s.clone())),
            Err(ValidationError::Baud)
        );
        s.baud = 9600;
        s.data_bits = 9;
        assert_eq!(
            validate_connection(&serial(s.clone())),
            Err(ValidationError::SerialFraming)
        );
        s.data_bits = 8;
        s.stop_bits = 3;
        assert_eq!(
            validate_connection(&serial(s)),
            Err(ValidationError::SerialFraming)
        );
    }

    #[test]
    fn serial_hosts_serialise_with_their_kind_and_never_become_ssh() {
        let connection = serial(SerialSettings::new("/dev/ttyS0"));
        let value = serde_json::to_value(&connection.target).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "kind": "serial", "port": "/dev/ttyS0", "baud": 115200,
                "data_bits": 8, "parity": "none", "stop_bits": 1, "flow_control": "none"
            })
        );
        let back: ConnectionTarget = serde_json::from_value(value).unwrap();
        assert_eq!(back, connection.target);
        assert!(crate::SshInvocation::for_connection(&crate::OpenSsh, &connection).is_err());
        assert!(crate::sftp::sftp_args(&connection, false).is_err());
    }
}
