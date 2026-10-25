use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use windows_sys::core::PWSTR;
use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE, HLOCAL};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
    TOKEN_USER,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::{owner_only_sddl, Endpoint, IpcError, APP_DIR_NAME};

pub struct Listener {
    name: OsString,
    server: NamedPipeServer,
}

pub struct Stream {
    inner: Kind,
}

enum Kind {
    Client(NamedPipeClient),
    Server(NamedPipeServer),
}

pub fn user_endpoint() -> Result<Endpoint, IpcError> {
    let sid = current_user_sid()?;
    if owner_only_sddl(&sid).is_none() {
        return Err(IpcError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "идентификатор пользователя отклонён",
        )));
    }
    Ok(Endpoint {
        name: OsString::from(format!(r"\\.\pipe\{APP_DIR_NAME}-{sid}")),
    })
}

pub fn pipe_name_for_test(data_dir: &Path) -> OsString {
    let leaf = data_dir.file_name().unwrap_or_default().to_string_lossy();
    let safe: String = leaf
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
        .collect();
    let safe = if safe.is_empty() {
        "test"
    } else {
        safe.as_str()
    };
    OsString::from(format!(r"\\.\pipe\{APP_DIR_NAME}-{safe}"))
}

pub async fn listen(endpoint: &Endpoint) -> Result<Listener, IpcError> {
    let server = create_server(&endpoint.name)?;
    Ok(Listener {
        name: endpoint.name.clone(),
        server,
    })
}

pub async fn connect(endpoint: &Endpoint) -> Result<Stream, IpcError> {
    let client = ClientOptions::new().open(&endpoint.name)?;
    Ok(Stream {
        inner: Kind::Client(client),
    })
}

impl Listener {
    pub async fn accept(&mut self) -> Result<Stream, IpcError> {
        self.server.connect().await?;
        let next = create_server(&self.name)?;
        let connected = std::mem::replace(&mut self.server, next);
        Ok(Stream {
            inner: Kind::Server(connected),
        })
    }
}

fn create_server(name: &OsStr) -> Result<NamedPipeServer, IpcError> {
    let sid = current_user_sid()?;
    let sddl = owner_only_sddl(&sid).ok_or_else(|| {
        IpcError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "идентификатор пользователя отклонён",
        ))
    })?;
    let sddl = wide(&sddl);
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: `sddl` is a null-terminated UTF-16 SDDL string we just built.
    // The descriptor is freed by `DescriptorGuard` before this function returns.
    let created = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    };
    if created == 0 {
        return Err(IpcError::Io(io::Error::last_os_error()));
    }
    let _descriptor = DescriptorGuard(descriptor);
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let mut options = ServerOptions::new();
    options.reject_remote_clients(true);
    // SAFETY: `attributes` points at a live SECURITY_ATTRIBUTES whose descriptor
    // stays allocated for this call. Tokio copies it into the pipe and does not
    // keep the pointer. The returned handle is owned by NamedPipeServer.
    let server = unsafe {
        options.create_with_security_attributes_raw(
            name,
            &mut attributes as *mut SECURITY_ATTRIBUTES as *mut std::ffi::c_void,
        )?
    };
    Ok(server)
}

fn current_user_sid() -> Result<String, IpcError> {
    // SAFETY: the token handle is closed by `HandleGuard`. The SID buffer is
    // sized from GetTokenInformation and aligned to usize. The wide SID string
    // is copied into a Rust String before LocalFree.
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(IpcError::Io(io::Error::last_os_error()));
        }
        let _token = HandleGuard(token);
        let mut needed = 0u32;
        let _ = GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
        if needed == 0 {
            return Err(IpcError::Io(io::Error::last_os_error()));
        }
        let words = needed.div_ceil(std::mem::size_of::<usize>() as u32) as usize;
        let mut buffer = vec![0usize; words.max(1)];
        if GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr() as *mut _,
            needed,
            &mut needed,
        ) == 0
        {
            return Err(IpcError::Io(io::Error::last_os_error()));
        }
        let user = &*(buffer.as_ptr() as *const TOKEN_USER);
        let mut sid_wide: PWSTR = std::ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut sid_wide) == 0 {
            return Err(IpcError::Io(io::Error::last_os_error()));
        }
        let sid = wide_ptr_to_string(sid_wide);
        LocalFree(sid_wide as HLOCAL);
        if sid.is_empty() {
            return Err(IpcError::Io(io::Error::other(
                "пустой идентификатор пользователя",
            )));
        }
        Ok(sid)
    }
}

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

unsafe fn wide_ptr_to_string(value: PWSTR) -> String {
    let mut length = 0usize;
    while !value.is_null() && *value.add(length) != 0 {
        length += 1;
    }
    if value.is_null() {
        return String::new();
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(value, length))
}

struct HandleGuard(HANDLE);

impl Drop for HandleGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CloseHandle(self.0) };
        }
    }
}

struct DescriptorGuard(PSECURITY_DESCRIPTOR);

impl Drop for DescriptorGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LocalFree(self.0 as HLOCAL) };
        }
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        match &mut self.get_mut().inner {
            Kind::Client(stream) => std::pin::Pin::new(stream).poll_read(cx, buf),
            Kind::Server(stream) => std::pin::Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        match &mut self.get_mut().inner {
            Kind::Client(stream) => std::pin::Pin::new(stream).poll_write(cx, buf),
            Kind::Server(stream) => std::pin::Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        match &mut self.get_mut().inner {
            Kind::Client(stream) => std::pin::Pin::new(stream).poll_flush(cx),
            Kind::Server(stream) => std::pin::Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        match &mut self.get_mut().inner {
            Kind::Client(stream) => std::pin::Pin::new(stream).poll_shutdown(cx),
            Kind::Server(stream) => std::pin::Pin::new(stream).poll_shutdown(cx),
        }
    }
}
