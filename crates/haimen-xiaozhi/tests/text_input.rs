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
    fn supports_streaming_playback(&self) -> bool {
        true
    }
    async fn generate_response_stream_with_tts(
        &self,
        _: Vec<AudioFrame>,
        _: &str,
        tx: mpsc::Sender<PlaybackEvent>,
        tts_enabled: bool,
    ) -> Result<(), String> {
        assert!(
            !tts_enabled,
            "voice request must carry the muted preference"
        );
        tx.send(PlaybackEvent::Stt("voice question".into()))
            .await
            .unwrap();
        tx.send(PlaybackEvent::LlmSentence("voice reply".into()))
            .await
            .unwrap();
        Ok(())
    }
    async fn generate_text_response_stream_with_tts(
        &self,
        text: String,
        session_id: &str,
        tx: mpsc::Sender<PlaybackEvent>,
        tts_enabled: bool,
    ) -> Result<(), String> {
        if tts_enabled {
            return self
                .generate_text_response_stream(text, session_id, tx)
                .await;
        }
        tx.send(PlaybackEvent::LlmSentence(format!("muted:{text}")))
            .await
            .unwrap();
        if text == "wait" {
            std::future::pending::<()>().await;
        }
        Ok(())
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
        if text == "playing" {
            for i in 0..50 {
                tx.send(PlaybackEvent::Audio(AudioFrame {
                    timestamp: i * 60,
                    data: vec![1, 2, 3],
                }))
                .await
                .map_err(|_| "cancelled".to_string())?;
            }
            std::future::pending::<()>().await;
        }
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

#[tokio::test]
async fn muted_turn_streams_before_completion_can_abort_and_restores_audio_next_turn() {
    let (mut socket, server) = connect().await;
    send(
        &mut socket,
        json!({"type":"text","text":"wait","tts_enabled":false}),
    )
    .await;
    // The strategy never finishes: receipt proves text isn't waiting for audio prebuffer.
    assert_eq!(next_json(&mut socket).await["text"], "muted:wait");
    send(&mut socket, json!({"type":"abort"})).await;
    assert_eq!(next_json(&mut socket).await["state"], "stop");
    send(
        &mut socket,
        json!({"type":"text","text":"quiet","tts_enabled":false}),
    )
    .await;
    assert_eq!(next_json(&mut socket).await["text"], "muted:quiet");
    assert_eq!(next_json(&mut socket).await["state"], "stop");
    send(
        &mut socket,
        json!({"type":"text","text":"audio","tts_enabled":true}),
    )
    .await;
    assert_eq!(next_json(&mut socket).await["type"], "stt");
    assert_eq!(next_json(&mut socket).await["state"], "start");
    assert_eq!(next_json(&mut socket).await["text"], "reply:audio");
    assert!(matches!(next(&mut socket).await, Message::Binary(_)));
    assert_eq!(next_json(&mut socket).await["state"], "stop");
    socket.close(None).await.unwrap();
    server.abort();
}

#[tokio::test]
async fn muted_voice_preference_survives_listen_stop_without_option() {
    let (mut socket, server) = connect().await;
    send(
        &mut socket,
        json!({"type":"listen","state":"start","tts_enabled":false}),
    )
    .await;
    send(&mut socket, json!({"type":"listen","state":"stop"})).await;
    assert_eq!(next_json(&mut socket).await["text"], "voice question");
    assert_eq!(next_json(&mut socket).await["text"], "voice reply");
    assert_eq!(next_json(&mut socket).await["state"], "stop");
    socket.close(None).await.unwrap();
    server.abort();
}

#[tokio::test]
async fn correlated_abort_orders_next_turn_after_old_events() {
    for old_text in ["wait", "playing", "text-only"] {
        let (mut socket, server) = connect().await;
        send(&mut socket, json!({"type":"text","text":old_text})).await;
        if old_text == "playing" {
            loop {
                if matches!(next(&mut socket).await, Message::Binary(_)) {
                    break;
                }
            }
        }
        // For text-only this races with a natural stop; for wait it cancels
        // prebuffering; for playing it cancels the live audio stream.
        send(
            &mut socket,
            json!({"type":"abort","request_id":"replace-1"}),
        )
        .await;
        let mut saw_stop = false;
        loop {
            if let Message::Text(text) = next(&mut socket).await {
                let event: Value = serde_json::from_str(&text).unwrap();
                if event["type"] == "tts" && event["state"] == "stop" {
                    saw_stop = true;
                }
                if event["type"] == "aborted" {
                    assert!(saw_stop);
                    assert_eq!(event["request_id"], "replace-1");
                    break;
                }
            }
        }
        send(
            &mut socket,
            json!({"type":"text","text":"new","tts_enabled":false}),
        )
        .await;
        assert_eq!(next_json(&mut socket).await["text"], "muted:new");
        assert_eq!(next_json(&mut socket).await["state"], "stop");
        // Already-completed replies also acknowledge a correlated abort.
        send(
            &mut socket,
            json!({"type":"abort","request_id":"replace-2"}),
        )
        .await;
        assert_eq!(next_json(&mut socket).await["state"], "stop");
        assert_eq!(next_json(&mut socket).await["request_id"], "replace-2");
        socket.close(None).await.unwrap();
        server.abort();
    }
}
