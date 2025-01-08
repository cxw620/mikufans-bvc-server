//! HTTP 1.1 protocol implementation.

use anyhow::{Context, Result, bail};
use fluent_uri::UriRef;
use http::{
    HeaderMap, HeaderName, HeaderValue, Method, StatusCode,
    header::{CONNECTION, CONTENT_LENGTH, CONTENT_TYPE, SERVER},
};
use macro_toolset::string_v2::{NumStr, StringExtT};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter},
    net::TcpStream,
};

#[allow(dead_code, reason = "May be used in the future")]
#[derive(Debug, Clone)]
/// HTTP Request
pub(crate) struct Request {
    /// Request-Line - HTTP Method
    pub method: Method,

    /// Request-Line - Request URI
    pub request_uri: UriRef<String>,

    /// Request Headers
    pub headers: HeaderMap,
}

#[derive(Debug, Clone, Copy)]
#[derive(thiserror::Error)]
pub(crate) enum Error {
    #[error("Invalid HTTP Request-Line")]
    /// Invalid HTTP Request-Line
    RequestLine,

    #[error("Invalid HTTP Request-Line Method")]
    /// Invalid HTTP Request-Line Method
    RequestLineMethod,

    #[error("Invalid HTTP Request-Line URI")]
    /// Invalid HTTP Request-Line URI
    RequestLineUri,

    #[error("Invalid HTTP Version")]
    /// Invalid HTTP Version
    HTTPVersion,

    #[error("Invalid HTTP Header")]
    /// Invalid HTTP Header
    Header,
}

impl Request {
    /// Parse a HTTP Request from a [`TcpStream`].
    pub(crate) async fn handle(tcp_stream: &mut TcpStream) -> Result<Option<Self>> {
        let mut request_lines = BufReader::new(tcp_stream).lines();

        let start_line = request_lines.next_line().await?;

        if start_line.is_none() {
            return Ok(None);
        }

        // Start handle request
        let start_line = start_line.unwrap();

        let mut start_line = start_line.split(' ');

        let mut request = Request {
            method: Method::from_bytes(
                start_line
                    .next()
                    .context(Error::RequestLineMethod)?
                    .as_bytes(),
            )
            .context(Error::RequestLineMethod)?,
            request_uri: fluent_uri::UriRef::parse(
                start_line.next().context(Error::RequestLineUri)?,
            )
            .context(Error::RequestLineUri)?
            .to_owned(),
            headers: HeaderMap::with_capacity(8),
        };

        if start_line.next().context(Error::RequestLine)? != "HTTP/1.1" {
            bail!(Error::HTTPVersion)
        }

        loop {
            let header_line = request_lines
                .next_line()
                .await
                .context(Error::Header)?
                .context(Error::Header)?;

            if header_line.is_empty() {
                break;
            }

            let (header_name, header_value) = header_line.split_once(':').context(Error::Header)?;
            request.headers.insert(
                HeaderName::from_bytes(header_name.as_bytes()).context(Error::Header)?,
                header_value.trim().parse().context(Error::Header)?,
            );
        }

        Ok(Some(request))
    }
}

#[derive(Debug, Clone)]
/// HTTP Response
pub(crate) struct Response {
    /// Response Status code
    pub status: StatusCode,

    /// Response Headers
    pub headers: HeaderMap,

    /// Response Body
    pub body: Option<Vec<u8>>,
}

impl Default for Response {
    fn default() -> Self {
        let mut this = Self {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: None,
        };

        this.headers
            .insert(SERVER, HeaderValue::from_static(env!("CARGO_PKG_NAME")));
        this.headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        this.headers
            .insert(CONNECTION, HeaderValue::from_static("keep-alive"));

        this
    }
}

#[allow(unused, reason = "pub(crate), may be used in the future")]
impl Response {
    #[inline]
    pub(crate) fn status(status: StatusCode) -> Self {
        Self {
            status,
            ..Default::default()
        }
    }

    /// Set HTTP [`StatusCode`].
    pub(crate) const fn set_status(mut self, status: StatusCode) -> Self {
        self.status = status;
        self
    }

    /// Set HTTP [`HeaderMap`].
    pub(crate) fn set_headers(mut self, headers: HeaderMap) -> Self {
        self.headers = headers;
        self
    }

    /// Set Body
    pub(crate) fn set_body(mut self, body: Option<Vec<u8>>) -> Self {
        self.body = body;
        self
    }

    #[inline]
    /// Get a mutable reference to the headers.
    pub(crate) const fn headers_mut(&mut self) -> &mut HeaderMap {
        &mut self.headers
    }

    /// Write the response to a [`TcpStream`].
    pub(crate) async fn write_to_stream(mut self, tcp_stream: &mut TcpStream) -> Result<()> {
        tracing::debug!("Writting response to {}", tcp_stream.peer_addr()?);

        let mut buf_writer = BufWriter::new(tcp_stream);

        // Response line
        buf_writer.write_all(b"HTTP/1.1 ").await?;
        buf_writer
            .write_all(self.status.as_str().as_bytes())
            .await?;
        buf_writer.write_all(b"\r\n").await?;

        // Header lines
        if let Some(len) = self.body.as_ref().map(|body| body.len()) {
            self.headers.insert(
                CONTENT_LENGTH,
                NumStr::new_default(len).to_http_header_value()?,
            );
        }
        for (header_name, header_value) in self.headers.iter() {
            buf_writer
                .write_all(header_name.as_str().as_bytes())
                .await?;
            buf_writer.write_all(b": ").await?;
            buf_writer.write_all(header_value.as_bytes()).await?;
            buf_writer.write_all(b"\r\n").await?;
        }

        // CRLF
        buf_writer.write_all(b"\r\n").await?;

        // Body
        if let Some(body) = self.body {
            buf_writer.write_all(&body).await?;
        }

        buf_writer.flush().await?;

        Ok(())
    }
}
