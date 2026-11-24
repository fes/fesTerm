// Registry reads have their own 500 ms lock deadline. Only that exact native
// contention result is an unavailable observation, never an empty inventory.
pub(super) fn registry_observation<T>(
    result: Result<T, festerm_sessiond::PersistentSessionError>,
) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error)
            if error.to_string()
                == format!(
                    "session registry unavailable (lock deadline 500 ms): {}",
                    fs2::lock_contended_error()
                ) =>
        {
            None
        }
        Err(error) => panic!("registry observation failed: {error}"),
    }
}
