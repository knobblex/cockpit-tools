//! CONNECT adapter: resolve Google's daily endpoint locally while preserving TLS.
use std::{io, net::SocketAddr, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_util::sync::CancellationToken;

async fn header(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    while bytes.len() < 8192 {
        bytes.push(stream.read_u8().await?);
        if bytes.ends_with(b"\r\n\r\n") {
            return Ok(bytes);
        }
    }
    Err(io::Error::other("CONNECT header too large"))
}

async fn tunnel(parent: SocketAddr, target: &str) -> io::Result<TcpStream> {
    let mut stream = TcpStream::connect(parent).await?;
    stream
        .write_all(format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n").as_bytes())
        .await?;
    let response = header(&mut stream).await?;
    if !response.starts_with(b"HTTP/1.1 200 ") && !response.starts_with(b"HTTP/1.0 200 ") {
        return Err(io::Error::other("Parent proxy refused CONNECT"));
    }
    Ok(stream)
}

async fn forward(mut client: TcpStream, parent: SocketAddr) -> io::Result<()> {
    let request = tokio::time::timeout(Duration::from_secs(5), header(&mut client)).await??;
    let line = std::str::from_utf8(&request).map_err(|_| io::Error::other("Invalid CONNECT"))?;
    let fields: Vec<_> = line
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .collect();
    if fields.len() != 3 || fields[0] != "CONNECT" {
        return Err(io::Error::other("CONNECT required"));
    }
    let host = fields[1]
        .strip_suffix(":443")
        .ok_or_else(|| io::Error::other("HTTPS required"))?;
    if !host.ends_with(".googleapis.com")
        && !matches!(
            host,
            "raw.githubusercontent.com" | "api.github.com" | "github.com"
        )
    {
        return Err(io::Error::other("Unsupported upstream host"));
    }
    let targets = if host == "daily-cloudcode-pa.googleapis.com" {
        let mut addresses: Vec<_> = tokio::net::lookup_host((host, 443))
            .await?
            .filter(|address| address.is_ipv4())
            .collect();
        addresses.sort_unstable();
        addresses.dedup();
        addresses.reverse();
        addresses
            .into_iter()
            .map(|address| address.to_string())
            .collect()
    } else {
        vec![fields[1].to_owned()]
    };
    client
        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
        .await?;
    // Retry only the initial TLS record, before any HTTP request or model input.
    // TLS authentication stays between CLIProxyAPI and Google; never intercept it.
    let mut prefix = [0u8; 5];
    client.read_exact(&mut prefix).await?;
    let length = u16::from_be_bytes([prefix[3], prefix[4]]) as usize;
    if prefix[0] != 22 || length > 16384 {
        return Err(io::Error::other("TLS ClientHello required"));
    }
    let mut hello = prefix.to_vec();
    hello.resize(5 + length, 0);
    client.read_exact(&mut hello[5..]).await?;
    forward_hello(client, parent, targets, hello).await
}

async fn forward_hello(
    mut client: TcpStream,
    parent: SocketAddr,
    targets: Vec<String>,
    hello: Vec<u8>,
) -> io::Result<()> {
    for target in targets {
        let attempt = async {
            let mut upstream = tunnel(parent, &target).await?;
            upstream.write_all(&hello).await?;
            let first = upstream.read_u8().await?;
            // A fatal TLS alert means this address cannot complete a handshake.
            if first != 22 {
                return Err(io::Error::other("Upstream TLS handshake failed"));
            }
            Ok::<_, io::Error>((upstream, first))
        };
        if let Ok(Ok((mut upstream, first))) =
            tokio::time::timeout(Duration::from_secs(5), attempt).await
        {
            client.write_all(&[first]).await?;
            tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
            return Ok(());
        }
    }
    Err(io::Error::other("No reachable upstream TLS address"))
}

pub async fn serve(listener: TcpListener, parent: SocketAddr, cancel: CancellationToken) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            result = listener.accept() => {
                let Ok((stream, _)) = result else { return };
                let cancel = cancel.clone();
                tokio::spawn(async move {
                    tokio::select! {
                        _ = cancel.cancelled() => {},
                        _ = tokio::time::timeout(Duration::from_secs(360), forward(stream, parent)) => {},
                    }
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn retries_tls_hello_before_forwarding_application_bytes() {
        let parent = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let parent_addr = parent.local_addr().unwrap();
        let proxy = tokio::spawn(async move {
            for index in 0..2 {
                let (mut socket, _) = parent.accept().await.unwrap();
                let request = header(&mut socket).await.unwrap();
                assert!(String::from_utf8(request).unwrap().contains(if index == 0 {
                    "192.0.2.1:443"
                } else {
                    "192.0.2.2:443"
                }));
                socket.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
                let mut hello = [0; 5];
                socket.read_exact(&mut hello).await.unwrap();
                assert_eq!(&hello, b"hello");
                if index == 1 {
                    socket.write_all(&[22]).await.unwrap();
                    let mut body = [0; 7];
                    socket.read_exact(&mut body).await.unwrap();
                    assert_eq!(&body, b"payload");
                    socket.write_all(b"reply").await.unwrap();
                }
            }
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server, _) = listener.accept().await.unwrap();
        let adapter = tokio::spawn(forward_hello(
            server,
            parent_addr,
            vec!["192.0.2.1:443".into(), "192.0.2.2:443".into()],
            b"hello".to_vec(),
        ));
        assert_eq!(client.read_u8().await.unwrap(), 22);
        client.write_all(b"payload").await.unwrap();
        let mut reply = [0; 5];
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(&reply, b"reply");
        drop(client);
        adapter.await.unwrap().unwrap();
        proxy.await.unwrap();
    }
}
