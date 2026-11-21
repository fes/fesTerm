//! Selected legacy Keychain items. Mechanism only; callers own HOST consent and
//! authenticated executable selection. No existing item ACL is ever rewritten.

/// Finite failures without paths, references, ACLs or secret data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Missing,
    Unavailable,
    Collision,
    InvalidInput,
    UnsupportedIdentity,
    Policy,
    Native,
}

const LIMIT: usize = 64 * 1024;
const MAX_ACLS: usize = 16;
#[cfg(target_os = "macos")]
const MAX_AUTHORIZATIONS: usize = 32;

fn input(service: &[u8], account: &[u8], data: &[u8]) -> Result<(), Error> {
    if service.is_empty()
        || service.len() > 128
        || account.is_empty()
        || account.len() > 128
        || service.contains(&0)
        || account.contains(&0)
        || data.len() > LIMIT
    {
        return Err(Error::InvalidInput);
    }
    Ok(())
}

// None is an all-application list; Some(empty) is no automatic trust.
fn policy(entries: &[(bool, bool, Option<Vec<u8>>)]) -> Result<(), Error> {
    if entries.is_empty() || entries.len() > MAX_ACLS {
        return Err(Error::Policy);
    }
    let mut read = false;
    let mut owner = false;
    for (decrypt, administration, roles) in entries {
        if *administration {
            owner = true;
            if roles.as_deref() != Some(&[]) {
                return Err(Error::Policy);
            }
        }
        if *decrypt {
            read = true;
            let mut roles = roles.clone().ok_or(Error::Policy)?;
            roles.sort_unstable();
            if roles != [0, 1] {
                return Err(Error::Policy);
            }
        }
    }
    if read && owner {
        Ok(())
    } else {
        Err(Error::Policy)
    }
}

#[cfg(target_os = "macos")]
mod native;
#[cfg(target_os = "macos")]
pub use native::{Applications, Item};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mac_host_item_policy_distinguishes_any_app_empty_and_exact_two() {
        let owner = (false, true, Some(vec![]));
        assert_eq!(
            policy(&[(true, false, Some(vec![1, 0])), owner.clone()]),
            Ok(())
        );
        for apps in [
            None,
            Some(vec![]),
            Some(vec![0]),
            Some(vec![0, 0]),
            Some(vec![0, 1, 2]),
        ] {
            assert_eq!(
                policy(&[(true, false, apps), owner.clone()]),
                Err(Error::Policy)
            );
        }
        for apps in [None, Some(vec![0]), Some(vec![1])] {
            assert_eq!(
                policy(&[(true, false, Some(vec![0, 1])), (false, true, apps)]),
                Err(Error::Policy)
            );
        }
        assert_eq!(
            policy(&[(true, false, Some(vec![0, 1]))]),
            Err(Error::Policy)
        );
        assert_eq!(policy(&[owner]), Err(Error::Policy));
    }
    #[test]
    fn mac_host_item_policy_checks_every_read_entry_and_bounds() {
        let valid = [(true, false, Some(vec![0, 1])), (false, true, Some(vec![]))];
        let mut broader = valid.to_vec();
        broader.push((true, false, None));
        assert_eq!(policy(&broader), Err(Error::Policy));
        broader[2] = (false, false, None); // standard ENCRYPT is not plaintext read
        assert_eq!(policy(&broader), Ok(()));
        assert_eq!(
            policy(&vec![(false, false, None); MAX_ACLS + 1]),
            Err(Error::Policy)
        );
        assert_eq!(input(b"service", b"account", &[0; LIMIT]), Ok(()));
        for (s, a, d) in [
            (b"".as_slice(), b"a".as_slice(), vec![]),
            (b"s", b"a\0", vec![]),
            (b"s", b"a", vec![0; LIMIT + 1]),
        ] {
            assert_eq!(input(s, a, &d), Err(Error::InvalidInput));
        }
    }
}

fn create_if_missing<T>(
    find: impl FnOnce() -> Result<(), Error>,
    create: impl FnOnce() -> Result<T, Error>,
) -> Result<T, Error> {
    match find() {
        Ok(()) => Err(Error::Collision),
        Err(Error::Missing) => create(),
        Err(error) => Err(error),
    }
}
fn update_selected(
    check: impl FnOnce() -> Result<(), Error>,
    modify: impl FnOnce() -> Result<(), Error>,
) -> Result<(), Error> {
    check()?;
    modify()
}

#[cfg(test)]
mod operation_tests {
    use super::*;
    #[test]
    fn mac_host_item_creation_only_follows_proven_absence() {
        for result in [
            Ok(()),
            Err(Error::Unavailable),
            Err(Error::Policy),
            Err(Error::Native),
        ] {
            let expected = result.err().unwrap_or(Error::Collision);
            assert_eq!(
                create_if_missing(|| result, || panic!("no publication")),
                Err::<(), _>(expected)
            );
        }
        assert_eq!(create_if_missing(|| Err(Error::Missing), || Ok(7)), Ok(7));
        assert_eq!(
            create_if_missing(|| Err(Error::Missing), || Err::<(), _>(Error::Collision)),
            Err(Error::Collision)
        );
    }
    #[test]
    fn mac_host_item_update_has_no_create_fallback_and_preserves_primary_errors() {
        for error in [
            Error::Missing,
            Error::Unavailable,
            Error::Policy,
            Error::Native,
        ] {
            assert_eq!(
                update_selected(|| Err(error), || panic!("no modify")),
                Err(error)
            );
            assert_eq!(update_selected(|| Ok(()), || Err(error)), Err(error));
        }
        assert_eq!(update_selected(|| Ok(()), || Ok(())), Ok(()));
    }
}
