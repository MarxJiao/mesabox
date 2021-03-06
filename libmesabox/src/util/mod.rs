//
// Copyright (c) 2018, The MesaLock Linux Project Contributors
// All rights reserved.
//
// This work is licensed under the terms of the BSD 3-Clause License.
// For a copy, see the LICENSE file.
//

pub use self::platform::{is_tty, AsRawObject, OsStrExt, Pipe, RawObject, RawObjectWrapper};
use super::{LockableRead, LockableWrite, MesaError, Result};

use failure;
use std::borrow::Cow;
use std::error::Error as StdError;
use std::io::{self, Read, Write};
use std::path::Path;
use std::result::Result as StdResult;
use std::str::FromStr;

mod platform;

pub const EXIT_SUCCESS: ExitCode = 0;
pub const EXIT_FAILURE: ExitCode = 1;

// defined out here rather than in parse_num_with_suffix() because we need the array for testing
const SUFFIXES: [char; 8] = ['K', 'M', 'G', 'T', 'P', 'E', 'Z', 'Y'];
const OBSOLETE_SUFFIXES: [char; 2] = ['k', 'm'];

pub type ExitCode = i32;

/// Wrapper around a `Vec<T>` to make it readable (using the standard `Read` trait).
pub struct ReadableVec<T>(pub Vec<T>);

impl Read for ReadableVec<u8> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&self.0[..]).read(buf)
    }
}

/// A structure to enable using dynamic dispatch with an object that implements
/// [UtilRead](../trait.UtilRead.html).
pub struct UtilReadDyn {
    pub(crate) inner: Box<for<'a> LockableRead<'a>>,
    fd: Option<RawObject>,
}

impl UtilReadDyn {
    pub fn new(inner: Box<for<'a> LockableRead<'a>>, fd: Option<RawObject>) -> Self {
        Self {
            inner: inner,
            fd: fd,
        }
    }

    pub fn fd(&self) -> Option<RawObject> {
        self.fd
    }
}

/// A structure to enable using dynamic dispatch with an object that implements
/// [UtilWrite](../trait.UtilWrite.html).
pub struct UtilWriteDyn {
    pub(crate) inner: Box<for<'a> LockableWrite<'a>>,
    fd: Option<RawObject>,
}

impl UtilWriteDyn {
    pub fn new(inner: Box<for<'a> LockableWrite<'a>>, fd: Option<RawObject>) -> Self {
        Self {
            inner: inner,
            fd: fd,
        }
    }

    pub fn fd(&self) -> Option<RawObject> {
        self.fd
    }
}

impl Read for UtilReadDyn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Write for UtilWriteDyn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

pub(crate) struct ExitCodeWrapper(pub ExitCode);

impl From<()> for ExitCodeWrapper {
    fn from(_val: ()) -> Self {
        ExitCodeWrapper(0)
    }
}

impl From<ExitCode> for ExitCodeWrapper {
    fn from(val: ExitCode) -> Self {
        ExitCodeWrapper(val)
    }
}

/// Set the exit code for an error to `code` and return the resulting
/// [MesaError](../error/struct.MesaError.html).
pub(crate) fn set_exitcode<T, E: StdError + Send + Sync + 'static>(
    error: StdResult<T, E>,
    code: ExitCode,
) -> Result<T> {
    error.map_err(|e| {
        let mut err: MesaError = e.into();
        err.exitcode = code;
        err
    })
}

/// Convert an `Err(String)` to a [MesaError](../error/struct.MesaError.html).
pub(crate) fn string_to_err<T>(error: StdResult<T, String>) -> Result<T> {
    error.map_err(|e| failure::err_msg(e).compat().into())
}

// XXX: the idea for this function is to limit file traversal to one filesystem
#[allow(dead_code)]
pub(crate) fn one_filesystem<T, U>(_start_dir: T, _func: U) -> Result<()>
where
    T: AsRef<Path>,
    U: FnMut() -> Result<()>,
{
    // TODO: should probably loop over specified directory or something and call the function
    Ok(())
}

/// Get the actual path of a file or directory assuming `current_dir` is the current working
/// directory.  If `current_dir` is `None` or `path` is an absolute path, the returned path will
/// be `path`.
pub fn actual_path<'a, D, P>(current_dir: &Option<D>, path: &'a P) -> Cow<'a, Path>
where
    D: AsRef<Path>,
    P: AsRef<Path> + ?Sized,
{
    match current_dir {
        Some(dir) if !Path::new(path.as_ref()).is_absolute() => Cow::from(dir.as_ref().join(path)),
        _ => Cow::from(path.as_ref()),
    }
}

/// Parse an integer with a suffix like "kb" or "MB".
pub fn parse_num_with_suffix(s: &str) -> Option<usize> {
    parse_num_common(s, &SUFFIXES, false)
}

/// Parse an integer with one of the suffixes used by obsolete options (_e.g._ -1k or -4m).
pub fn parse_obsolete_num(s: &str) -> Option<usize> {
    parse_num_common(s, &OBSOLETE_SUFFIXES, true)
}

fn parse_num_common(s: &str, suffixes: &[char], obsolete: bool) -> Option<usize> {
    let mut chars = s.chars();
    let mut found_si = false;
    let mut base = 1;
    let mut power = 1;
    loop {
        let ch = chars.clone().rev().next()?;
        let mut rchars = (&mut chars).rev();
        match ch {
            'b' if !found_si => {
                // special case this one because it's slightly different
                base = 512;
                let _ = rchars.next();
                if obsolete {
                    found_si = true;
                } else {
                    break;
                }
            }
            'B' if !found_si && !obsolete => {
                let _ = rchars.next();
                found_si = true;
            }
            _ => {
                for (i, &suffix) in suffixes.iter().enumerate() {
                    if suffix == ch {
                        base = if found_si { 1000 } else { 1024 };
                        power = i as u32 + 1;
                        let _ = rchars.next();
                        break;
                    }
                }
                break;
            }
        }
    }

    usize::from_str(chars.as_str())
        .ok()?
        .checked_mul(pow(base, power)?)
}

// usize::pow() can panic, and the versions that don't panic are not yet stable
fn pow(mut base: usize, mut exp: u32) -> Option<usize> {
    let mut acc: usize = 1;

    while exp > 1 {
        if (exp & 1) == 1 {
            acc = acc.checked_mul(base)?;
        }
        exp /= 2;
        base = base.checked_mul(base)?;
    }

    if exp == 1 {
        acc = acc.checked_mul(base)?;
    }

    Some(acc)
}

#[test]
fn parse_num_invalid() {
    let strings = ["  1", "1  ", "  1  ", "1X", "b", "1 b", "-1"];
    for s in strings.iter() {
        assert_eq!(parse_num_with_suffix(s), None);
    }

    for suffix in &SUFFIXES {
        assert_eq!(parse_num_with_suffix(&suffix.to_string()), None);
        assert_eq!(parse_num_with_suffix(&format!("{}B", suffix)), None);
        assert_eq!(parse_num_with_suffix(&format!("1 {}", suffix)), None);
        assert_eq!(parse_num_with_suffix(&format!("1 {}B", suffix)), None);

        // TODO: add tests ensuring too large values fail as well
    }

    assert_eq!(
        parse_num_with_suffix(&format!("{}1", usize::max_value())),
        None
    );
}

#[test]
fn parse_num_valid() {
    let strings = [("0", 0), ("1", 1), ("1b", 512)];
    for s in strings.iter() {
        assert_eq!(parse_num_with_suffix(s.0), Some(s.1));
    }

    for (i, suffix) in SUFFIXES.iter().enumerate() {
        let exp = i as u32 + 1;

        assert_eq!(
            parse_num_with_suffix(&format!("1{}", suffix)),
            pow(1024, exp)
        );
        assert_eq!(
            parse_num_with_suffix(&format!("1{}B", suffix)),
            pow(1000, exp)
        );

        // TODO: add tests ensuring values that are almost too large pass
    }

    assert_eq!(
        parse_num_with_suffix(&format!("{}", usize::max_value())),
        Some(usize::max_value())
    );
}

#[test]
fn pow_overflow() {
    let root = (usize::max_value() as f64).sqrt().ceil() as usize;

    assert_eq!(pow(root, 2), None);
    assert!(pow(root - 1, 2).is_some());
}

#[test]
fn pow_correct() {
    assert_eq!(pow(1, 0), Some(1));
    assert_eq!(pow(2, 16), Some(65536));
    assert_eq!(pow(256, 2), Some(65536));
}
