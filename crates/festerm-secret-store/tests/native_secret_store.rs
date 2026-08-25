use std::env;

use festerm_secret_store::{
    native_store, SecretBytes, SecretReference, SecretStore, SecretStoreError,
};

const NATIVE_TEST_ENV: &str = "FESTERM_RUN_NATIVE_SECRET_STORE_TESTS";

struct CreatedEntry<'a> {
    store: &'a dyn SecretStore,
    persisted_reference: String,
    armed: bool,
}

impl<'a> CreatedEntry<'a> {
    fn new(store: &'a dyn SecretStore, reference: &SecretReference) -> Self {
        Self {
            store,
            persisted_reference: reference.to_persisted_string(),
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CreatedEntry<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }

        if let Ok(reference) = SecretReference::parse(&self.persisted_reference) {
            let _ = self.store.delete(&reference);
        }
    }
}

fn native_test_enabled() -> bool {
    matches!(env::var(NATIVE_TEST_ENV).as_deref(), Ok("1"))
}

fn disposable_value(stage: &str) -> Vec<u8> {
    format!(
        "festerm-native-secret-store-test:{stage}:{}",
        SecretReference::generate().to_persisted_string()
    )
    .into_bytes()
}

fn panic_native_error(operation: &str, error: SecretStoreError) -> ! {
    match error {
        SecretStoreError::LockedOrUnavailable => {
            panic!("{operation}: native secret store is locked or unavailable")
        }
        SecretStoreError::Unsupported => panic!("{operation}: native secret store is unsupported"),
        SecretStoreError::Missing => {
            panic!("{operation}: disposable native secret-store entry is unexpectedly missing")
        }
        SecretStoreError::BackendFailure => {
            panic!("{operation}: native secret-store backend failed")
        }
        SecretStoreError::InvalidReference => {
            panic!("{operation}: generated native secret-store reference was invalid")
        }
    }
}

fn require_native_result<T>(operation: &str, result: Result<T, SecretStoreError>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic_native_error(operation, error),
    }
}

fn require_missing(operation: &str, result: Result<SecretBytes, SecretStoreError>) {
    match result {
        Err(SecretStoreError::Missing) => {}
        Ok(_) => panic!("{operation}: deleted disposable entry still returned a secret"),
        Err(error) => panic_native_error(operation, error),
    }
}

#[test]
#[ignore = "may prompt or require a desktop secret service; set FESTERM_RUN_NATIVE_SECRET_STORE_TESTS=1"]
fn native_store_runs_a_disposable_put_get_update_delete_lifecycle() {
    if !native_test_enabled() {
        eprintln!("skipped: set {NATIVE_TEST_ENV}=1 to opt in to native secret-store integration");
        return;
    }

    let store = require_native_result("create store", native_store());
    let original = disposable_value("original");
    let updated = disposable_value("updated");
    let original_secret = SecretBytes::copy_from_slice(&original);
    let reference = require_native_result("put", store.put(&original_secret));
    let mut cleanup = CreatedEntry::new(store.as_ref(), &reference);

    let retrieved = require_native_result("get after put", store.get(&reference));
    retrieved.with_bytes(|bytes| assert_eq!(bytes, original.as_slice()));

    let updated_secret = SecretBytes::copy_from_slice(&updated);
    require_native_result("update", store.update(&reference, &updated_secret));
    let retrieved = require_native_result("get after update", store.get(&reference));
    retrieved.with_bytes(|bytes| assert_eq!(bytes, updated.as_slice()));

    assert!(
        require_native_result("delete", store.delete(&reference)),
        "delete must remove the disposable entry"
    );

    require_missing("get after delete", store.get(&reference));
    assert!(
        !require_native_result("repeat delete", store.delete(&reference)),
        "repeat delete must report that the disposable entry is absent"
    );
    cleanup.disarm();
}

#[cfg(not(any(target_os = "macos", windows, all(unix, not(target_os = "macos")))))]
#[test]
fn unsupported_targets_report_unsupported_explicitly() {
    assert!(matches!(native_store(), Err(SecretStoreError::Unsupported)));
}
