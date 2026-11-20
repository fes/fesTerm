//! Fresh native reads with no unlock, prompt, initialization, or fallback path.
use super::*;

#[cfg(target_os = "macos")]
pub(super) fn initialize_headless_keychain() -> Result<(), SecretStoreError> {
    use security_framework::os::macos::keychain::SecKeychain;
    install_headless_fence(
        || {
            let guard = SecKeychain::disable_user_interaction()
                .map_err(|_| SecretStoreError::LockedOrUnavailable)?;
            // Drop would re-enable process UI. This dedicated worker keeps the
            // fence for its whole lifetime, including failed startup/cleanup.
            std::mem::forget(guard);
            Ok(())
        },
        || {
            SecKeychain::user_interaction_allowed()
                .map_err(|_| SecretStoreError::LockedOrUnavailable)
        },
    )?;
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
fn install_headless_fence(
    set: impl FnOnce() -> Result<(), SecretStoreError>,
    read: impl FnOnce() -> Result<bool, SecretStoreError>,
) -> Result<(), SecretStoreError> {
    set()?;
    match read()? {
        false => Ok(()),
        true => Err(SecretStoreError::LockedOrUnavailable),
    }
}

#[cfg(any(target_os = "macos", test))]
fn mac_results(results: Vec<Option<Vec<u8>>>) -> Result<SecretBytes, SecretStoreError> {
    let mut selected = None;
    let mut invalid = false;
    // Consume and zeroize every returned data buffer, even on ambiguity/shape errors.
    for result in results {
        match result {
            Some(bytes) => {
                let secret = SecretBytes::from_vec(bytes);
                if selected.is_some() {
                    invalid = true;
                } else {
                    selected = Some(secret);
                }
            }
            None => invalid = true,
        }
    }
    if invalid {
        return Err(SecretStoreError::BackendFailure);
    }
    // UISkip cannot distinguish absent items from items omitted for authentication.
    let secret = selected.ok_or(SecretStoreError::LockedOrUnavailable)?;
    check_size(&secret)?;
    Ok(secret)
}

#[cfg(target_os = "macos")]
pub(super) fn get(reference: &SecretReference) -> Result<SecretBytes, SecretStoreError> {
    use security_framework::{
        item::{ItemClass, ItemSearchOptions, SearchResult},
        os::macos::keychain::{SecKeychain, SecPreferencesDomain},
    };
    let keychain = SecKeychain::default_for_domain(SecPreferencesDomain::User)
        .map_err(|e| map_keyring_error(apple_native_keyring_store::keychain::decode_error(e)))?;
    let mut query = ItemSearchOptions::new();
    // Apple: silently omit authentication-requiring items for SecItemCopyMatching.
    // https://developer.apple.com/documentation/security/ksecuseauthenticationuiskip
    // Same User/login keychain, generic class, service/account as NativeSecretStore.
    query
        .keychains(&[keychain])
        .class(ItemClass::generic_password())
        .case_insensitive(Some(false))
        .service(SERVICE_NAMESPACE)
        .account(&reference.account_name())
        .load_data(true)
        .limit(2)
        .skip_authenticated_items(true);
    let results = query.search().map_err(|e| {
        if e.code() == -25300 {
            SecretStoreError::LockedOrUnavailable
        } else {
            map_keyring_error(apple_native_keyring_store::keychain::decode_error(e))
        }
    })?;
    mac_results(
        results
            .into_iter()
            .map(|r| match r {
                SearchResult::Data(bytes) => Some(bytes),
                _ => None,
            })
            .collect(),
    )
}

#[cfg(any(
    all(
        unix,
        not(any(target_os = "macos", target_os = "ios", target_os = "android"))
    ),
    test
))]
fn unlocked_result<T>(
    locked: usize,
    mut unlocked: Vec<T>,
    read: impl FnOnce(T) -> Result<Vec<u8>, SecretStoreError>,
) -> Result<SecretBytes, SecretStoreError> {
    if locked != 0 {
        return Err(SecretStoreError::LockedOrUnavailable);
    }
    if unlocked.is_empty() {
        return Err(SecretStoreError::Missing);
    }
    if unlocked.len() != 1 {
        return Err(SecretStoreError::BackendFailure);
    }
    let secret = SecretBytes::from_vec(read(unlocked.remove(0))?);
    check_size(&secret)?;
    Ok(secret)
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
pub(super) fn get(reference: &SecretReference) -> Result<SecretBytes, SecretStoreError> {
    use secret_service::{blocking::SecretService, EncryptionType};
    fn error(e: secret_service::Error) -> SecretStoreError {
        match e {
            secret_service::Error::Locked
            | secret_service::Error::Prompt
            | secret_service::Error::Unavailable => SecretStoreError::LockedOrUnavailable,
            secret_service::Error::NoResult => SecretStoreError::Missing,
            _ => SecretStoreError::BackendFailure,
        }
    }
    let service = SecretService::connect(EncryptionType::Dh).map_err(error)?;
    let account = reference.account_name();
    let result = service
        .search_items(HashMap::from([
            ("service", SERVICE_NAMESPACE),
            ("username", account.as_str()),
        ]))
        .map_err(error)?;
    // SearchItems and GetSecret do not call Unlock or Prompt. A lock race must
    // fail from GetSecret; it never retries via the interactive keyring adapter.
    unlocked_result(result.locked.len(), result.unlocked, |item| {
        item.get_secret().map_err(error)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn headless_fence_requires_successful_set_and_false_readback() {
        let reads = std::cell::Cell::new(0);
        assert_eq!(
            install_headless_fence(
                || Err(SecretStoreError::BackendFailure),
                || {
                    reads.set(1);
                    Ok(false)
                }
            ),
            Err(SecretStoreError::BackendFailure)
        );
        assert_eq!(reads.get(), 0);
        assert_eq!(
            install_headless_fence(|| Ok(()), || Ok(true)),
            Err(SecretStoreError::LockedOrUnavailable)
        );
        assert_eq!(
            install_headless_fence(|| Ok(()), || Err(SecretStoreError::BackendFailure)),
            Err(SecretStoreError::BackendFailure)
        );
        assert_eq!(install_headless_fence(|| Ok(()), || Ok(false)), Ok(()));
    }
    #[test]
    fn locked_missing_and_ambiguous_searches_never_read_or_unlock() {
        for (locked, unlocked, error) in [
            (1, vec![], SecretStoreError::LockedOrUnavailable),
            (1, vec![1], SecretStoreError::LockedOrUnavailable),
            (0, vec![], SecretStoreError::Missing),
            (0, vec![1, 2], SecretStoreError::BackendFailure),
        ] {
            assert!(
                matches!(unlocked_result(locked,unlocked,|_|panic!("no read/unlock/prompt allowed")),Err(e) if e==error)
            );
        }
    }
    #[test]
    fn unlocked_read_is_fresh_and_lock_race_never_retries() {
        let reads = std::cell::Cell::new(0);
        assert!(matches!(
            unlocked_result(0, vec![1], |_| {
                reads.set(reads.get() + 1);
                Err(SecretStoreError::LockedOrUnavailable)
            }),
            Err(SecretStoreError::LockedOrUnavailable)
        ));
        assert_eq!(reads.get(), 1);
        let secret = unlocked_result(0, vec![1], |_| Ok(vec![7])).unwrap();
        secret.with_bytes(|v| assert_eq!(v, [7]));
        assert!(matches!(
            unlocked_result(0, vec![1], |_| Ok(vec![0; MAX_SECRET_BYTES + 1])),
            Err(SecretStoreError::TooLarge)
        ));
    }
    #[test]
    fn mac_skip_empty_ambiguity_shape_and_size_fail_closed() {
        assert!(matches!(
            mac_results(vec![]),
            Err(SecretStoreError::LockedOrUnavailable)
        ));
        assert!(matches!(
            mac_results(vec![Some(vec![1]), Some(vec![2])]),
            Err(SecretStoreError::BackendFailure)
        ));
        assert!(matches!(
            mac_results(vec![Some(vec![1]), None]),
            Err(SecretStoreError::BackendFailure)
        ));
        assert!(matches!(
            mac_results(vec![Some(vec![0; MAX_SECRET_BYTES + 1])]),
            Err(SecretStoreError::TooLarge)
        ));
        mac_results(vec![Some(vec![1])])
            .unwrap()
            .with_bytes(|v| assert_eq!(v, [1]));
    }
}
