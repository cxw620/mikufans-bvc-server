//! Mikufans-BVC-Server

mod config;
mod playurl;
mod proto;
mod utils;

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::Result;
use http::{HeaderValue, StatusCode, header::CONTENT_TYPE};
use macro_toolset::init_tracing_simple;
use tokio::{
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
                                    _ = tcp_stream.peek(&mut _buf) => {},
                                    _ = async {
                                        loop {
                                            if should_shutdown.load(Ordering::Acquire) {
                                                break;
                                            }

                                            yield_now().await;
                                            sleep(Duration::from_millis(500)).await;
                                        }
                                    } => {
                                        break
                                    }
                                }
                            }

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
                                    if let Err(e) = proto::Response::status(StatusCode::BAD_REQUEST)
                                        .write_to_stream(&mut tcp_stream)
                                        .await
                                    {
                                        tracing::error!("Write response error: {e:?}");
                                        break;
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
        tracing::debug!("Current connection finished or invalid, shutdown current TCP stream");
        return Ok(false);
    }

    tracing::info!("{request:?}");

    let mut response = proto::Response::default().set_body(Some(b"Hello World".to_vec()));

    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));

    if let Err(e) = response.write_to_stream(tcp_stream).await {
        tracing::error!("Write response error: {e:?}");
        return Ok(false);
    }

    Ok(true)
}
