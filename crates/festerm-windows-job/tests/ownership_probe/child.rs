//! Private child roles for the experimental ownership test.

#[path = "role.rs"]
pub mod role;
#[path = "runner_gate.rs"]
pub mod runner_gate;

#[cfg(windows)]
#[path = "fixture.rs"]
pub mod fixture;

fn main() {
    if let Err(error) = run() {
        eprintln!("experimental ownership-probe helper refused: {error}");
        std::process::exit(1);
    }
}

fn run() -> std::io::Result<()> {
    let role = role::parse(std::env::args_os().skip(1))?;
    runner_gate::require_current()?;
    #[cfg(windows)]
    {
        match role {
            role::Role::Creator => fixture::probe::creator(),
            role::Role::Worker => Err(std::io::Error::other(
                "suspended fixture unexpectedly executed",
            )),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = role;
        Err(std::io::Error::other("Windows is required"))
    }
}
