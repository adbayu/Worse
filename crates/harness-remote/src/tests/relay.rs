//! Relay bridge end to end: the test plays the relay (a local WebSocket server) and a phone speaking the
//! E2E protocol, against the real loopback server (docs/plans/remote-relay.md §2–4).

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use futures_util::{SinkExt as _, StreamExt as _};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use super::*;
use crate::relay::crypto::{Assembler, Keys, HELLO};
use crate::{RelayIdentity, RelayStatus};

const CID: u32 = 5;

struct Phone {
    relay: WebSocketStream<TcpStream>,
    keys: Keys,
    asm: Assembler,
}

impl Phone {
    async fn send(&mut self, message: &Value) {
        for frame in self.keys.seal_message(message.to_string().as_bytes()) {
            let data = [CID.to_be_bytes().as_slice(), &frame].concat();
            self.relay.send(Message::binary(data)).await.unwrap();
        }
    }

    /// Next frame for the phone: `Ok(message)` or `Err(control text)`.
    async fn next(&mut self) -> Result<Value, String> {
        loop {
            let message = tokio::time::timeout(TIMEOUT, self.relay.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            match message {
                Message::Text(text) if text.as_str() != "ping" => return Err(text.to_string()),
                Message::Binary(data) => {
                    assert_eq!(&data[..4], CID.to_be_bytes().as_slice());
                    let plain = self.keys.open(&data[4..]).unwrap();
                    if let Some(message) = self.asm.push(&plain).unwrap() {
                        return Ok(serde_json::from_slice(&message).unwrap());
                    }
                }
                _ => {}
            }
        }
    }

    async fn call(
        &mut self,
        id: u64,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: &str,
    ) -> Value {
        let mut headers = json!({ "content-type": "application/json" });
        if let Some(token) = token {
            headers["authorization"] = json!(format!("Bearer {token}"));
        }
        self.send(&json!({
            "t": "req", "id": id, "method": method, "path": path, "headers": headers,
            "body": STANDARD.encode(body),
        }))
        .await;
        let res = self.next().await.unwrap();
        assert_eq!(res["t"], "res");
        assert_eq!(res["id"], id);
        res
    }
}

fn body(res: &Value) -> Value {
    serde_json::from_slice(&STANDARD.decode(res["body"].as_str().unwrap()).unwrap()).unwrap()
}

#[tokio::test]
async fn phone_reaches_the_api_through_the_relay() {
    let server = Server::start().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_url = format!("http://{}", listener.local_addr().unwrap());
    let handle = server.handle.start_relay(&relay_url).unwrap();
    let (tcp, _) = tokio::time::timeout(TIMEOUT, listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut relay = tokio_tungstenite::accept_async(tcp).await.unwrap();

    // The relay announces a phone; the phone says hello with its salt.
    relay
        .send(Message::text(format!("{{\"t\":\"open\",\"c\":{CID}}}")))
        .await
        .unwrap();
    let salt_c = [3_u8; 16];
    let hello = [CID.to_be_bytes().as_slice(), &[HELLO], &salt_c].concat();
    relay.send(Message::binary(hello)).await.unwrap();
    let answer = loop {
        if let Message::Binary(data) = relay.next().await.unwrap().unwrap() {
            break data;
        }
    };
    assert_eq!(answer.len(), 4 + 1 + 16);
    assert_eq!(answer[4], HELLO);
    let identity = RelayIdentity::load_or_create(&server.dir).unwrap();
    let keys = Keys::phone(identity.secret(), &salt_c, &answer[5..]);
    let mut phone = Phone {
        relay,
        keys,
        asm: Assembler::default(),
    };
    assert_eq!(handle.status(), RelayStatus::Online { phones: 1 });

    let health = phone.call(1, "GET", "/healthz", None, "").await;
    assert_eq!(health["status"], 200);
    assert_eq!(body(&health), json!({ "ok": true }));

    // Static files and other paths never reach the desktop through the relay.
    assert_eq!(
        phone.call(2, "GET", "/assets/app.js", None, "").await["status"],
        404
    );
    assert_eq!(
        phone.call(3, "GET", "/api/v1/status", None, "").await["status"],
        401
    );

    // Pairing works as on the local server; the token comes back in Set-Cookie.
    let offer = server.handle.new_pair_offer("Phone");
    let pair = phone
        .call(
            4,
            "POST",
            "/api/v1/pair",
            None,
            &json!({ "token": offer.token }).to_string(),
        )
        .await;
    assert_eq!(pair["status"], 200);
    let cookie = pair["headers"]["set-cookie"].as_str().unwrap();
    let token = cookie
        .strip_prefix("harness_remote=")
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    assert_eq!(
        phone
            .call(5, "GET", "/api/v1/status", Some(&token), "")
            .await["status"],
        200
    );

    // The event stream is relayed message by message.
    phone
        .send(&json!({ "t": "ws-open", "id": 9, "path": "/api/v1/events",
                       "headers": { "authorization": format!("Bearer {token}") } }))
        .await;
    let first = phone.next().await.unwrap();
    assert_eq!(first["t"], "ws");
    assert_eq!(first["id"], 9);
    let hello: Value = serde_json::from_str(first["data"].as_str().unwrap()).unwrap();
    assert_eq!(hello["type"], "hello");

    // A frame that does not decrypt gets the phone kicked.
    let junk = [CID.to_be_bytes().as_slice(), &[2_u8; 40]].concat();
    phone.relay.send(Message::binary(junk)).await.unwrap();
    loop {
        if let Err(text) = phone.next().await {
            assert_eq!(text, format!("{{\"t\":\"kick\",\"c\":{CID}}}"));
            break;
        }
    }

    handle.stop().await;
}
