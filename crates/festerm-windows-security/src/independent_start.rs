//! Internal same-user WMI starter hop. It supplies no session authority.
use std::io;

pub const HOP_ENV: &str = "FESTERM_SESSIOND_WMI_STARTER";
const MAX_UNITS: usize = 32766;

fn command_line(arguments: &[Vec<u16>]) -> io::Result<Vec<u16>> {
    let mut result = Vec::new();
    for argument in arguments {
        if argument.contains(&0) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        if !result.is_empty() {
            result.push(32);
        }
        result.push(34);
        let mut slashes = 0;
        for &unit in argument {
            if unit == 92 {
                slashes += 1;
                continue;
            }
            result.extend(std::iter::repeat_n(
                92,
                if unit == 34 { slashes * 2 + 1 } else { slashes },
            ));
            slashes = 0;
            result.push(unit);
        }
        result.extend(std::iter::repeat_n(92, slashes * 2));
        result.push(34);
        if result.len() > MAX_UNITS {
            return Err(io::ErrorKind::InvalidInput.into());
        }
    }
    if result.is_empty() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(result)
}

fn marker(value: &str, now_ms: u128) -> io::Result<(u32, u64, u64)> {
    let fields: Vec<_> = value.split(':').collect();
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "independent starter marker invalid",
        )
    };
    if fields.len() != 5
        || fields[0] != "1"
        || fields[3].len() != 64
        || !fields[3].bytes().all(|c| c.is_ascii_hexdigit())
    {
        return Err(invalid());
    }
    let pid = fields[1].parse::<u32>().map_err(|_| invalid())?;
    let birth = fields[2].parse::<u64>().map_err(|_| invalid())?;
    let expiry = fields[4].parse::<u128>().map_err(|_| invalid())?;
    let remaining = expiry
        .checked_sub(now_ms)
        .filter(|v| *v > 0 && *v <= 7000)
        .ok_or_else(invalid)?;
    if pid == 0 || birth == 0 {
        return Err(invalid());
    }
    Ok((pid, birth, remaining as u64))
}

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{relaunch_current_starter, verify_successor};

#[cfg(test)]
mod tests {
    use super::*;
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }
    #[test]
    fn command_line_quotes_empty_unicode_quotes_and_trailing_slashes() {
        let actual = command_line(&[
            wide("C:\\fixed\\helper.exe"),
            wide(""),
            wide("a b"),
            wide("a\"b"),
            wide("end\\"),
            wide("日本語"),
        ])
        .unwrap();
        assert_eq!(
            String::from_utf16(&actual).unwrap(),
            "\"C:\\fixed\\helper.exe\" \"\" \"a b\" \"a\\\"b\" \"end\\\\\" \"日本語\""
        );
    }
    #[test]
    fn marker_is_one_hop_bounded_and_never_accepts_missing_context() {
        let nonce = "a".repeat(64);
        assert_eq!(
            marker(&format!("1:42:99:{nonce}:6000"), 1000).unwrap(),
            (42, 99, 5000)
        );
        for value in [
            format!("1:0:99:{nonce}:6000"),
            format!("1:42:0:{nonce}:6000"),
            format!("2:42:99:{nonce}:6000"),
            format!("1:42:99:{nonce}:9000"),
            format!("1:42:99:{nonce}:1000"),
            "skip".into(),
        ] {
            assert!(marker(&value, 1000).is_err());
        }
    }
    #[test]
    fn command_line_refuses_nul_and_oversize_before_native_effects() {
        assert!(command_line(&[]).is_err());
        assert!(command_line(&[vec![0]]).is_err());
        assert!(command_line(&[vec![97; MAX_UNITS]]).is_err());
    }
}
