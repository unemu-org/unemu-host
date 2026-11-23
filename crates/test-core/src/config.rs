// SPDX-License-Identifier: AGPL-3.0-or-later
//! The config document `identify` writes and `validate` and `create` read:
//! the script by digest and size, never by name.
//!
//! ```text
//! unemu-test-core-config 1
//! script 64-HEX-DIGITS SIZE
//! ```

use crate::script::hex_value;
use unemu_core_api::{ConfigDocument, ConfigDocumentBuf, Diagnostic, Error, ErrorKind};

/// The media type of the config document.
pub const CONFIG_TYPE: &str = "application/vnd.unemu.test-core.config";

const FIRST_LINE: &str = "unemu-test-core-config 1";

/// The script a config names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ScriptRef {
    pub sha256: [u8; 32],
    pub size: u64,
}

/// Lower-case hex digits of a digest.
pub(crate) fn hex(digest: &[u8; 32]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(64);
    for byte in digest {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
    text
}

/// The config of a machine that runs this script.
pub(crate) fn write(script: ScriptRef) -> ConfigDocumentBuf {
    ConfigDocumentBuf::new(
        CONFIG_TYPE,
        format!(
            "{FIRST_LINE}\nscript {} {}\n",
            hex(&script.sha256),
            script.size
        )
        .into_bytes(),
    )
}

fn refuse(line: usize, message: &str) -> Error {
    let path = format!("line {line}");
    Error::new(ErrorKind::Config, format!("config {path}: {message}"))
        .with_diagnostics(vec![Diagnostic::new(path, message)])
}

/// Read a config: `Unsupported` for another media type, `Config` with a
/// diagnostic naming the line for anything malformed.
pub(crate) fn read(config: ConfigDocument<'_>) -> Result<ScriptRef, Error> {
    if config.media_type != CONFIG_TYPE {
        return Err(Error::new(
            ErrorKind::Unsupported,
            format!(
                "the test core reads configs of type {CONFIG_TYPE}, not {}",
                config.media_type
            ),
        ));
    }
    let text =
        std::str::from_utf8(config.bytes).map_err(|_| refuse(1, "the config is not UTF-8 text"))?;
    let mut lines = text.lines();
    if lines.next() != Some(FIRST_LINE) {
        return Err(refuse(
            1,
            "the first line is not `unemu-test-core-config 1`",
        ));
    }
    let line = lines
        .next()
        .ok_or_else(|| refuse(2, "the line `script SHA256 SIZE` is missing"))?;
    let words: Vec<&str> = line.split(' ').collect();
    let script = match words.as_slice() {
        ["script", digest, size] => {
            let sha256 = digest_of(digest)
                .ok_or_else(|| refuse(2, "the script's SHA-256 is not 64 lower-case hex digits"))?;
            let size = size
                .parse()
                .map_err(|_| refuse(2, "the script's size is not a number"))?;
            ScriptRef { sha256, size }
        }
        _ => return Err(refuse(2, "expected `script SHA256 SIZE`")),
    };
    if let Some(extra) = lines.next() {
        if !extra.is_empty() || lines.next().is_some() {
            return Err(refuse(3, "nothing follows the script line"));
        }
    }
    Ok(script)
}

fn digest_of(word: &str) -> Option<[u8; 32]> {
    let bytes = word.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let mut digest = [0u8; 32];
    for (index, pair) in bytes.chunks(2).enumerate() {
        digest[index] = hex_value(pair[0])? << 4 | hex_value(pair[1])?;
    }
    Some(digest)
}
