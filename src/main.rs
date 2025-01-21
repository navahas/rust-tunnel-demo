use futures::{SinkExt, StreamExt};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    select,
};
use tokio_tungstenite::{
    accept_async, connect_async,
    tungstenite::Message,
    WebSocketStream,
};

async fn handle_websocket_server(ws_stream: WebSocketStream<TcpStream>) {
    let mut target = match TcpStream::connect("127.0.0.1:5435").await {
        Ok(stream) => stream,
        Err(e) => {
            eprintln!("Failed to connect to target: {}", e);
            return;
        }
    };

    println!("Connected to target on port 5435");

    let (mut ws_sender, mut ws_receiver) = ws_stream.split();
    let mut target_buf = vec![0u8; 8192];
    let mut bytes_sent = 0;
    let mut bytes_received = 0;

    loop {
        select! {
            msg = ws_receiver.next() => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        bytes_sent += data.len();
                        println!("Server: Forwarding {} bytes to target (total sent: {})", data.len(), bytes_sent);
                        if let Err(e) = target.write_all(&data).await {
                            eprintln!("Failed to write to target: {}", e);
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) => {
                        println!("WebSocket connection closed by client");
                        break;
                    }
                    Some(Err(e)) => {
                        eprintln!("WebSocket error: {}", e);
                        break;
                    }
                    None => {
                        println!("WebSocket connection ended");
                        break;
                    }
                    _ => {} // Ignore other message types
                }
            }
            result = target.read(&mut target_buf) => {
                match result {
                    Ok(0) => {
                        println!("Target closed connection");
                        break;
                    }
                    Ok(n) => {
                        bytes_received += n;
                        println!("Server: Forwarding {} bytes to WebSocket (total received: {})", n, bytes_received);
                        if let Err(e) = ws_sender.send(Message::Binary(target_buf[..n].to_vec())).await {
                            eprintln!("Failed to send to WebSocket: {}", e);
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

    println!(
        "Server connection closed. Total bytes: sent={}, received={}",
        bytes_sent, bytes_received
    );
}

async fn handle_client_connection(mut local_stream: TcpStream) {
    let ws_stream = match connect_async("ws://127.0.0.1:9051").await {
        Ok((stream, _)) => stream,
        Err(e) => {
            eprintln!("Failed to connect to WebSocket server: {}", e);
            return;
        }
    };

    println!("Connected to WebSocket server on port 9051");

    let (mut ws_sender, mut ws_receiver) = ws_stream.split();
    let mut local_buf = vec![0u8; 8192];
    let mut bytes_sent = 0;
    let mut bytes_received = 0;

    loop {
        select! {
            result = local_stream.read(&mut local_buf) => {
                match result {
                    Ok(0) => {
                        println!("Local connection closed");
                        break;
                    }
                    Ok(n) => {
                        bytes_sent += n;
                        println!("Client: Forwarding {} bytes to WebSocket server (total sent: {})", n, bytes_sent);
                        if let Err(e) = ws_sender.send(Message::Binary(local_buf[..n].to_vec())).await {
                            eprintln!("Failed to send to WebSocket server: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        eprintln!("Failed to read from local connection: {}", e);
                        break;
                    }
                }
            }
            msg = ws_receiver.next() => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        bytes_received += data.len();
                        println!("Client: Forwarding {} bytes to local connection (total received: {})", data.len(), bytes_received);
                        if let Err(e) = local_stream.write_all(&data).await {
                            eprintln!("Failed to write to local connection: {}", e);
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) => {
                        println!("WebSocket connection closed by server");
                        break;
                    }
                    Some(Err(e)) => {
                        eprintln!("WebSocket error: {}", e);
                        break;
                    }
                    None => {
                        println!("WebSocket connection ended");
                        break;
                    }
                    _ => {} // Ignore other message types
                }
            }
        }
    }

    println!(
        "Client connection closed. Total bytes: sent={}, received={}",
        bytes_sent, bytes_received
    );
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("server") => {
            let listener = TcpListener::bind("127.0.0.1:9051").await?;
            println!("WebSocket server listening on ws://127.0.0.1:9051");

            while let Ok((stream, addr)) = listener.accept().await {
                println!("New WebSocket connection from {}", addr);
                tokio::spawn(async move {
                    match accept_async(stream).await {
                        Ok(ws_stream) => {
                            handle_websocket_server(ws_stream).await;
                        }
                        Err(e) => {
                            eprintln!("Failed to accept WebSocket connection: {}", e);
                        }
                    }
                });
            }
        }
        Some("client") => {
            let listener = TcpListener::bind("127.0.0.1:5439").await?;
            println!("Client listening on port 5439");

            while let Ok((stream, addr)) = listener.accept().await {
                println!("New client connection from {}", addr);
                tokio::spawn(async move {
                    handle_client_connection(stream).await;
                });
            }
        }
        _ => {
            println!("Usage: program [server|client]");
        }
    }

    Ok(())
}
