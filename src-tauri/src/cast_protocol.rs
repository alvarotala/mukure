//! Google Cast v2 framing and the default media receiver's JSON protocol.
//! Framed retains partial reads across select! and request timeouts.
use anyhow::{bail, ensure, Context, Result};
use futures_util::{SinkExt, StreamExt};
use prost::Message;
use rustls::{
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, ServerName, UnixTime},
    ClientConfig, DigitallySignedStruct, SignatureScheme,
};
use serde_json::{json, Value};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::net::TcpStream;
use tokio_rustls::{client::TlsStream, TlsConnector};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

pub const CONNECTION: &str = "urn:x-cast:com.google.cast.tp.connection";
pub const HEARTBEAT: &str = "urn:x-cast:com.google.cast.tp.heartbeat";
pub const RECEIVER: &str = "urn:x-cast:com.google.cast.receiver";
pub const MEDIA: &str = "urn:x-cast:com.google.cast.media";
pub const DEFAULT_APP: &str = "CC1AD845";

// Wire fields from Cast's cast_channel.proto, with the protocol's 64 KiB cap.
#[derive(Clone, PartialEq, Message)]
struct Envelope {
    #[prost(int32, required, tag = "1")]
    protocol_version: i32,
    #[prost(string, required, tag = "2")]
    source_id: String,
    #[prost(string, required, tag = "3")]
    destination_id: String,
    #[prost(string, required, tag = "4")]
    namespace: String,
    #[prost(int32, required, tag = "5")]
    payload_type: i32,
    #[prost(string, optional, tag = "6")]
    payload_utf8: Option<String>,
    #[prost(bytes, optional, tag = "7")]
    payload_binary: Option<Vec<u8>>,
}

#[derive(Debug)]
struct LocalCastCertificate;
impl ServerCertVerifier for LocalCastCertificate {
    // Cast receivers use device-issued certificates rather than web PKI names.
    // This verifier is only used for the discovered/private receiver IP, never
    // for subtitle providers or any other HTTPS traffic. TLS signatures remain checked.
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            signature,
            &rustls::crypto::aws_lc_rs::default_provider().signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            signature,
            &rustls::crypto::aws_lc_rs::default_provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::aws_lc_rs::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

pub struct CastClient {
    wire: Framed<TlsStream<TcpStream>, LengthDelimitedCodec>,
    sender: String,
    request_id: u32,
    pub transport: String,
    pub media_id: i64,
    pub session_id: String,
}
impl CastClient {
    pub async fn connect(address: SocketAddr) -> Result<Self> {
        let socket = tokio::time::timeout(Duration::from_secs(8), TcpStream::connect(address))
            .await
            .context("El Chromecast no respondió")?
            .context("No se pudo conectar al Chromecast")?;
        let config = ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(LocalCastCertificate))
            .with_no_client_auth();
        let stream = tokio::time::timeout(
            Duration::from_secs(8),
            TlsConnector::from(Arc::new(config))
                .connect(ServerName::IpAddress(address.ip().into()), socket),
        )
        .await
        .context("El Chromecast no completó la conexión")??;
        let codec = LengthDelimitedCodec::builder()
            .max_frame_length(64 * 1024)
            .new_codec();
        let mut client = Self {
            wire: Framed::new(stream, codec),
            sender: format!("sender-{}", uuid::Uuid::new_v4().simple()),
            request_id: 0,
            transport: String::new(),
            media_id: 0,
            session_id: String::new(),
        };
        client
            .send(
                "receiver-0",
                CONNECTION,
                json!({"type":"CONNECT","origin":{}}),
            )
            .await?;
        Ok(client)
    }
    pub async fn send(&mut self, destination: &str, namespace: &str, payload: Value) -> Result<()> {
        let envelope = Envelope {
            protocol_version: 0,
            source_id: self.sender.clone(),
            destination_id: destination.into(),
            namespace: namespace.into(),
            payload_type: 0,
            payload_utf8: Some(payload.to_string()),
            payload_binary: None,
        };
        let bytes = envelope.encode_to_vec();
        ensure!(bytes.len() <= 64 * 1024, "Mensaje Cast demasiado grande");
        tokio::time::timeout(
            Duration::from_secs(4),
            self.wire.send(axum::body::Bytes::from(bytes)),
        )
        .await
        .context("El Chromecast no recibe comandos")??;
        Ok(())
    }
    pub async fn next(&mut self) -> Result<(String, Value)> {
        let bytes = self
            .wire
            .next()
            .await
            .context("El Chromecast cerró la conexión")??;
        let envelope = Envelope::decode(bytes)?;
        ensure!(
            envelope.destination_id == self.sender || envelope.destination_id == "*",
            "Respuesta Cast para otra sesión"
        );
        let data = serde_json::from_str(envelope.payload_utf8.as_deref().unwrap_or("{}"))?;
        Ok((envelope.namespace, data))
    }
    pub async fn send_request(
        &mut self,
        destination: &str,
        namespace: &str,
        mut payload: Value,
    ) -> Result<u32> {
        self.request_id += 1;
        let id = self.request_id;
        payload["requestId"] = json!(id);
        self.send(destination, namespace, payload).await?;
        Ok(id)
    }
    pub async fn request(
        &mut self,
        destination: &str,
        namespace: &str,
        payload: Value,
    ) -> Result<Value> {
        let id = self.send_request(destination, namespace, payload).await?;
        tokio::time::timeout(Duration::from_secs(25), async {
            loop {
                let (ns, data) = self.next().await?;
                if ns == HEARTBEAT && data["type"] == "PING" {
                    self.send("receiver-0", HEARTBEAT, json!({"type":"PONG"}))
                        .await?;
                } else if ns == CONNECTION && data["type"] == "CLOSE" {
                    bail!("El Chromecast cerró la sesión");
                } else if ns == namespace && data["requestId"].as_u64() == Some(id as u64) {
                    check_response(&data)?;
                    return Ok(data);
                }
            }
        })
        .await
        .context("El Chromecast no confirmó la operación")?
    }
    pub async fn launch(&mut self) -> Result<()> {
        let response = self
            .request(
                "receiver-0",
                RECEIVER,
                json!({"type":"LAUNCH","appId":DEFAULT_APP}),
            )
            .await?;
        let app = response["status"]["applications"]
            .as_array()
            .and_then(|apps| apps.iter().find(|app| app["appId"] == DEFAULT_APP))
            .context("No se pudo iniciar el reproductor de Chromecast")?;
        self.session_id = app["sessionId"]
            .as_str()
            .context("Chromecast no devolvió una sesión")?
            .into();
        self.transport = app["transportId"]
            .as_str()
            .context("Chromecast no devolvió una sesión")?
            .into();
        self.send(
            &self.transport.clone(),
            CONNECTION,
            json!({"type":"CONNECT","origin":{}}),
        )
        .await
    }
    pub async fn media_request(&mut self, mut payload: Value) -> Result<Value> {
        payload["mediaSessionId"] = json!(self.media_id);
        self.request(&self.transport.clone(), MEDIA, payload).await
    }
    pub async fn stop(&mut self) {
        if self.media_id > 0 {
            let request = self.media_request(json!({"type":"STOP"}));
            let _ = tokio::time::timeout(Duration::from_millis(800), request).await;
        }
    }
}
fn check_response(data: &Value) -> Result<()> {
    match data["type"].as_str().unwrap_or("") {
        "LOAD_FAILED" => bail!("Chromecast no pudo reproducir el video. Probá activar Convertir para TV y revisá que ambos equipos estén en la misma red"),
        "LOAD_CANCELLED" => bail!("El Chromecast canceló la carga"),
        "LAUNCH_ERROR" => bail!("No se pudo abrir el reproductor en el Chromecast"),
        "INVALID_REQUEST" | "INVALID_PLAYER_STATE" => bail!("Chromecast rechazó el comando de reproducción"),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::pki_types::PrivatePkcs8KeyDer;
    use tokio::net::TcpListener;
    use tokio_rustls::{server::TlsStream as ServerStream, TlsAcceptor};

    type MockWire = Framed<ServerStream<TcpStream>, LengthDelimitedCodec>;
    async fn receive(wire: &mut MockWire) -> (Envelope, Value) {
        let envelope = Envelope::decode(wire.next().await.unwrap().unwrap()).unwrap();
        let value = serde_json::from_str(envelope.payload_utf8.as_deref().unwrap()).unwrap();
        (envelope, value)
    }
    async fn reply(wire: &mut MockWire, request: &Envelope, namespace: &str, value: Value) {
        let message = Envelope {
            protocol_version: 0,
            source_id: request.destination_id.clone(),
            destination_id: request.source_id.clone(),
            namespace: namespace.into(),
            payload_type: 0,
            payload_utf8: Some(value.to_string()),
            payload_binary: None,
        };
        wire.send(axum::body::Bytes::from(message.encode_to_vec()))
            .await
            .unwrap();
    }
    // A real TLS socket validates framing, heartbeats, correlation and controls
    // without discovering, connecting to, or playing on any physical device.
    #[tokio::test]
    async fn cast_tls_session_handles_heartbeat_load_controls_and_errors() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let key = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
            let config = rustls::ServerConfig::builder().with_no_client_auth()
                .with_single_cert(vec![key.cert.der().clone()], PrivatePkcs8KeyDer::from(key.signing_key.serialize_der()).into()).unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let stream = TlsAcceptor::from(Arc::new(config)).accept(socket).await.unwrap();
                let mut wire = Framed::new(stream, LengthDelimitedCodec::new());
                let (connect, data) = receive(&mut wire).await;
                assert_eq!(connect.namespace, CONNECTION);
                assert_eq!(connect.destination_id, "receiver-0");
                assert_eq!(data["type"], "CONNECT");
                let (launch, data) = receive(&mut wire).await;
                assert_eq!(data["appId"], DEFAULT_APP);
                assert_eq!(data["type"], "LAUNCH");
                reply(&mut wire, &launch, HEARTBEAT, json!({"type":"PING"})).await;
                let (pong, data_pong) = receive(&mut wire).await;
                assert_eq!(pong.namespace, HEARTBEAT);
                assert_eq!(data_pong["type"], "PONG");
                // A stale response must not complete the current request.
                reply(&mut wire, &launch, RECEIVER, json!({"type":"LAUNCH_ERROR","requestId":999})).await;
                reply(&mut wire, &launch, RECEIVER, json!({"type":"RECEIVER_STATUS","requestId":data["requestId"],
                    "status":{"applications":[{"appId":DEFAULT_APP,"transportId":"transport-test","sessionId":"session-test"}]}})).await;
                let (connect, _) = receive(&mut wire).await;
                assert_eq!(connect.namespace, CONNECTION);
                assert_eq!(connect.destination_id, "transport-test");
                for (index, expected) in ["LOAD", "PAUSE", "SEEK", "STOP", "LOAD"].into_iter().enumerate() {
                    let (envelope, data) = receive(&mut wire).await;
                    assert_eq!(envelope.namespace, MEDIA);
                    assert_eq!(envelope.destination_id, "transport-test");
                    assert_eq!(data["type"], expected);
                    if expected != "LOAD" { assert_eq!(data["mediaSessionId"], 37); }
                    if expected == "SEEK" { assert_eq!(data["currentTime"], 42.5); }
                    if index == 0 { assert_eq!(data["sessionId"], "session-test"); }
                    let response = if index == 4 { json!({"type":"LOAD_FAILED","requestId":data["requestId"]}) }
                        else { json!({"type":"MEDIA_STATUS","requestId":data["requestId"],"status":[{"mediaSessionId":37,"playerState":"PLAYING"}]}) };
                    reply(&mut wire, &envelope, MEDIA, response).await;
                }
            });
            let mut client = CastClient::connect(address).await.unwrap();
            client.launch().await.unwrap();
            let response = client.request(&client.transport.clone(), MEDIA, json!({"type":"LOAD","sessionId":client.session_id,"media":{"contentId":"http://127.0.0.1/test.mp4","contentType":"video/mp4"}})).await.unwrap();
            client.media_id = response["status"][0]["mediaSessionId"].as_i64().unwrap();
            client.media_request(json!({"type":"PAUSE"})).await.unwrap();
            client.media_request(json!({"type":"SEEK","currentTime":42.5})).await.unwrap();
            client.stop().await;
            let error = client.request(&client.transport.clone(), MEDIA, json!({"type":"LOAD"})).await.unwrap_err();
            assert!(error.to_string().contains("Convertir para TV"));
            server.await.unwrap();
        }).await.expect("simulated Cast session timed out");
    }
}
