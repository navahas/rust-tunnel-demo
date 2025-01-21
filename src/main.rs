use futures::{SinkExt, StreamExt};
use std::env;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    select,
    sync::broadcast,
};
use tokio_tungstenite::{accept_async, connect_async, tungstenite::Message, WebSocketStream};

async fn tunnel_server() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:9050").await?;
    println!("Tunnel server listening on port 9050");

    while let Ok((stream, _)) = listener.accept().await {
        tokio::spawn(async move {
            let ws_stream = accept_async(stream).await.expect("Failed to accept");
            let postgres = TcpStream::connect("127.0.0.1:5433")
                .await
                .expect("Failed to connect to postgres");
            handle_tunnel(ws_stream, postgres).await;
        });
    }
    Ok(())
}

async fn tunnel_client() -> Result<(), Box<dyn std::error::Error>> {
    let ws_stream = connect_async("ws://127.0.0.1:9050").await?.0;
    let (mut ws_write, mut ws_read) = ws_stream.split();
    let (tx, _rx) = broadcast::channel::<Vec<u8>>(32);
    let local_listener = TcpListener::bind("127.0.0.1:5439").await?;
    println!("Client listening on port 5439");

    while let Ok((local_stream, _)) = local_listener.accept().await {
        let tx = tx.clone();
        let mut rx = tx.subscribe();
        tokio::spawn(async move {
            handle_local_connection(local_stream, tx, rx).await;
        });
    }
    Ok(())
}

async fn handle_local_connection(
    mut local_stream: TcpStream,
    tx: broadcast::Sender<Vec<u8>>,
    mut rx: broadcast::Receiver<Vec<u8>>,
) {
    let mut buf = vec![0u8; 8192];
    let mut bytes_sent = 0;
    let mut bytes_received = 0;

    loop {
        select! {
            n = local_stream.read(&mut buf) => {
                match n {
                    Ok(n) if n == 0 => break, // Connection closed
                    Ok(n) => {
                        bytes_sent += n;
                        println!("Sent {} bytes (total: {})", n, bytes_sent);
                        if let Err(e) = tx.send(buf[..n].to_vec()) {
                            eprintln!("Failed to send to broadcast: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        eprintln!("Failed to read from local stream: {}", e);
                        break;
                    }
                }
            }
            msg = rx.recv() => {
                match msg {
                    Ok(data) => {
                        bytes_received += data.len();
                        println!("Received {} bytes (total: {})", data.len(), bytes_received);
                        if let Err(e) = local_stream.write_all(&data).await {
                            eprintln!("Failed to write to local stream: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        eprintln!("Failed to receive from broadcast: {}", e);
                        break;
                    }
                }
            }
        }
    }
    println!("Connection closed. Total bytes: sent={}, received={}", bytes_sent, bytes_received);
}

async fn handle_tunnel(ws_stream: WebSocketStream<TcpStream>, mut postgres: TcpStream) {
    let (mut ws_write, mut ws_read) = ws_stream.split();
    let mut buf = vec![0u8; 8192];

    loop {
        select! {
            n = postgres.read(&mut buf) => {
                match n {
                    Ok(n) if n == 0 => break, // Connection closed
                    Ok(n) => {
                        println!("Writing {} bytes to websocket", n);
                        if let Err(e) = ws_write.send(Message::Binary(buf[..n].to_vec())).await {
                            eprintln!("Failed to send to websocket: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        eprintln!("Failed to read from postgres: {}", e);
                        break;
                    }
                }
            }
            msg = ws_read.next() => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        println!("Received {} bytes", data.len());
                        if let Err(e) = postgres.write_all(&data).await {
                            eprintln!("Failed to write to postgres: {}", e);
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(e)) => {
                        eprintln!("WebSocket error: {}", e);
                        break;
                    }
                    _ => {} // Ignore other message types
                }
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("server") => tunnel_server().await?,
        Some("client") => tunnel_client().await?,
        _ => println!("Usage: program [server|client]"),
    }
    Ok(())
}
