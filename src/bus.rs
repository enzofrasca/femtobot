use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Mutex};
use tracing::warn;

#[derive(Clone, Debug)]
pub struct InboundMessage {
    pub channel: String,
    pub chat_id: String,
    pub sender_id: String,
    pub content: String,
}

#[derive(Clone, Debug)]
pub struct OutboundMessage {
    pub channel: String,
    pub chat_id: String,
    pub content: String,
}

#[derive(Clone)]
pub struct MessageBus {
    inbound_tx: mpsc::Sender<InboundMessage>,
    outbound_tx: mpsc::Sender<OutboundMessage>,
    inbound_rx: Arc<Mutex<mpsc::Receiver<InboundMessage>>>,
    outbound_broadcast_tx: broadcast::Sender<OutboundMessage>,
}

impl MessageBus {
    pub fn new() -> Self {
        let (inbound_tx, inbound_rx) = mpsc::channel(100);
        let (outbound_tx, mut outbound_rx) = mpsc::channel(100);
        let (outbound_broadcast_tx, _) = broadcast::channel(100);

        let inbound_rx = Arc::new(Mutex::new(inbound_rx));

        let bus = MessageBus {
            inbound_tx,
            outbound_tx,
            inbound_rx: inbound_rx.clone(),
            outbound_broadcast_tx: outbound_broadcast_tx.clone(),
        };

        tokio::spawn(async move {
            while let Some(msg) = outbound_rx.recv().await {
                if let Err(e) = outbound_broadcast_tx.send(msg) {
                    warn!("Failed to broadcast outbound message: {e}");
                }
            }
        });

        bus
    }

    pub async fn publish_inbound(&self, msg: InboundMessage) {
        if let Err(e) = self.inbound_tx.send(msg).await {
            warn!("Failed to publish inbound message: {e}");
        }
    }

    pub async fn publish_outbound(&self, msg: OutboundMessage) {
        if let Err(e) = self.outbound_tx.send(msg).await {
            warn!("Failed to publish outbound message: {e}");
        }
    }

    pub async fn consume_inbound(&self) -> Option<InboundMessage> {
        let mut rx = self.inbound_rx.lock().await;
        rx.recv().await
    }

    pub fn subscribe_outbound(&self) -> broadcast::Receiver<OutboundMessage> {
        self.outbound_broadcast_tx.subscribe()
    }
}
