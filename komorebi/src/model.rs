use std::fmt;
use std::ops::Deref;

use nanoid::nanoid;
use serde::Deserialize;
use serde::Serialize;

macro_rules! stable_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        #[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
        pub struct $name(String);

        impl $name {
            pub fn new() -> Self {
                Self(nanoid!())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn is_empty(&self) -> bool {
                self.0.is_empty()
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_string())
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.as_str() == *other
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl Deref for $name {
            type Target = str;

            fn deref(&self) -> &Self::Target {
                self.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

stable_id!(WorkspaceId);
stable_id!(ContainerId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_transparent_strings_in_state_json() {
        let id = WorkspaceId::from("workspace-1");
        let json = serde_json::to_string(&id).unwrap();

        assert_eq!(json, r#""workspace-1""#);
        assert_eq!(serde_json::from_str::<WorkspaceId>(&json).unwrap(), id);
    }

    #[test]
    fn new_ids_are_nonempty_and_distinct() {
        let first = ContainerId::new();
        let second = ContainerId::new();

        assert!(!first.is_empty());
        assert_ne!(first, second);
    }
}
