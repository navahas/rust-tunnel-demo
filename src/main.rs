use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    select,
};

async fn handle_server_connection(mut client_stream: TcpStream) {
    let mut target = match TcpStream::connect("127.0.0.1:5435").await {
        Ok(stream) => stream,
        Err(e) => {
            eprintln!("Failed to connect to target: {}", e);
            return;
        }
    };

    println!("Connected to target on port 5435");

    let mut client_buf = vec![0u8; 8192];
    let mut target_buf = vec![0u8; 8192];
    let mut bytes_sent = 0;
    let mut bytes_received = 0;

    loop {
        select! {
            result = client_stream.read(&mut client_buf) => {
                match result {
                    Ok(0) => {
                        println!("Client closed connection");
                        break;
                    }
                    Ok(n) => {
                        bytes_sent += n;
                        println!("Forwarding {} bytes to target (total sent: {})", n, bytes_sent);
                        if let Err(e) = target.write_all(&client_buf[..n]).await {
                            eprintln!("Failed to write to target: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        eprintln!("Failed to read from client: {}", e);
                        break;
                    }
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
                        println!("Forwarding {} bytes to client (total received: {})", n, bytes_received);
                        if let Err(e) = client_stream.write_all(&target_buf[..n]).await {
                            eprintln!("Failed to write to client: {}", e);
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
        "Connection closed. Total bytes: sent={}, received={}",
        bytes_sent, bytes_received
    );
}

async fn handle_client_connection(mut server_stream: TcpStream) {
    let mut target = match TcpStream::connect("127.0.0.1:9051").await {
        Ok(stream) => stream,
        Err(e) => {
            eprintln!("Failed to connect to server: {}", e);
            return;
        }
    };

    println!("Connected to server on port 9051");

    let mut client_buf = vec![0u8; 8192];
    let mut server_buf = vec![0u8; 8192];
    let mut bytes_sent = 0;
    let mut bytes_received = 0;

    loop {
        select! {
            result = server_stream.read(&mut client_buf) => {
                match result {
                    Ok(0) => {
                        println!("Client closed connection");
                        break;
                    }
                    Ok(n) => {
                        bytes_sent += n;
                        println!("Forwarding {} bytes to server (total sent: {})", n, bytes_sent);
                        if let Err(e) = target.write_all(&client_buf[..n]).await {
                            eprintln!("Failed to write to server: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        eprintln!("Failed to read from client: {}", e);
                        break;
                    }
                }
            }
            result = target.read(&mut server_buf) => {
                match result {
                    Ok(0) => {
                        println!("Server closed connection");
                        break;
                    }
                    Ok(n) => {
                        bytes_received += n;
                        println!("Forwarding {} bytes to client (total received: {})", n, bytes_received);
                        if let Err(e) = server_stream.write_all(&server_buf[..n]).await {
                            eprintln!("Failed to write to client: {}", e);
                            break;
                        }
                    }
                    Err(e) => {
                        eprintln!("Failed to read from server: {}", e);
                        break;
                    }
                }
            }
        }
    }

    println!(
        "Connection closed. Total bytes: sent={}, received={}",
        bytes_sent, bytes_received
    );
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("server") => {
            let listener = TcpListener::bind("127.0.0.1:9051").await?;
            println!("Server listening on port 9051, forwarding to port 5435");

            while let Ok((stream, addr)) = listener.accept().await {
                println!("New server connection from {}", addr);
                tokio::spawn(async move {
                    handle_server_connection(stream).await;
                });
            }
        }
        Some("client") => {
            let listener = TcpListener::bind("127.0.0.1:5439").await?;
            println!("Client listening on port 5439, forwarding to server on port 9051");

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
