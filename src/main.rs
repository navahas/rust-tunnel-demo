use anyhow::Result;
use axum::{
    extract::ws::{Message as AxumMessage, WebSocket, WebSocketUpgrade},
    response::Response,
    routing::get,
    Router,
};
mod multiplexing;
mod utils;
use futures::{ready, AsyncReadExt, AsyncWriteExt, FutureExt, SinkExt, StreamExt};
use http::{header, Request, Uri};
use std::sync::Arc;
use std::{future::Future, pin::Pin};
use tokio::sync::Mutex as TokioMutex;
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    select,
    sync::Mutex,
};
use tokio::{pin, sync::mpsc};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{handshake::client::generate_key, protocol::WebSocketConfig, Message},
    WebSocketStream, MaybeTlsStream,
};
use tokio_yamux::{stream::StreamHandle, Control, Session};

async fn handle_websocket_server(mut socket: WebSocket) {
    // Create a Yamux connection for the server side
    // let (yamux_control, mut yamux_connection) =
    // let config = yamux::Config::default();
    // let conn = yamux::Connection::new(
    //     WebSocketYamuxAdapter::new(socket),
    //     config,
    //     yamux::Mode::Server,
    // );
    // let (incoming_tx, incoming_rx) = mpsc::channel(10);
    // let (request_tx, request_rx) = mpsc::channel(1);
    // let incoming = crate::multiplexing::YamuxWorker::new(incoming_tx, request_rx, counter.clone());
    // let control = Control::new(request_tx);
    // tokio::spawn(incoming.run(connection));
    // // Spawn a task to handle the Yamux connection
    // tokio::spawn(async move {
    //     if let Err(e) = conn.drive().await {
    //         eprintln!("Yamux connection error: {}", e);
    //     }
    // });
    let yamux = match multiplexing::Yamux::upgrade_connection(
        WebSocketYamuxAdapter::new(socket),
        multiplexing::ConnectionDirection::Inbound,
    ) {
        Ok(yamux) => yamux,
        Err(e) => {
            eprintln!("Failed to upgrade connection: {}", e);
            return;
        }
    };
    // let mut control = yamux.get_yamux_control();
    let mut inc = yamux.into_incoming();

    // Accept new streams
    loop {
        match inc.next().await {
            Some(stream) => {
                println!("New multiplexed stream accepted");
                let target = match TcpStream::connect("127.0.0.1:5435").await {
                    Ok(t) => t,
                    Err(e) => {
                        eprintln!("Failed to connect to target: {}", e);
                        continue;
                    }
                };

                tokio::spawn(handle_multiplexed_stream(stream, target));
            }
            None => {
                eprintln!("No stream accepted");
                break;
            }
        }
    }
}

// Add this adapter to bridge WebSocket and Yamux
struct WebSocketYamuxAdapter {
    socket: Mutex<WebSocket>,
}

impl WebSocketYamuxAdapter {
    fn new(socket: WebSocket) -> Self {
        Self {
            socket: Mutex::new(socket),
        }
    }
}

impl AsyncRead for WebSocketYamuxAdapter {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let lock = self.socket.lock();
        pin!(lock);
        let mut socket = ready!(lock.poll(cx));
        let mut recv = socket.recv();
        pin!(recv);
        match recv.poll(cx) {
            std::task::Poll::Ready(Some(Ok(AxumMessage::Binary(data)))) => {
                buf.put_slice(&data);
                std::task::Poll::Ready(Ok(()))
            }
            std::task::Poll::Ready(Some(Ok(_))) => std::task::Poll::Pending,
            std::task::Poll::Ready(Some(Err(e))) => {
                std::task::Poll::Ready(Err(std::io::Error::new(std::io::ErrorKind::Other, e)))
            }
            std::task::Poll::Ready(None) => std::task::Poll::Ready(Ok(())),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

impl AsyncWrite for WebSocketYamuxAdapter {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        let lock = self.socket.lock();
        pin!(lock);
        let mut socket = ready!(lock.poll(cx));
        let mut send = socket.send(AxumMessage::Binary(buf.to_vec()));
        pin!(send);
        match send.poll(cx) {
            std::task::Poll::Ready(Ok(())) => std::task::Poll::Ready(Ok(buf.len())),
            std::task::Poll::Ready(Err(e)) => {
                std::task::Poll::Ready(Err(std::io::Error::new(std::io::ErrorKind::Other, e)))
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        std::task::Poll::Ready(Ok(()))
    }
}

async fn handle_multiplexed_stream(
    mut yamux_stream: crate::multiplexing::Substream,
    mut target: TcpStream,
) {
    let mut target_buf = vec![0u8; 8192];
    let mut stream_buf = vec![0u8; 8192];

    loop {
        select! {
            result = yamux_stream.read(&mut stream_buf) => {
                match result {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Err(e) = target.write_all(&stream_buf[..n]).await {
                            eprintln!("Failed to write to target: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        eprintln!("Failed to read from yamux stream: {}", e);
                        break;
                    }
                }
            }
            result = target.read(&mut target_buf) => {
                match result {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Err(e) = yamux_stream.write_all(&target_buf[..n]).await {
                            eprintln!("Failed to write to yamux stream: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        eprintln!("Failed to read from target: {}", e);
                        break;
                    }
                }
            }
        }
    }
}

async fn ws_handler(ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(handle_websocket_server)
}

// New struct to hold the shared Yamux control
struct YamuxClient {
    control: crate::multiplexing::Control,
}

impl YamuxClient {
    async fn new(websocket_url: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let uri: Uri = websocket_url.parse()?;

        let request = Request::builder()
            .uri(uri.clone())
            .header("Host", uri.host().expect("No host in URL"))
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", generate_key())
            .header(
                "Authorization",
                "Bearer ee0c2149-0186-4210-a30a-b32661662ac3",
            )
            .body(())
            .expect("Failed to build request");

        let (ws_stream, _) = connect_async_with_config(request, None, false)
            .await
            .map_err(|e| format!("Failed to connect to WebSocket server: {}", e))?;

        println!("Connected to WebSocket server");

        // Use the new TungsteniteWebSocketAdapter instead of WebSocketYamuxAdapter
        let yamux = multiplexing::Yamux::upgrade_connection(
            TungsteniteWebSocketAdapter::new(ws_stream),
            multiplexing::ConnectionDirection::Outbound,
        )
        .map_err(|e| format!("Failed to upgrade to Yamux: {}", e))?;

        let control = yamux.get_yamux_control();

        // Spawn a task to handle incoming streams
        let mut incoming = yamux.into_incoming();
        tokio::spawn(async move {
            while let Some(stream) = incoming.next().await {
                println!("Received new incoming stream");
                // Handle any incoming streams if needed
            }
        });

        Ok(Self { control })
    }

    async fn open_stream(
        &mut self,
    ) -> Result<crate::multiplexing::Substream, Box<dyn std::error::Error>> {
        self.control
            .open_stream()
            .await
            .map_err(|e| format!("Failed to open Yamux stream: {}", e).into())
    }
}

async fn handle_client_connection(local_stream: TcpStream, control: crate::multiplexing::Control) {
    // Open a new stream for this connection
    let mut control = control.clone();
    let stream = match control.open_stream().await {
        Ok(stream) => stream,
        Err(e) => {
            eprintln!("Failed to open stream: {}", e);
            return;
        }
    };

    // Handle the multiplexed stream
    handle_multiplexed_stream(stream, local_stream).await;
}

// Add this adapter for tokio-tungstenite WebSocket streams
struct TungsteniteWebSocketAdapter {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl TungsteniteWebSocketAdapter {
    fn new(socket: WebSocketStream<MaybeTlsStream<TcpStream>>) -> Self {
        Self { socket }
    }
}

impl AsyncRead for TungsteniteWebSocketAdapter {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.socket.poll_next_unpin(cx) {
            std::task::Poll::Ready(Some(Ok(Message::Binary(data)))) => {
                buf.put_slice(&data);
                std::task::Poll::Ready(Ok(()))
            }
            std::task::Poll::Ready(Some(Ok(_))) => std::task::Poll::Pending,
            std::task::Poll::Ready(Some(Err(e))) => {
                std::task::Poll::Ready(Err(std::io::Error::new(std::io::ErrorKind::Other, e)))
            }
            std::task::Poll::Ready(None) => std::task::Poll::Ready(Ok(())),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

impl AsyncWrite for TungsteniteWebSocketAdapter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        match self.socket.poll_ready_unpin(cx) {
            std::task::Poll::Ready(Ok(())) => {
                match self.socket.start_send_unpin(Message::Binary(buf.to_vec())) {
                    Ok(()) => std::task::Poll::Ready(Ok(buf.len())),
                    Err(e) => std::task::Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        e,
                    ))),
                }
            }
            std::task::Poll::Ready(Err(e)) => std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                e,
            ))),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        match self.socket.poll_flush_unpin(cx) {
            std::task::Poll::Ready(Ok(())) => std::task::Poll::Ready(Ok(())),
            std::task::Poll::Ready(Err(e)) => std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                e,
            ))),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        match self.socket.poll_close_unpin(cx) {
            std::task::Poll::Ready(Ok(())) => std::task::Poll::Ready(Ok(())),
            std::task::Poll::Ready(Err(e)) => std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                e,
            ))),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("server") => {
            let app = Router::new().route("/ws", get(ws_handler));
            println!("WebSocket server listening on ws://127.0.0.1:9051/ws");
            let listener = TcpListener::bind("127.0.0.1:9051").await?;
            axum::serve(listener, app.into_make_service()).await?;
        }
        Some("client") => {
            // Create a single Yamux client instance
            let websocket_url = "ws://127.0.0.1:9051/ws";
            let yamux_client = Arc::new(YamuxClient::new(websocket_url).await?);
            let control = yamux_client.control.clone();
            let listener = TcpListener::bind("127.0.0.1:5439").await?;
            println!("Client listening on port 5439");

            while let Ok((stream, addr)) = listener.accept().await {
                println!("New client connection from {}", addr);
                // let yamux_client = yamux_client.clone();
                let mut control = control.clone();
                tokio::spawn(async move {
                    handle_client_connection(stream, control).await;
                });
            }
        }
        _ => {
            println!("Usage: program [server|client]");
        }
    }

    Ok(())
}
