pub mod diff;
pub mod edit;
pub mod merge;
pub mod merge_advice;
pub mod structural;
pub mod syntax;
pub mod text;
pub mod unified;

use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct TextSnapshot {
    text: Arc<str>,
    identity: Arc<()>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum TextError {
    InvalidUtf8,
    BinaryInput,
}

impl TextSnapshot {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TextError> {
        let text = std::str::from_utf8(bytes).map_err(|_| TextError::InvalidUtf8)?;
        if bytes.contains(&0) {
            return Err(TextError::BinaryInput);
        }
        Ok(Self::from_owned(text.to_owned()))
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.text.as_bytes()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn from_owned(text: String) -> Self {
        Self {
            text: Arc::from(text),
            identity: Arc::new(()),
        }
    }

    pub(crate) fn from_owned_with_identity(text: String, identity: Arc<()>) -> Self {
        Self {
            text: Arc::from(text),
            identity,
        }
    }

    pub fn same_identity(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.identity, &other.identity)
    }
}
