//! Windows: access control is an explicit DACL that grants only the current
//! user. The runtime directory carries an inheritable, protected DACL, so the
//! lock and secret files created inside it are owner-only too. The named pipe
//! gets the same DACL at creation.

use crate::error::{Error, Result};
use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, ERROR_SUCCESS, HANDLE};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    GetNamedSecurityInfoW, SetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    EqualSid, GetSecurityDescriptorDacl, GetTokenInformation, TokenUser, DACL_SECURITY_INFORMATION,
    OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, SECURITY_ATTRIBUTES,
    TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

pub fn default_runtime_dir() -> PathBuf {
    match std::env::var_os("LOCALAPPDATA") {
        Some(p) if !p.is_empty() => PathBuf::from(p).join("EasySSH").join("run"),
        _ => std::env::temp_dir().join("EasySSH-run"),
    }
}

/// Named pipes have no filesystem path length limit.
pub fn check_endpoint_len(_socket: &Path) -> Result<()> {
    Ok(())
}

pub fn private_open_options() -> OpenOptions {
    OpenOptions::new()
}

/// Files inherit the owner-only DACL from the runtime directory.
pub fn verify_private_file(_path: &Path, _f: &File) -> Result<()> {
    Ok(())
}

fn wide(p: &Path) -> Vec<u16> {
    p.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn wide_str(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

struct Token(HANDLE);
impl Drop for Token {
    fn drop(&mut self) {
        // SAFETY: handle came from OpenProcessToken and is closed once.
        unsafe { CloseHandle(self.0) };
    }
}

/// `TOKEN_USER` buffer for the current process. 8-byte aligned storage.
fn token_user() -> io::Result<Vec<u64>> {
    let mut tok: HANDLE = null_mut();
    // SAFETY: out-pointer is valid.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let tok = Token(tok);
    let mut needed = 0u32;
    // SAFETY: size probe with a null buffer; failure is expected.
    unsafe { GetTokenInformation(tok.0, TokenUser, null_mut(), 0, &mut needed) };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
    // SAFETY: buffer is at least `needed` bytes.
    let ok = unsafe {
        GetTokenInformation(
            tok.0,
            TokenUser,
            buf.as_mut_ptr() as *mut c_void,
            needed,
            &mut needed,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(buf)
}

fn user_sid(buf: &[u64]) -> *mut c_void {
    // SAFETY: buf holds a TOKEN_USER filled in by GetTokenInformation.
    unsafe { (*(buf.as_ptr() as *const TOKEN_USER)).User.Sid }
}

fn user_sid_string() -> io::Result<String> {
    let buf = token_user()?;
    let mut s: *mut u16 = null_mut();
    // SAFETY: SID is valid for the life of `buf`; `s` is LocalAlloc'd on success.
    if unsafe { ConvertSidToStringSidW(user_sid(&buf), &mut s) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut len = 0;
    // SAFETY: `s` is NUL-terminated.
    unsafe {
        while *s.add(len) != 0 {
            len += 1;
        }
        let out = String::from_utf16_lossy(std::slice::from_raw_parts(s, len));
        LocalFree(s as *mut c_void);
        Ok(out)
    }
}

/// A security descriptor that grants full access to the current user only.
pub struct OwnerOnlySd {
    sd: *mut c_void,
}

// SAFETY: the descriptor is immutable after creation.
unsafe impl Send for OwnerOnlySd {}
unsafe impl Sync for OwnerOnlySd {}

impl OwnerOnlySd {
    /// `inheritable`: also apply to files and subdirectories created inside.
    pub fn new(inheritable: bool) -> io::Result<Self> {
        let sid = user_sid_string()?;
        let flags = if inheritable { "OICI" } else { "" };
        let right = if inheritable { "FA" } else { "GA" };
        // D:P = protected DACL (no inherited ACEs); a single allow ACE.
        let sddl = wide_str(&format!("D:P(A;{flags};{right};;;{sid})"));
        let mut sd: *mut c_void = null_mut();
        // SAFETY: sddl is NUL-terminated; sd receives a LocalAlloc'd descriptor.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(OwnerOnlySd { sd })
    }

    /// For `ServerOptions::create_with_security_attributes_raw`. Valid while
    /// `self` and the returned struct are alive.
    pub fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.sd,
            bInheritHandle: 0,
        }
    }
}

impl Drop for OwnerOnlySd {
    fn drop(&mut self) {
        // SAFETY: allocated by the SDDL conversion above.
        unsafe { LocalFree(self.sd) };
    }
}

fn insecure(path: &Path, reason: impl Into<String>) -> Error {
    Error::InsecurePath {
        path: path.to_path_buf(),
        reason: reason.into(),
    }
}

/// Create the directory if missing, require that we own it, and replace its
/// DACL with the protected owner-only one (so a looser pre-existing DACL
/// cannot leak the secret file).
pub fn prepare_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let md = std::fs::symlink_metadata(dir)?;
    if !md.is_dir() {
        return Err(insecure(dir, "not a directory (or a reparse point)"));
    }
    let path = wide(dir);

    let mut owner: *mut c_void = null_mut();
    let mut sd_owner: *mut c_void = null_mut();
    // SAFETY: valid NUL-terminated path and out-pointers.
    let rc = unsafe {
        GetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut sd_owner,
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(rc as i32).into());
    }
    let me = token_user()?;
    // SAFETY: both SIDs are valid; sd_owner is freed once.
    let same = unsafe { EqualSid(owner, user_sid(&me)) } != 0;
    unsafe { LocalFree(sd_owner) };
    if !same {
        return Err(insecure(dir, "owned by a different user"));
    }

    let sd = OwnerOnlySd::new(true)?;
    let mut present = 0;
    let mut defaulted = 0;
    let mut dacl: *mut _ = null_mut();
    // SAFETY: sd is a valid descriptor; dacl points into it.
    if unsafe { GetSecurityDescriptorDacl(sd.sd, &mut present, &mut dacl, &mut defaulted) } == 0 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: path and dacl are valid.
    let rc = unsafe {
        SetNamedSecurityInfoW(
            path.as_ptr() as *mut u16,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            dacl,
            null(),
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(rc as i32).into());
    }
    Ok(())
}
