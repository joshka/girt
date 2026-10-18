//! The upload-pack protocol version a client asks a server to speak.

/// Upload-pack protocol version requested from an HTTP or SSH server.
///
/// See [Protocol versions](crate::transport#protocol-versions) for what each version changes.
///
/// A request is a preference, not a guarantee: the server chooses, and girt parses whatever its
/// first response line announces. Requesting v2 from a server that only knows v0/v1 therefore
/// falls back to v0/v1. Requesting v0 or v1 from a server that answers `version 2` fails. Only
/// upload-pack (fetch and discovery) sends the request; receive-pack (push) has no v2 and always
/// speaks v0/v1.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum RequestedProtocol {
    /// Send no version request; the server speaks v0.
    #[default]
    V0,
    /// Request v1, a version marker before a v0 advertisement.
    V1,
    /// Request v2, falling back to the server's v0/v1 when it does not support v2.
    V2,
}

impl RequestedProtocol {
    /// Parses a Git `protocol.version` value: `0`, `1` or `2`.
    ///
    /// Returns `None` for any other value, including surrounding whitespace.
    ///
    /// # Examples
    ///
    /// ```
    /// use girt::Config;
    /// use girt::transport::RequestedProtocol;
    ///
    /// let config = Config::parse(b"[protocol]\nversion = 1\n")?;
    /// let requested = match config.value("protocol", None, "version") {
    ///     // Unset: Git requests version 2.
    ///     None => RequestedProtocol::V2,
    ///     Some(value) => value
    ///         .and_then(RequestedProtocol::from_config)
    ///         .ok_or("invalid protocol.version")?,
    /// };
    /// assert_eq!(requested, RequestedProtocol::V1);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn from_config(value: &[u8]) -> Option<Self> {
        match value {
            b"0" => Some(Self::V0),
            b"1" => Some(Self::V1),
            b"2" => Some(Self::V2),
            _ => None,
        }
    }

    /// The value Git sends as `GIT_PROTOCOL` (over SSH) or `Git-Protocol` (over HTTP), such as
    /// `version=2`, or `None` for v0.
    ///
    /// An application [`Transport`](super::Transport) passes it to its service the same way.
    pub fn git_protocol(self) -> Option<&'static str> {
        self.parameter()
    }

    pub(crate) fn parameter(self) -> Option<&'static str> {
        match self {
            Self::V0 => None,
            Self::V1 => Some("version=1"),
            Self::V2 => Some("version=2"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_exact_config_values() {
        assert_eq!(
            RequestedProtocol::from_config(b"2"),
            Some(RequestedProtocol::V2)
        );
        assert_eq!(
            RequestedProtocol::from_config(b"0"),
            Some(RequestedProtocol::V0)
        );
        assert_eq!(RequestedProtocol::from_config(b"3"), None);
        assert_eq!(RequestedProtocol::from_config(b" 2"), None);
        assert_eq!(RequestedProtocol::V0.parameter(), None);
        assert_eq!(RequestedProtocol::V2.parameter(), Some("version=2"));
    }
}
