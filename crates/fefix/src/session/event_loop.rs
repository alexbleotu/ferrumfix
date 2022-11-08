use crate::tagvalue::{DecodeError, DecoderStreaming, Message};
use crate::StreamingDecoder;
use futures::select;
use futures::{AsyncRead, AsyncReadExt, FutureExt};
use futures_timer::Delay;
use std::io;
use std::time::Duration;
use std::time::Instant;

/// Asynchronous, executor-agnostic low-level event loop for FIX connectors.
///
/// This event loop allows FIX connectors to delegate event-tracking logic to a
/// single entity. This event loop keeps track of such events within a FIX
/// session. See [`LlEvent`] for more information.
#[derive(Debug)]
pub struct LlEventLoop<I> {
    decoder: DecoderStreaming<Vec<u8>>,
    input: I,
    heartbeat: Duration,
    heartbeat_soft_tolerance: Duration,
    heartbeat_hard_tolerance: Duration,
    last_reset: Instant,
    last_heartbeat: Instant,
    is_alive: bool,
}

impl<I> LlEventLoop<I>
where
    I: AsyncRead + std::marker::Unpin,
{
    /// Creates a new [`LlEventLoop`] with the provided `decoder` and
    /// `heartbeat`. Events will be read from `input`.
    pub fn new(decoder: DecoderStreaming<Vec<u8>>, input: I, heartbeat: Duration) -> Self {
        let heartbeat_soft_tolerance = heartbeat * 2;
        let heartbeat_hard_tolerance = heartbeat * 3;
        Self {
            decoder,
            input,
            heartbeat,
            heartbeat_soft_tolerance,
            heartbeat_hard_tolerance,
            last_reset: Instant::now(),
            last_heartbeat: Instant::now(),
            is_alive: true,
        }
    }

    /// How long after a missed `Heartbeat <0>` should we send a `TestRequest
    /// <1>`?
    pub fn set_soft_tolerance(&mut self, soft_tolerance: Duration) {
        self.heartbeat_soft_tolerance = soft_tolerance;
    }

    /// How long after a missed `Heartbeat <0>` should we send a `Logout <5>`?
    pub fn set_hard_tolerance(&mut self, hard_tolerance: Duration) {
        self.heartbeat_hard_tolerance = hard_tolerance;
    }

    pub async fn next_event<'a>(&'a mut self) -> Option<LlEvent<'a>> {
        // Clear the  decoder for the next message - assumption is the message has been handled
        if self.decoder.is_ready() {
            self.decoder.clear();
        }

        loop {
            if !self.is_alive {
                return None;
            }

            let now = Instant::now();
            let mut timer_heartbeat = Delay::new(now - self.last_heartbeat + self.heartbeat).fuse();
            let mut timer_test_request =
                Delay::new(now - self.last_reset + self.heartbeat_soft_tolerance).fuse();
            let mut timer_logout =
                Delay::new(now - self.last_reset + self.heartbeat_hard_tolerance).fuse();
            let buf = self.decoder.fillable();
            let mut read_result = self.input.read(buf).fuse();

            select! {
                read_result = read_result => {
                    match read_result {
                        Err(e) => {
                            return Some(LlEvent::IoError(e));
                        }
                        Ok(num_bytes) => {
                            self.decoder.add_bytes_read(num_bytes);

                            // num bytes required is total bytes from the header, num_bytes_read is
                            // total read so far from (add_bytes_read). once they meet then the
                            // buffer is done.  NOTE: could just make add_bytes_read return a value
                            // indicating if there are more bytes to read for this message.
                            if self.decoder.num_bytes_read() < self.decoder.num_bytes_required() {
                                continue;
                            }

                            let result = self.decoder.try_parse();

                            match result {
                                Ok(Some(())) => {
                                    let msg = self.decoder.message();
                                    return Some(LlEvent::Message(msg));
                                }
                                Ok(None) => {
                                    continue;
                                }
                                Err(err) => {
                                    self.is_alive = false;
                                    return Some(LlEvent::BadMessage(err))
                                }
                            }
                        }
                    };
                },
                () = timer_heartbeat => {
                    self.last_heartbeat = Instant::now();
                    return Some(LlEvent::Heartbeat);
                },
                () = timer_test_request => {
                    return Some(LlEvent::TestRequest);
                },
                () = timer_logout => {
                    self.is_alive = false;
                    return Some(LlEvent::Logout);
                }
            }
        }
    }

    /// Resets the FIX counterparty's `Heartbeat <0>` -associated timers.
    pub fn ping_heartbeat(&mut self) {
        self.last_reset = Instant::now();
    }
}

/// A low level event produced by a [`LlEventLoop`].
#[derive(Debug)]
pub enum LlEvent<'a> {
    /// Incoming FIX message.
    Message(Message<'a, &'a [u8]>),
    /// Tried to parse an incoming FIX message, but got illegal data.
    BadMessage(DecodeError),
    /// I/O error at the transport layer.
    IoError(io::Error),
    /// Time to send a new `HeartBeat <0>` message.
    Heartbeat,
    /// The FIX counterparty has missed the `Heartbeat <0>` deadline by some
    /// amount of time, and it's time to send a `Test Request <1>`
    /// message to check what's going on.
    TestRequest,
    /// The FIX counterparty has missed the `Heartbeat <0>` deadline by some
    /// amount of time, and it's stopped responding. It's time to
    /// disconnect via a `Logout <5>` message.
    Logout,
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::tagvalue::{Config, Decoder, Encoder};
    use tokio::io::AsyncWriteExt;
    use tokio::net::{TcpListener, TcpStream};
    use tokio_util::compat::*;
    use crate::{GetConfig, SetField};
    use crate::field_types::Timestamp;

    async fn produce_events(events: Vec<(Vec<u8>, Duration)>) -> TcpStream {
        let tcp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_addr = tcp_listener.local_addr().unwrap();

        tokio::spawn(async move {
            let mut stream = TcpStream::connect(local_addr).await.unwrap();
            for (event_bytes, delay) in events.iter() {
                stream.write(event_bytes.as_slice()).await.unwrap();
                tokio::time::sleep(*delay).await;
            }
        });

        tcp_listener.accept().await.unwrap().0
    }

    async fn new_event_loop(
        events: Vec<(Vec<u8>, Duration)>,
    ) -> LlEventLoop<Compat<TcpStream>> {
        let input = produce_events(events).await;
        let mut decoder = Decoder::<Config>::new(crate::Dictionary::fix44()).streaming(vec![]);

        LlEventLoop::new(
            decoder,
            input.compat(),
            Duration::from_secs(3),
        )
    }

    #[tokio::test]
    async fn dead_input_triggers_logout() {
        let mut event_loop = new_event_loop(vec![(b"8".to_vec(), Duration::from_secs(10))]).await;
        let event = event_loop.next_event().await;
        assert!(matches!(event, Some(LlEvent::Heartbeat)));
        let event = event_loop.next_event().await;
        assert!(
            matches!(event, Some(LlEvent::Heartbeat))
                || matches!(event, Some(LlEvent::TestRequest))
        );
    }

    /// Test receiving large messages received over multiple parts
    #[tokio::test]
    async fn test_multi_part_messages() {
        // Simulate a large orderbook
        let mut encoder = Encoder::<Config>::new();
        let mut buffer = Vec::<u8>::new();
        let mut input_msg = encoder.start_message(b"FIX.4.4", &mut buffer, b"W");
        input_msg.set(35, "W");
        input_msg.set(56, "SENDER");
        input_msg.set(49, "TARGET");
        input_msg.set(34, 1);
        input_msg.set(52, Timestamp::utc_now());
        input_msg.set(262, "test");
        input_msg.set(155, "ETHUSDT");
        input_msg.set(268, 8000);
        // Offers
        for i in 0..2000 {
            input_msg.set(269, "1");
            input_msg.set(270, 1500.0 + i as f64 * 0.01);
            input_msg.set(271, 1.0 + i as f64 * 0.01);
        }
        // Bids
        for i in 0..6000 {
            input_msg.set(269, "0");
            input_msg.set(270, 1499.0 - i as f64 * 0.01);
            input_msg.set(271, 1.0 + i as f64 * 0.01);
        }
        let (msg, _) = input_msg.done();

        let mut parts = vec![];
        let mut used = 0;
        let step = 10000;
        while used < msg.len() {
            if (used + step) > msg.len() {
                parts.push((msg[used..msg.len()].to_vec(), Duration::from_secs(0)));
                break;
            }
            parts.push((msg[used..used+step].to_vec(), Duration::from_secs(0)));
            used += step;
        }
        let mut event_loop = new_event_loop(parts).await;
        let event = event_loop.next_event().await.unwrap();
        match  event {
            LlEvent::Message(received_msg) => {
                assert_eq!(received_msg.as_bytes(), msg, "Partial message received")
            }
            _ => panic!("Expected message")
        }
    }
}
