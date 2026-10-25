use std::fs;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{UnixListener, UnixStream};

use crate::{set_owner_only, Endpoint, IpcError};

pub struct Listener {
    inner: UnixListener,
}

pub struct Stream(UnixStream);

pub async fn listen(endpoint: &Endpoint) -> Result<Listener, IpcError> {
    if let Some(parent) = endpoint.path.parent() {
        fs::create_dir_all(parent)?;
        set_owner_only(parent, true)?;
    }
    if endpoint.path.exists() {
        match UnixStream::connect(&endpoint.path).await {
            Ok(stream) => {
                drop(stream);
                return Err(IpcError::AlreadyRunning);
            }
            Err(_) => fs::remove_file(&endpoint.path)?,
        }
    }
    let listener = UnixListener::bind(&endpoint.path)?;
    // bind applies the process umask, so the mode is set again afterwards.
    set_owner_only(&endpoint.path, false)?;
    Ok(Listener { inner: listener })
}

pub async fn connect(endpoint: &Endpoint) -> Result<Stream, IpcError> {
    Ok(Stream(UnixStream::connect(&endpoint.path).await?))
}

impl Listener {
    pub async fn accept(&mut self) -> Result<Stream, IpcError> {
        let (stream, _peer) = self.inner.accept().await?;
        Ok(Stream(stream))
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().0).poll_read(cx, buf)
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.get_mut().0).poll_write(cx, buf)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().0).poll_flush(cx)
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.get_mut().0).poll_shutdown(cx)
    }
}
