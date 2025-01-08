//! Mikufans-BVC-Server

mod config;
mod playurl;
mod proto;
mod utils;

use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::Result;
use http::{
    HeaderValue, Method, StatusCode,
    header::{
        ACCEPT_RANGES, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
        ACCESS_CONTROL_EXPOSE_HEADERS, ACCESS_CONTROL_MAX_AGE, CONTENT_LENGTH, CONTENT_RANGE,
        CONTENT_TYPE, RANGE,
    },
};
use http_range_header::{ParsedRanges, SyntacticallyCorrectRange};
use macro_toolset::{init_tracing_simple, str_concat_v2, string_v2::StringExtT};
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt, BufReader, copy_buf},
    net::{TcpListener, TcpStream},
    signal::ctrl_c,
    task::yield_now,
    time::sleep,
};

#[tokio::main]
/// Main function
async fn main() -> Result<()> {
    init_tracing_simple!();

    let tcp_listener = TcpListener::bind("172.16.201.2:7080").await?;

    tokio::spawn(async move {
        loop {
            let (mut tcp_stream, peer_addr) = tcp_listener.accept().await?;

            tracing::debug!("New connection from {peer_addr}");

            tokio::spawn(async move {
                let idle_handler = utils::IdleHandler::new();
                let should_shutdown: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));

                let handler = {
                    let idle_handler = idle_handler.clone();
                    let should_shutdown = should_shutdown.clone();

                    tokio::spawn(async move {
                        loop {
                            {
                                // HTTP/1.1 Keep-Alive, wait for new data
                                let mut _buf = [0; 1];
                                tokio::select! {
                                    biased;
                                    data = tcp_stream.peek(&mut _buf) => {
                                        if data.is_ok_and(|count| count > 0) {
                                            tracing::debug!("New incoming data from {peer_addr}");
                                        } else {
                                            tracing::debug!("Connection was shut down by peer");
                                            break
                                        }
                                    },
                                    _ = async {
                                        let sleep_dur = Duration::from_millis(500);
                                        loop {
                                            if should_shutdown.load(Ordering::Acquire) {
                                                break;
                                            }

                                            sleep(sleep_dur).await;
                                            yield_now().await;
                                        }
                                    } => {
                                        break
                                    }
                                }
                            }

                            {
                                let _guard = idle_handler.idle_guard();

                                match handler(&mut tcp_stream).await {
                                    Ok(can_continue) => {
                                        if !can_continue {
                                            break;
                                        }
                                    }
                                    Err(e) => {
                                        tracing::error!("{e:?}");

                                        // Default response
                                        if let Err(e) =
                                            proto::Response::status(StatusCode::BAD_REQUEST)
                                                .write_to_stream(&mut tcp_stream)
                                                .await
                                        {
                                            tracing::error!("Write response error: {e:?}");
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    })
                };

                tokio::select! {
                    _ = handler => {}
                    _ = idle_handler.wait_max_idle(None) => {
                        tracing::debug!("Keep-alive idle timeout, shutting down connection from {peer_addr}");

                        should_shutdown.store(true, Ordering::Release);
                    }
                }
            });
        }

        #[allow(unreachable_code, reason = "Make compiler happy")]
        Ok::<_, anyhow::Error>(())
    });

    ctrl_c().await?;

    Ok(())
}

#[inline]
async fn handler(tcp_stream: &mut TcpStream) -> Result<bool> {
    let request = proto::Request::handle(tcp_stream).await?;

    if request.is_none() {
        tracing::debug!("No Request?");
        return Ok(true);
    }

    let request = request.unwrap();
    tracing::debug!("{request:?}");

    let request_path = request.request_uri.path().as_str();

    let mut response = proto::Response::default();

    match request_path {
        _ if request_path.starts_with("/resource/mikufans") => {
            // Resource HEADERS
            {
                let headers = response.headers_mut();

                // headers.insert(HeaderName::from_static("x-mikufans-request-id"), val);
                headers.insert(
                    ACCESS_CONTROL_ALLOW_ORIGIN,
                    HeaderValue::from_static("https://www.bilibili.com"),
                );
                headers.insert(
                    ACCESS_CONTROL_ALLOW_METHODS,
                    HeaderValue::from_static("GET, POST, PUT, DELETE, HEAD"),
                );
                headers.insert(
                    ACCESS_CONTROL_EXPOSE_HEADERS,
                    HeaderValue::from_static("Content-Length,Content-Range"),
                );
                headers.insert(ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("0"));
            }

            let parsed_ranges = request.headers.get(RANGE).and_then(|range| {
                #[allow(unsafe_code, reason = "HeaderValue")]
                http_range_header::parse_range_header(unsafe {
                    std::str::from_utf8_unchecked(range.as_bytes())
                })
                .ok()
            });

            let mut file = File::open("./test/video.m4s").await?;
            let file_length = file.metadata().await?.len();

            match parsed_ranges {
                Some(ParsedRanges { ranges }) => {
                    if ranges.len() == 1 {
                        let SyntacticallyCorrectRange { start, end } = ranges[0];

                        if let Some((start, end)) =
                            match start {
                                http_range_header::StartPosition::Index(idx) => Some(idx),
                                http_range_header::StartPosition::FromLast(idx) => {
                                    file_length.checked_sub(idx)
                                }
                            }
                            .take_if(|&mut idx| idx <= file_length)
                            .zip(match end {
                                http_range_header::EndPosition::Index(idx) => {
                                    if idx <= file_length { Some(idx) } else { None }
                                }
                                http_range_header::EndPosition::LastByte => Some(file_length),
                            })
                        {
                            // RANGE response
                            {
                                response.set_status(StatusCode::PARTIAL_CONTENT);
                                let headers = response.headers_mut();

                                headers.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
                                headers
                                    .insert(CONTENT_LENGTH, (end - start).to_http_header_value()?);
                                headers.insert(
                                    CONTENT_RANGE,
                                    str_concat_v2!("bytes ", start, "-", end, "/", file_length)
                                        .to_http_header_value()?,
                                );
                            }

                            if let Err(e) = response.write_to_stream(tcp_stream).await {
                                tracing::error!("Write response error: {e:?}");
                                return Ok(false);
                            }

                            if request.method != Method::GET {
                                // Not GET, return
                                return Ok(true);
                            }

                            file.seek(io::SeekFrom::Start(start)).await?;
                            let mut file = file.take(end - start);

                            // TODO: rate limit?
                            if let Err(e) = copy_buf(
                                &mut BufReader::with_capacity(8 * 1024 * 1024, &mut file),
                                tcp_stream,
                            )
                            .await
                            {
                                tracing::error!("Copy file error: {e:?}");
                                return Ok(false);
                            }

                            return Ok(true);
                        };
                    }

                    // Invalid Range request, return all
                    response
                        .headers_mut()
                        .insert(CONTENT_LENGTH, file_length.to_http_header_value()?);

                    if let Err(e) = response.write_to_stream(tcp_stream).await {
                        tracing::error!("Write response error: {e:?}");
                        return Ok(false);
                    }

                    if request.method != Method::GET {
                        // Not GET, return
                        return Ok(true);
                    }

                    // TODO: rate limit?
                    if let Err(e) = copy_buf(
                        &mut BufReader::with_capacity(8 * 1024 * 1024, &mut file),
                        tcp_stream,
                    )
                    .await
                    {
                        tracing::error!("Copy file error: {e:?}");
                        return Ok(false);
                    }
                }
                None => {
                    response
                        .headers_mut()
                        .insert(CONTENT_LENGTH, file_length.to_http_header_value()?);

                    if let Err(e) = response.write_to_stream(tcp_stream).await {
                        tracing::error!("Write response error: {e:?}");
                        return Ok(false);
                    }

                    if request.method != Method::GET {
                        // Not GET, return
                        return Ok(true);
                    }

                    // TODO: rate limit?
                    if let Err(e) = copy_buf(
                        &mut BufReader::with_capacity(8 * 1024 * 1024, &mut file),
                        tcp_stream,
                    )
                    .await
                    {
                        tracing::error!("Copy file error: {e:?}");
                        return Ok(false);
                    }
                }
            }
        }
        "/favicon.ico" => {
            response
                .headers_mut()
                .insert(CONTENT_TYPE, HeaderValue::from_static("image/icon"));
            response
                .headers_mut()
                .insert(CONTENT_LENGTH, HeaderValue::from_static("0"));

            if let Err(e) = response.write_to_stream(tcp_stream).await {
                tracing::error!("Write response error: {e:?}");
                return Ok(false);
            }
        }
        _ => {
            response
                .headers_mut()
                .insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));

            if let Err(e) = response
                .with_body(env!("CARGO_PKG_NAME").as_bytes())
                .write_to_stream(tcp_stream)
                .await
            {
                tracing::error!("Write response error: {e:?}");
                return Ok(false);
            }
        }
    }

    Ok(true)
}
