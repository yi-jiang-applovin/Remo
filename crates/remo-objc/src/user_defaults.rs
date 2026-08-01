//! `NSUserDefaults` access — real key/value store debugging, generic to any
//! iOS (or macOS) app, not something an app has to register a capability
//! for itself.
//!
//! Foundation-only (`NSUserDefaults`, `NSString`, `NSNumber`, `NSArray`,
//! `NSDictionary`, `NSData`) — no UIKit involved, so this is gated on
//! `target_vendor = "apple"` alone, not the `uikit` feature (matching
//! `main_thread.rs`'s own reasoning: GCD dispatch is Apple-general, not
//! iOS-specific). That is what lets this be exercised for real against a
//! bare macOS dev machine's own `NSUserDefaults` (the standalone example,
//! `cargo test`) — unlike the UIKit-touching capabilities, which only ever
//! run their stub path outside a real iOS process.

use serde_json::Value;

#[cfg(target_vendor = "apple")]
mod apple {
    use super::*;
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_foundation::NSString;

    unsafe fn nsstring_to_string(obj: *mut AnyObject) -> Option<String> {
        if obj.is_null() {
            return None;
        }
        let ns: &NSString = &*(obj as *const NSString);
        Some(ns.to_string())
    }

    unsafe fn is_kind_of(obj: *mut AnyObject, class_name: &std::ffi::CStr) -> bool {
        let Some(class) = objc2::runtime::AnyClass::get(class_name) else {
            return false;
        };
        msg_send![obj, isKindOfClass: class]
    }

    /// Converts an arbitrary property-list object (what every
    /// `NSUserDefaults` value already is — `NSString`/`NSNumber`/`NSArray`/
    /// `NSDictionary`/`NSData`/`NSNull`, recursively) into a JSON value.
    ///
    /// # Safety
    /// `obj` must be a valid ObjC object pointer or null.
    unsafe fn object_to_json(obj: *mut AnyObject) -> Value {
        if obj.is_null() {
            return Value::Null;
        }
        if is_kind_of(obj, c"NSString") {
            return Value::String(nsstring_to_string(obj).unwrap_or_default());
        }
        if is_kind_of(obj, c"NSNumber") {
            // NSNumber's objCType discriminates bool ('c'/'B') from other
            // numeric encodings — without this, `true` round-trips as `1`.
            let type_ptr: *const std::os::raw::c_char = msg_send![obj, objCType];
            let type_code = if type_ptr.is_null() {
                0u8
            } else {
                *(type_ptr as *const u8)
            };
            if type_code == b'c' || type_code == b'B' {
                let b: bool = msg_send![obj, boolValue];
                return Value::Bool(b);
            }
            let d: f64 = msg_send![obj, doubleValue];
            if d.fract() == 0.0 && d.abs() < 1e15 {
                return Value::Number((d as i64).into());
            }
            return serde_json::Number::from_f64(d)
                .map(Value::Number)
                .unwrap_or(Value::Null);
        }
        if is_kind_of(obj, c"NSArray") {
            let count: usize = msg_send![obj, count];
            let mut items = Vec::with_capacity(count);
            for i in 0..count {
                let item: *mut AnyObject = msg_send![obj, objectAtIndex: i];
                items.push(object_to_json(item));
            }
            return Value::Array(items);
        }
        if is_kind_of(obj, c"NSDictionary") {
            let keys: *mut AnyObject = msg_send![obj, allKeys];
            let count: usize = msg_send![keys, count];
            let mut map = serde_json::Map::new();
            for i in 0..count {
                let key: *mut AnyObject = msg_send![keys, objectAtIndex: i];
                let key_str = nsstring_to_string(key).unwrap_or_default();
                let value: *mut AnyObject = msg_send![obj, objectForKey: key];
                map.insert(key_str, object_to_json(value));
            }
            return Value::Object(map);
        }
        if is_kind_of(obj, c"NSData") {
            let length: usize = msg_send![obj, length];
            // `-[NSData bytes]` returns `const void *` (type code `^v`) —
            // typing this as `*const u8` directly makes objc2's runtime
            // type-check panic on a real call (found against this machine's
            // own actual NSUserDefaults domain, not a hypothetical).
            let bytes_ptr: *const std::ffi::c_void = msg_send![obj, bytes];
            let slice = if bytes_ptr.is_null() || length == 0 {
                &[][..]
            } else {
                std::slice::from_raw_parts(bytes_ptr.cast::<u8>(), length)
            };
            use base64::Engine;
            return Value::String(base64::engine::general_purpose::STANDARD.encode(slice));
        }
        // NSNull or anything unrecognized — description string is better
        // than silently dropping the value.
        let desc: *mut AnyObject = msg_send![obj, description];
        nsstring_to_string(desc).map_or(Value::Null, Value::String)
    }

    /// Converts a JSON value into a property-list ObjC object suitable for
    /// `NSUserDefaults setObject:forKey:`. `Value::Null` isn't representable
    /// (there is no "nil in a dictionary" in a property list) — callers that
    /// want to remove a key should call `delete`, not `set` with `null`.
    ///
    /// # Safety
    /// The returned pointer is a valid, retained-by-autorelease-pool ObjC
    /// object for the duration of the caller's use.
    unsafe fn json_to_object(value: &Value) -> *mut AnyObject {
        match value {
            Value::Null => std::ptr::null_mut(),
            Value::Bool(b) => {
                let n: *mut AnyObject = msg_send![objc2::class!(NSNumber), numberWithBool: *b];
                n
            }
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    msg_send![objc2::class!(NSNumber), numberWithLongLong: i]
                } else {
                    let d = n.as_f64().unwrap_or(0.0);
                    msg_send![objc2::class!(NSNumber), numberWithDouble: d]
                }
            }
            Value::String(s) => {
                let ns = NSString::from_str(s);
                let obj: *mut AnyObject = &*ns as *const NSString as *mut AnyObject;
                obj
            }
            Value::Array(items) => {
                let array: *mut AnyObject =
                    msg_send![objc2::class!(NSMutableArray), arrayWithCapacity: items.len()];
                for item in items {
                    let obj = json_to_object(item);
                    if !obj.is_null() {
                        let _: () = msg_send![array, addObject: obj];
                    }
                }
                array
            }
            Value::Object(map) => {
                let dict: *mut AnyObject = msg_send![objc2::class!(NSMutableDictionary), dictionaryWithCapacity: map.len()];
                for (key, val) in map {
                    let obj = json_to_object(val);
                    if !obj.is_null() {
                        let key_ns = NSString::from_str(key);
                        let key_obj: *mut AnyObject = &*key_ns as *const NSString as *mut AnyObject;
                        let _: () = msg_send![dict, setObject: obj, forKey: key_obj];
                    }
                }
                dict
            }
        }
    }

    fn standard_defaults() -> *mut AnyObject {
        // SAFETY: NSUserDefaults's class is always available in any process
        // linking Foundation; +standardUserDefaults never returns nil.
        unsafe { msg_send![objc2::class!(NSUserDefaults), standardUserDefaults] }
    }

    /// # Safety
    /// Foundation calls are safe to make from any thread for this specific
    /// class (`NSUserDefaults` is documented thread-safe), so unlike the
    /// UIKit-touching capabilities this does not require `run_on_main_sync`.
    pub unsafe fn list() -> Vec<(String, Value)> {
        let defaults = standard_defaults();
        let dict: *mut AnyObject = msg_send![defaults, dictionaryRepresentation];
        let keys: *mut AnyObject = msg_send![dict, allKeys];
        let count: usize = msg_send![keys, count];
        let mut entries = Vec::with_capacity(count);
        for i in 0..count {
            let key: *mut AnyObject = msg_send![keys, objectAtIndex: i];
            let key_str = nsstring_to_string(key).unwrap_or_default();
            let value: *mut AnyObject = msg_send![dict, objectForKey: key];
            entries.push((key_str, object_to_json(value)));
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries
    }

    /// # Safety
    /// See [`list`].
    pub unsafe fn get(key: &str) -> Option<Value> {
        let defaults = standard_defaults();
        let key_ns = NSString::from_str(key);
        let key_obj: *mut AnyObject = &*key_ns as *const NSString as *mut AnyObject;
        let value: *mut AnyObject = msg_send![defaults, objectForKey: key_obj];
        if value.is_null() {
            None
        } else {
            Some(object_to_json(value))
        }
    }

    /// # Safety
    /// See [`list`].
    pub unsafe fn set(key: &str, value: &Value) -> Result<(), String> {
        if value.is_null() {
            return Err("cannot set null — use delete to remove a key".to_string());
        }
        let defaults = standard_defaults();
        let key_ns = NSString::from_str(key);
        let key_obj: *mut AnyObject = &*key_ns as *const NSString as *mut AnyObject;
        let value_obj = json_to_object(value);
        let _: () = msg_send![defaults, setObject: value_obj, forKey: key_obj];
        Ok(())
    }

    /// # Safety
    /// See [`list`].
    pub unsafe fn delete(key: &str) {
        let defaults = standard_defaults();
        let key_ns = NSString::from_str(key);
        let key_obj: *mut AnyObject = &*key_ns as *const NSString as *mut AnyObject;
        let _: () = msg_send![defaults, removeObjectForKey: key_obj];
    }
}

#[cfg(not(target_vendor = "apple"))]
mod apple {
    use super::*;

    pub unsafe fn list() -> Vec<(String, Value)> {
        tracing::warn!("user_defaults::list called on non-Apple target, returning empty");
        Vec::new()
    }

    pub unsafe fn get(_key: &str) -> Option<Value> {
        tracing::warn!("user_defaults::get called on non-Apple target, returning None");
        None
    }

    pub unsafe fn set(_key: &str, _value: &Value) -> Result<(), String> {
        tracing::warn!("user_defaults::set called on non-Apple target, no-op");
        Ok(())
    }

    pub unsafe fn delete(_key: &str) {
        tracing::warn!("user_defaults::delete called on non-Apple target, no-op");
    }
}

/// Every key currently in `NSUserDefaults`' standard domain, as `(key,
/// json_value)` pairs sorted by key. Includes anything registered via
/// `registerDefaults:` as well as anything actually set.
///
/// # Safety
/// Apple platforms only guarantee `NSUserDefaults` is thread-safe; this
/// still requires a linked Foundation runtime (always true on any Apple
/// target this crate builds for).
pub unsafe fn list_user_defaults() -> Vec<(String, Value)> {
    apple::list()
}

/// The value for `key`, or `None` if unset.
///
/// # Safety
/// See [`list_user_defaults`].
pub unsafe fn get_user_default(key: &str) -> Option<Value> {
    apple::get(key)
}

/// Sets `key` to `value` (must not be `Value::Null` — see
/// [`delete_user_default`] to remove a key instead).
///
/// # Safety
/// See [`list_user_defaults`].
pub unsafe fn set_user_default(key: &str, value: &Value) -> Result<(), String> {
    apple::set(key, value)
}

/// Removes `key` entirely.
///
/// # Safety
/// See [`list_user_defaults`].
pub unsafe fn delete_user_default(key: &str) {
    apple::delete(key);
}

#[cfg(all(test, target_vendor = "apple"))]
mod tests {
    //! Runs against this machine's *real* `NSUserDefaults` — Foundation-only
    //! code needs no iOS simulator/device to exercise for real, unlike the
    //! UIKit-touching capabilities elsewhere in this crate (see the module
    //! doc for why). Every key is prefixed and cleaned up so a test run
    //! never leaves stray state behind.
    use super::*;
    use serde_json::json;

    fn test_key(name: &str) -> String {
        format!("remo.remo-objc-tests.{name}")
    }

    // SAFETY: every test below just exercises this crate's own public API
    // against real NSUserDefaults, per its documented contract.

    #[test]
    fn string_round_trips() {
        let key = test_key("string_round_trips");
        // SAFETY: exercising this crate's own public API against real NSUserDefaults.
        unsafe {
            set_user_default(&key, &json!("hello")).unwrap();
            assert_eq!(get_user_default(&key), Some(json!("hello")));
            delete_user_default(&key);
            assert_eq!(get_user_default(&key), None);
        }
    }

    #[test]
    fn bool_round_trips_as_bool_not_number() {
        let key = test_key("bool_round_trips_as_bool_not_number");
        // SAFETY: exercising this crate's own public API against real NSUserDefaults.
        unsafe {
            set_user_default(&key, &json!(true)).unwrap();
            assert_eq!(get_user_default(&key), Some(json!(true)));
            delete_user_default(&key);
        }
    }

    #[test]
    fn integer_round_trips() {
        let key = test_key("integer_round_trips");
        // SAFETY: exercising this crate's own public API against real NSUserDefaults.
        unsafe {
            set_user_default(&key, &json!(42)).unwrap();
            assert_eq!(get_user_default(&key), Some(json!(42)));
            delete_user_default(&key);
        }
    }

    #[test]
    fn float_round_trips() {
        let key = test_key("float_round_trips");
        // SAFETY: exercising this crate's own public API against real NSUserDefaults.
        unsafe {
            set_user_default(&key, &json!(3.5)).unwrap();
            assert_eq!(get_user_default(&key), Some(json!(3.5)));
            delete_user_default(&key);
        }
    }

    #[test]
    fn array_and_object_round_trip() {
        let key = test_key("array_and_object_round_trip");
        let value = json!({"names": ["a", "b"], "count": 2});
        // SAFETY: exercising this crate's own public API against real NSUserDefaults.
        unsafe {
            set_user_default(&key, &value).unwrap();
            assert_eq!(get_user_default(&key), Some(value));
            delete_user_default(&key);
        }
    }

    #[test]
    fn set_null_is_rejected_not_silently_ignored() {
        let key = test_key("set_null_is_rejected_not_silently_ignored");
        // SAFETY: exercising this crate's own public API against real NSUserDefaults.
        unsafe {
            let result = set_user_default(&key, &Value::Null);
            assert!(result.is_err());
        }
    }

    #[test]
    fn list_includes_a_freshly_set_key() {
        let key = test_key("list_includes_a_freshly_set_key");
        // SAFETY: exercising this crate's own public API against real NSUserDefaults.
        unsafe {
            set_user_default(&key, &json!("visible")).unwrap();
            let all = list_user_defaults();
            assert!(all.iter().any(|(k, v)| k == &key && v == &json!("visible")));
            delete_user_default(&key);
        }
    }

    #[test]
    fn delete_then_get_is_none() {
        let key = test_key("delete_then_get_is_none");
        // SAFETY: exercising this crate's own public API against real NSUserDefaults.
        unsafe {
            set_user_default(&key, &json!("temp")).unwrap();
            delete_user_default(&key);
            assert_eq!(get_user_default(&key), None);
        }
    }
}
