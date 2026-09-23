use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use haimen_xiaozhi::{AudioFrame, PlaybackEvent, ResponseStrategy};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
struct TextStrategy;
#[async_trait]
impl ResponseStrategy for TextStrategy {
    fn name(&self) -> &'static str {
        "typed-test"
    }
    async fn generate_response(
        &self,
        _: Vec<AudioFrame>,
        _: &str,
    ) -> Result<Vec<AudioFrame>, String> {
        panic!("Typed input must never enter ASR/audio response path")
    }
    async fn generate_text_response_stream(
        &self,
        text: String,
        _: &str,
        tx: mpsc::Sender<PlaybackEvent>,
    ) -> Result<(), String> {
        if text == "fail" {
            return Err("test generation failure".into());
        }
        if text == "wait" {
            std::future::pending::<()>().await;
        }
        tx.send(PlaybackEvent::Stt(text.clone())).await.unwrap();
        tx.send(PlaybackEvent::LlmSentence(format!("reply:{text}")))
            .await
            .unwrap();
        if text != "text-only" {
            tx.send(PlaybackEvent::Audio(AudioFrame {
                timestamp: 0,
                data: vec![1, 2, 3],
            }))
            .await
            .unwrap();
        }
        Ok(())
    }
}
async fn connect() -> (Socket, tokio::task::JoinHandle<()>) {
    let app = haimen_xiaozhi::add_routes(axum::Router::new(), Arc::new(TextStrategy));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{address}/xiaozhi/ws"))
        .await
        .unwrap();
    send(&mut socket, json!({"type":"hello","version":2,"audio_params":{"format":"opus","sample_rate":24000,"channels":1,"frame_duration":60}})).await;
    assert_eq!(next_json(&mut socket).await["type"], "hello");
    (socket, server)
}
async fn send(socket: &mut Socket, value: Value) {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
}
async fn next(socket: &mut Socket) -> Message {
    tokio::time::timeout(Duration::from_secs(3), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}
async fn next_json(socket: &mut Socket) -> Value {
    match next(socket).await {
        Message::Text(text) => serde_json::from_str(&text).unwrap(),
        other => panic!("Unexpected {other:?}"),
    }
}
#[tokio::test]
async fn typed_turns_return_text_and_audio_and_allow_next_turn() {
    let (mut socket, server) = connect().await;
    for text in ["  你好  ", "第二轮"] {
        send(&mut socket, json!({"type":"text","text":text})).await;
        assert_eq!(next_json(&mut socket).await["text"], text.trim());
        assert_eq!(next_json(&mut socket).await["state"], "start");
        assert_eq!(
            next_json(&mut socket).await["text"],
            format!("reply:{}", text.trim())
        );
        assert!(matches!(next(&mut socket).await, Message::Binary(_)));
        assert_eq!(next_json(&mut socket).await["state"], "stop");
    }
    socket.close(None).await.unwrap();
    server.abort();
}
#[tokio::test]
async fn validates_input_reports_failures_and_finishes_without_audio() {
    let (mut socket, server) = connect().await;
    for text in [" ".to_string(), "x".repeat(32769)] {
        send(&mut socket, json!({"type":"text","text":text})).await;
        assert_eq!(next_json(&mut socket).await["code"], "invalid_text");
    }
    send(&mut socket, json!({"type":"text","text":"fail"})).await;
    assert_eq!(next_json(&mut socket).await["code"], "generation_error");
    send(&mut socket, json!({"type":"text","text":"text-only"})).await;
    assert_eq!(next_json(&mut socket).await["type"], "stt");
    assert_eq!(next_json(&mut socket).await["text"], "reply:text-only");
    assert_eq!(next_json(&mut socket).await["state"], "stop");
    socket.close(None).await.unwrap();
    server.abort();
}
#[tokio::test]
async fn abort_while_waiting_for_first_reply_restores_session() {
    let (mut socket, server) = connect().await;
    send(&mut socket, json!({"type":"text","text":"wait"})).await;
    send(&mut socket, json!({"type":"abort"})).await;
    assert_eq!(next_json(&mut socket).await["state"], "stop");
    send(&mut socket, json!({"type":"text","text":"fail"})).await;
    assert_eq!(next_json(&mut socket).await["code"], "generation_error");
    socket.close(None).await.unwrap();
    server.abort();
}
