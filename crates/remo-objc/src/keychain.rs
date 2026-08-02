//! Keychain generic-password access — real credential-store debugging,
//! generic to any iOS (or macOS) app, not something an app has to register a
//! capability for itself. Mirrors `user_defaults.rs`'s shape: `list`/`get`/
//! `set`/`delete` over a single storage domain, keyed here by `(service,
//! account)` rather than a single string key, since that's how
//! `kSecClassGenericPassword` items are actually identified.
//!
//! Unlike `NSUserDefaults`, the Keychain has no bulk "give me every value"
//! call — listing means searching for every `kSecClassGenericPassword` item
//! with attributes loaded, which only yields metadata (service/account/label/
//! ...), not the secret data itself (that needs a second, targeted query per
//! item, which `list` deliberately avoids doing automatically — see `list`'s
//! doc comment).

use serde_json::Value;

#[cfg(target_vendor = "apple")]
mod apple {
    use super::*;
    use security_framework::item::{ItemClass, ItemSearchOptions, Limit, SearchResult};
    use security_framework::passwords::{
        delete_generic_password, get_generic_password, set_generic_password,
    };

    /// Every generic-password item currently in the keychain, as attribute
    /// maps (service/account/label/... — whatever Security.framework
    /// reports). Deliberately does not include the secret value: bulk-reading
    /// every password's plaintext just to list what exists is a needless
    /// blast radius `get` (given a specific service/account) already covers.
    pub fn list() -> Result<Vec<Value>, String> {
        let results = ItemSearchOptions::new()
            .class(ItemClass::generic_password())
            .load_attributes(true)
            .limit(Limit::All)
            .search();
        let results = match results {
            Ok(results) => results,
            // errSecItemNotFound just means an empty keychain domain, not a
            // real failure — same as user_defaults::list returning empty.
            Err(e) if e.code() == security_framework_sys::base::errSecItemNotFound => {
                return Ok(Vec::new())
            }
            Err(e) => return Err(e.to_string()),
        };
        Ok(results
            .iter()
            .filter_map(SearchResult::simplify_dict)
            .map(|attrs| {
                let map: serde_json::Map<String, Value> = attrs
                    .into_iter()
                    .map(|(k, v)| (k, Value::String(v)))
                    .collect();
                Value::Object(map)
            })
            .collect())
    }

    /// The password bytes for `service`/`account`, or `None` if no such item
    /// exists. Returned as a UTF-8 string when valid, else base64 — most
    /// generic passwords are text, but the Keychain doesn't guarantee it.
    pub fn get(service: &str, account: &str) -> Result<Option<Value>, String> {
        match get_generic_password(service, account) {
            Ok(bytes) => Ok(Some(bytes_to_json(&bytes))),
            Err(e) if e.code() == security_framework_sys::base::errSecItemNotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Sets (creating or overwriting) the password for `service`/`account`.
    pub fn set(service: &str, account: &str, password: &str) -> Result<(), String> {
        set_generic_password(service, account, password.as_bytes()).map_err(|e| e.to_string())
    }

    /// Deletes the item for `service`/`account`. A missing item is not an
    /// error — matches `userDefaults.delete`'s "already gone is fine" shape.
    pub fn delete(service: &str, account: &str) -> Result<(), String> {
        match delete_generic_password(service, account) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == security_framework_sys::base::errSecItemNotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn bytes_to_json(bytes: &[u8]) -> Value {
        match std::str::from_utf8(bytes) {
            Ok(s) => Value::String(s.to_string()),
            Err(_) => {
                use base64::Engine;
                Value::String(base64::engine::general_purpose::STANDARD.encode(bytes))
            }
        }
    }
}

#[cfg(not(target_vendor = "apple"))]
mod apple {
    use super::*;

    pub fn list() -> Result<Vec<Value>, String> {
        tracing::warn!("keychain::list called on non-Apple target, returning empty");
        Ok(Vec::new())
    }

    pub fn get(_service: &str, _account: &str) -> Result<Option<Value>, String> {
        tracing::warn!("keychain::get called on non-Apple target, returning None");
        Ok(None)
    }

    pub fn set(_service: &str, _account: &str, _password: &str) -> Result<(), String> {
        tracing::warn!("keychain::set called on non-Apple target, no-op");
        Ok(())
    }

    pub fn delete(_service: &str, _account: &str) -> Result<(), String> {
        tracing::warn!("keychain::delete called on non-Apple target, no-op");
        Ok(())
    }
}

/// Attribute maps for every generic-password keychain item — see
/// [`apple::list`] for why secret values are never included in bulk.
pub fn list_keychain_items() -> Result<Vec<Value>, String> {
    apple::list()
}

/// The password for `service`/`account`, or `None` if no such item exists.
pub fn get_keychain_item(service: &str, account: &str) -> Result<Option<Value>, String> {
    apple::get(service, account)
}

/// Sets (creating or overwriting) the password for `service`/`account`.
pub fn set_keychain_item(service: &str, account: &str, password: &str) -> Result<(), String> {
    apple::set(service, account, password)
}

/// Removes the item for `service`/`account`, if present.
pub fn delete_keychain_item(service: &str, account: &str) -> Result<(), String> {
    apple::delete(service, account)
}

#[cfg(all(test, target_vendor = "apple"))]
mod tests {
    //! Runs against this machine's *real* keychain — Security.framework
    //! needs no iOS simulator/device to exercise for real, same reasoning as
    //! `user_defaults.rs`'s tests. Every item is distinctly prefixed and
    //! cleaned up so a test run never leaves stray state behind.
    use super::*;

    fn test_service(name: &str) -> String {
        format!("remo.remo-objc-tests.{name}")
    }

    #[test]
    fn string_round_trips() {
        let service = test_service("string_round_trips");
        set_keychain_item(&service, "acct", "hello").unwrap();
        assert_eq!(
            get_keychain_item(&service, "acct").unwrap(),
            Some(Value::String("hello".to_string()))
        );
        delete_keychain_item(&service, "acct").unwrap();
        assert_eq!(get_keychain_item(&service, "acct").unwrap(), None);
    }

    #[test]
    fn set_then_set_again_overwrites_not_duplicates() {
        let service = test_service("set_then_set_again_overwrites_not_duplicates");
        set_keychain_item(&service, "acct", "first").unwrap();
        set_keychain_item(&service, "acct", "second").unwrap();
        assert_eq!(
            get_keychain_item(&service, "acct").unwrap(),
            Some(Value::String("second".to_string()))
        );
        delete_keychain_item(&service, "acct").unwrap();
    }

    #[test]
    fn delete_then_get_is_none() {
        let service = test_service("delete_then_get_is_none");
        set_keychain_item(&service, "acct", "temp").unwrap();
        delete_keychain_item(&service, "acct").unwrap();
        assert_eq!(get_keychain_item(&service, "acct").unwrap(), None);
    }

    #[test]
    fn delete_of_missing_item_is_not_an_error() {
        let service = test_service("delete_of_missing_item_is_not_an_error");
        delete_keychain_item(&service, "acct").unwrap();
    }

    #[test]
    fn list_includes_a_freshly_set_item() {
        let service = test_service("list_includes_a_freshly_set_item");
        set_keychain_item(&service, "acct", "visible").unwrap();

        // securityd serializes concurrent keychain access across the whole
        // test binary, so a `kSecMatchLimitAll` search running alongside
        // other tests' own adds/deletes can occasionally observe a
        // transient snapshot that hasn't caught up yet — retry briefly
        // rather than flake. `string_round_trips`/etc. don't need this
        // because get/delete target one exact item, not a full-table scan.
        let mut found = false;
        for _ in 0..20 {
            let all = list_keychain_items().unwrap();
            found = all.iter().any(|item| {
                item.get("svce")
                    .and_then(Value::as_str)
                    .map(|s| s == service)
                    .unwrap_or(false)
            });
            if found {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(found, "expected {service} to appear in keychain.list");

        delete_keychain_item(&service, "acct").unwrap();
    }
}
