use std::{
    collections::VecDeque,
    io,
    task::{Context, Poll, Waker},
    time::Duration,
};

use futures::{future::BoxFuture, AsyncReadExt, AsyncWriteExt, FutureExt};
use futures_timer::Delay;
use libp2p::{
    core::upgrade::ReadyUpgrade,
    swarm::{
        handler::{ConnectionEvent, FullyNegotiatedInbound, FullyNegotiatedOutbound},
        ConnectionHandler, ConnectionHandlerEvent, StreamUpgradeError, SubstreamProtocol,
    },
    Stream, StreamProtocol,
};
use nomos_mix_message::{is_noise, MSG_SIZE, NOISE};
use nomos_mix_queue::{NonMixQueue, Queue};

use crate::behaviour::Config;

const PROTOCOL_NAME: StreamProtocol = StreamProtocol::new("/nomos/mix/0.1.0");

/// A [`ConnectionHandler`] that handles the mix protocol.
pub struct MixConnectionHandler {
    inbound_substream: Option<MsgRecvFuture>,
    outbound_substream: Option<OutboundSubstreamState>,
    interval: Duration, // TODO: use absolute time
    timer: Delay,
    queue: Box<dyn Queue<Vec<u8>> + Send>,
    pending_events_to_behaviour: VecDeque<ToBehaviour>,
    waker: Option<Waker>,
}

type MsgSendFuture = BoxFuture<'static, Result<Stream, io::Error>>;
type MsgRecvFuture = BoxFuture<'static, Result<(Stream, Vec<u8>), io::Error>>;

enum OutboundSubstreamState {
    /// A request to open a new outbound substream is being processed.
    PendingOpenSubstream,
    /// An outbound substream is open and ready to send messages.
    Idle(Stream),
    /// A message is being sent on the outbound substream.
    PendingSend(MsgSendFuture),
}

impl MixConnectionHandler {
    pub fn new(config: &Config) -> Self {
        let interval_sec = 1.0 / config.transmission_rate;
        let interval = Duration::from_millis((interval_sec * 1000.0) as u64);
        Self {
            inbound_substream: None,
            outbound_substream: None,
            interval,
            timer: Delay::new(interval),
            queue: Box::new(NonMixQueue::new(NOISE.to_vec())),
            pending_events_to_behaviour: VecDeque::new(),
            waker: None,
        }
    }

    fn try_wake(&mut self) {
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }
}

#[derive(Debug)]
pub enum FromBehaviour {
    /// A message to be sent to the connection.
    Message(Vec<u8>),
}

#[derive(Debug)]
pub enum ToBehaviour {
    /// An outbound substream has been successfully upgraded for the mix protocol.
    FullyNegotiatedOutbound,
    /// An outbound substream was failed to be upgraded for the mix protocol.
    NegotiationFailed,
    /// A message has been received from the connection.
    Message(Vec<u8>),
    /// An IO error from the connection
    IOError(io::Error),
}

impl ConnectionHandler for MixConnectionHandler {
    type FromBehaviour = FromBehaviour;
    type ToBehaviour = ToBehaviour;
    type InboundProtocol = ReadyUpgrade<StreamProtocol>;
    type InboundOpenInfo = ();
    type OutboundProtocol = ReadyUpgrade<StreamProtocol>;
    type OutboundOpenInfo = ();

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        SubstreamProtocol::new(ReadyUpgrade::new(PROTOCOL_NAME), ())
    }

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<
        ConnectionHandlerEvent<Self::OutboundProtocol, Self::OutboundOpenInfo, Self::ToBehaviour>,
    > {
        // Process pending events to be sent to the behaviour
        if let Some(event) = self.pending_events_to_behaviour.pop_front() {
            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(event));
        }

        // Process inbound stream
        tracing::debug!("Processing inbound stream");
        if let Some(msg_recv_fut) = self.inbound_substream.as_mut() {
            match msg_recv_fut.poll_unpin(cx) {
                Poll::Ready(Ok((stream, msg))) => {
                    tracing::debug!("Received message from inbound stream. Notifying behaviour...");
                    self.inbound_substream = Some(recv_msg(stream).boxed());
                    if !is_noise(&msg) {
                        return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                            ToBehaviour::Message(msg),
                        ));
                    }
                }
                Poll::Ready(Err(e)) => {
                    tracing::error!("Failed to receive message from inbound stream: {:?}", e);
                    self.inbound_substream = None;
                }
                Poll::Pending => {}
            }
        }

        // Process outbound stream
        tracing::debug!("Processing outbound stream");
        loop {
            match self.outbound_substream.take() {
                // If the request to open a new outbound substream is still being processed, wait more.
                Some(OutboundSubstreamState::PendingOpenSubstream) => {
                    self.outbound_substream = Some(OutboundSubstreamState::PendingOpenSubstream);
                    self.waker = Some(cx.waker().clone());
                    return Poll::Pending;
                }
                // If the substream is idle, and if it's time to send a message, send it.
                Some(OutboundSubstreamState::Idle(stream)) => match self.timer.poll_unpin(cx) {
                    Poll::Ready(_) => {
                        let msg = self.queue.pop();
                        tracing::debug!("Sending message to outbound stream: {:?}", msg);
                        self.outbound_substream = Some(OutboundSubstreamState::PendingSend(
                            send_msg(stream, msg).boxed(),
                        ));
                        self.timer.reset(self.interval);
                    }
                    Poll::Pending => {
                        self.outbound_substream = Some(OutboundSubstreamState::Idle(stream));
                        self.waker = Some(cx.waker().clone());
                        return Poll::Pending;
                    }
                },
                // If a message is being sent, check if it's done.
                Some(OutboundSubstreamState::PendingSend(mut msg_send_fut)) => {
                    match msg_send_fut.poll_unpin(cx) {
                        Poll::Ready(Ok(stream)) => {
                            tracing::debug!("Message sent to outbound stream");
                            self.outbound_substream = Some(OutboundSubstreamState::Idle(stream));
                        }
                        Poll::Ready(Err(e)) => {
                            tracing::error!("Failed to send message to outbound stream: {:?}", e);
                            self.outbound_substream = None;
                            return Poll::Ready(ConnectionHandlerEvent::NotifyBehaviour(
                                ToBehaviour::IOError(e),
                            ));
                        }
                        Poll::Pending => {
                            self.outbound_substream =
                                Some(OutboundSubstreamState::PendingSend(msg_send_fut));
                            self.waker = Some(cx.waker().clone());
                            return Poll::Pending;
                        }
                    }
                }
                // If there is no outbound substream, request to open a new one.
                None => {
                    self.outbound_substream = Some(OutboundSubstreamState::PendingOpenSubstream);
                    return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                        protocol: SubstreamProtocol::new(ReadyUpgrade::new(PROTOCOL_NAME), ()),
                    });
                }
            }
        }
    }

    fn on_behaviour_event(&mut self, event: Self::FromBehaviour) {
        match event {
            FromBehaviour::Message(msg) => {
                self.queue.push(msg);
            }
        }
    }

    fn on_connection_event(
        &mut self,
        event: ConnectionEvent<
            Self::InboundProtocol,
            Self::OutboundProtocol,
            Self::InboundOpenInfo,
            Self::OutboundOpenInfo,
        >,
    ) {
        match event {
            ConnectionEvent::FullyNegotiatedInbound(FullyNegotiatedInbound {
                protocol: stream,
                ..
            }) => {
                tracing::debug!("FullyNegotiatedInbound: Creating inbound substream");
                self.inbound_substream = Some(recv_msg(stream).boxed())
            }
            ConnectionEvent::FullyNegotiatedOutbound(FullyNegotiatedOutbound {
                protocol: stream,
                ..
            }) => {
                tracing::debug!("FullyNegotiatedOutbound: Creating outbound substream");
                self.outbound_substream = Some(OutboundSubstreamState::Idle(stream));
                self.pending_events_to_behaviour
                    .push_back(ToBehaviour::FullyNegotiatedOutbound);
            }
            ConnectionEvent::DialUpgradeError(e) => {
                tracing::error!("DialUpgradeError: {:?}", e);
                match e.error {
                    StreamUpgradeError::NegotiationFailed => {
                        self.pending_events_to_behaviour
                            .push_back(ToBehaviour::NegotiationFailed);
                    }
                    StreamUpgradeError::Io(e) => {
                        self.pending_events_to_behaviour
                            .push_back(ToBehaviour::IOError(e));
                    }
                    StreamUpgradeError::Timeout => {
                        self.pending_events_to_behaviour
                            .push_back(ToBehaviour::IOError(io::Error::new(
                                io::ErrorKind::TimedOut,
                                "mix protocol negotiation timed out",
                            )));
                    }
                    StreamUpgradeError::Apply(_) => unreachable!(),
                }
            }
            event => {
                tracing::debug!("Ignoring connection event: {:?}", event)
            }
        }

        self.try_wake();
    }
}

/// Write a message to the stream
async fn send_msg(mut stream: Stream, msg: Vec<u8>) -> io::Result<Stream> {
    stream.write_all(&msg).await?;
    stream.flush().await?;
    Ok(stream)
}

/// Read a fixed-length message from the stream
// TODO: Consider handling variable-length messages
async fn recv_msg(mut stream: Stream) -> io::Result<(Stream, Vec<u8>)> {
    let mut buf = vec![0; MSG_SIZE];
    stream.read_exact(&mut buf).await?;
    Ok((stream, buf))
}