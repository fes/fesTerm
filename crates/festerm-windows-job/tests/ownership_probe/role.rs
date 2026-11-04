use std::{ffi::OsString, io};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    Creator,
    Worker,
}

impl Role {
    #[cfg(all(windows, feature = "experimental-windows-job-probe"))]
    pub fn argument(self) -> &'static str {
        match self {
            Self::Creator => "--ownership-probe-creator",
            Self::Worker => "--ownership-probe-worker",
        }
    }
}

pub fn parse(mut args: impl Iterator<Item = OsString>) -> io::Result<Role> {
    let first = args.next();
    if args.next().is_some() {
        return Err(io::Error::other("invalid ownership-probe helper arguments"));
    }
    match first.as_deref().and_then(|arg| arg.to_str()) {
        Some("--ownership-probe-creator") => Ok(Role::Creator),
        Some("--ownership-probe-worker") => Ok(Role::Worker),
        _ => Err(io::Error::other("invalid ownership-probe helper role")),
    }
}
