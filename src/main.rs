use futures::{SinkExt, StreamExt};
use std::env;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    select
};
use tokio_tungstenite::{
    accept_async, connect_async, tungstenite::Message, 
    WebSocketStream, MaybeTlsStream
};
type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type WsSink = futures::stream::SplitSink<WsStream, Message>;
type WsSource = futures::stream::SplitStream<WsStream>;

const HTTP_PORT: u16 = 5422;
const TCP_PORT: u16 = 5433;

async fn tunnel_server() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:9050").await?;
    println!("Tunnel server listening on port 9050");

    while let Ok((stream, _)) = listener.accept().await {
        tokio::spawn(async move {
            let mut peek_buf = [0u8; 8];
            let mut target_port = TCP_PORT; // Default to PostgreSQL

            // Peek at the incoming data BEFORE moving the stream
            match stream.peek(&mut peek_buf).await {
                Ok(n) if n > 0 => {
                    let first_bytes = &peek_buf[..n];

                    // Detect HTTP methods like GET, POST, PUT, etc.
                    if first_bytes.starts_with(b"GET ")
                        || first_bytes.starts_with(b"POST ")
                        || first_bytes.starts_with(b"HEAD ")
                        || first_bytes.starts_with(b"PUT ")
                        || first_bytes.starts_with(b"DELETE ")
                        || first_bytes.starts_with(b"OPTIONS ")
                    {
                        println!("Detected HTTP traffic, forwarding to {}", HTTP_PORT);
                        target_port = HTTP_PORT;
                    } else {
                        println!("Detected TCP traffic, forwarding to {}", TCP_PORT);
                    }
                }
                Err(e) => {
                    eprintln!("Failed to peek incoming data: {}", e);
                    return;
                }
                _ => {
                    eprintln!("No data received, closing connection.");
                    return;
                }
            }

            // Now we safely pass `stream` to WebSocket handler
            match accept_async(stream).await {
                Ok(ws_stream) => {
                    let target_service = TcpStream::connect(format!("127.0.0.1:{}", target_port))
                        .await
                        .expect("Failed to connect to target service");

                    if target_port == HTTP_PORT {
                        handle_http_tunnel(ws_stream, target_service).await;
                    } else {
                        handle_tunnel(ws_stream, target_service).await;
                    }
                }
                Err(e) => {
                    eprintln!("Failed to accept WebSocket connection: {}", e);
                }
            }
        });
    }
    Ok(())
}

async fn tunnel_client() -> Result<(), Box<dyn std::error::Error>> {
    let (ws_stream, _) = connect_async("ws://127.0.0.1:9050").await?;
    let (ws_write, ws_read) = ws_stream.split();
    let ws_write = Arc::new(Mutex::new(ws_write));
    let ws_read = Arc::new(Mutex::new(ws_read));
    
    let local_listener = TcpListener::bind("127.0.0.1:5439").await?;
    println!("Client listening on port 5439");

    while let Ok((local_stream, _)) = local_listener.accept().await {
        let ws_write = ws_write.clone();
        let ws_read = ws_read.clone();
        
        tokio::spawn(async move {
            handle_client_connection(local_stream, ws_write, ws_read).await;
        });
    }
    Ok(())
}

async fn handle_client_connection(
    mut local_stream: TcpStream,
    ws_write: Arc<Mutex<WsSink>>,
    ws_read: Arc<Mutex<WsSource>>,
) {
    let mut buf = vec![0u8; 8192];
    let mut bytes_sent = 0;
    let mut bytes_received = 0;

    loop {
        let ws_message_future = async {
            let mut reader = ws_read.lock().await;
            reader.next().await
        };

        select! {
            n = local_stream.read(&mut buf) => {
                match n {
                    Ok(n) if n == 0 => break, // Connection closed
                    Ok(n) => {
                        bytes_sent += n;
                        println!("Sending {} bytes (total: {})", n, bytes_sent);
                        
                        let mut ws_writer = ws_write.lock().await;
                        if let Err(e) = ws_writer.send(Message::Binary(buf[..n].to_vec())).await {
                            eprintln!("Failed to send to websocket: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        eprintln!("Failed to read from local stream: {}", e);
                        break;
                    }
                }
            }
            msg = ws_message_future => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        bytes_received += data.len();
                        println!("Received {} bytes (total: {})", data.len(), bytes_received);
                        
                        if let Err(e) = local_stream.write_all(&data).await {
                            eprintln!("Failed to write to local stream: {}", e);
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

async fn handle_http_tunnel(ws_stream: WebSocketStream<TcpStream>, mut target_service: TcpStream) {
    let (mut ws_write, mut ws_read) = ws_stream.split();
    let mut buf = vec![0u8; 8192];

    loop {
        select! {
            // Read from the HTTP service (Next.js) and send response to WebSocket
            n = target_service.read(&mut buf) => {
                match n {
                    Ok(n) if n == 0 => {
                        println!("HTTP connection closed by server.");
                        break;
                    }
                    Ok(n) => {
                        let response = String::from_utf8_lossy(&buf[..n]).to_string();
                        println!("Forwarding HTTP Response:\n{}", response);

                        // Ensure we send a complete HTTP response to WebSocket
                        if let Err(e) = ws_write.send(Message::Binary(buf[..n].to_vec())).await {
                            eprintln!("Failed to send HTTP response to WebSocket: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        eprintln!("Failed to read from HTTP service: {}", e);
                        break;
                    }
                }
            }
            // Read from WebSocket and send to HTTP service
            msg = ws_read.next() => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        let request = String::from_utf8_lossy(&data).to_string();
                        println!("Received HTTP Request:\n{}", request);

                        // Ensure we send a full HTTP request to the Next.js service
                        if let Err(e) = target_service.write_all(&data).await {
                            eprintln!("Failed to write HTTP request to service: {}", e);
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
