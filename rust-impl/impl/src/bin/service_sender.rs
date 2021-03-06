// Rust flags
#![warn(nonstandard_style)]
#![warn(future_incompatible)]
#![warn(rust_2018_compatibility)]
#![warn(rust_2018_idioms)]
#![warn(unused)]
#![warn(missing_debug_implementations)]
#![warn(missing_copy_implementations)]
#![warn(trivial_casts)]
#![warn(trivial_numeric_casts)]
#![warn(unsafe_code)]
#![warn(unused_extern_crates)]
#![warn(unused_import_braces)]
#![warn(unused_qualifications)]
#![warn(unused_results)]
#![warn(variant_size_differences)]
#![recursion_limit = "1024"]
// Clippy flags
#![warn(clippy::all)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::missing_errors_doc)]

use rand::thread_rng;
use rand::Rng;
use rust_impl::DeadLockSafeRwLock;
use rust_impl::BUS_PUBLISHERS_SOCKET_ADDRS;
use rust_impl::BUS_ROUTER_SOCKET_ADDR;
use rust_impl::LOG_LEVEL;
use rust_impl::REQUESTS_COUNT_INSIDE_ONE_GROUP;
use rust_impl::RUST_LOG_ENVIRONMENT_VARIABLE_NAME;
use rust_impl::ZEROMQ_ZERO_FLAG;
use std::cmp::Ord;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::convert::From;
use std::env;
use std::iter::Iterator;
use std::rc::Rc;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use uuid::Uuid;
use zeromq_messages::codec::decode_message_kind;
use zeromq_messages::codec::decode_message_payload;
use zeromq_messages::codec::decode_message_uuid;
use zeromq_messages::codec::encode_message;
use zeromq_messages::kind::ZeromqMessageKind;
use zeromq_messages::messages::ValueMultiplicationRequest;
use zeromq_messages::messages::ValueMultiplicationResponse;
use zmq::Context as ZmqContext;
use zmq::Message;
use zmq::SocketType;

const RESEND_REQUESTS_EVERY_DURATION: Duration = Duration::from_secs(5_u64);

type AwaitingRequestsStorage = DeadLockSafeRwLock<HashMap<Uuid, RequestData>>;

#[derive(Debug, Clone)]
struct RequestData {
    value: i64,
    multiplier: i64,
    expected_result: i64,
    last_send_attempt_time: Instant,
}

impl RequestData {
    fn new(value: i64, multiplier: i64) -> Self {
        Self {
            value,
            multiplier,
            expected_result: value * multiplier,
            last_send_attempt_time: Instant::now(),
        }
    }

    fn should_resend_request(&self) -> bool {
        Instant::now().duration_since(self.last_send_attempt_time)
            > RESEND_REQUESTS_EVERY_DURATION
    }

    fn update_last_send_attempt_time(&mut self) {
        self.last_send_attempt_time = Instant::now();
    }
}

#[allow(clippy::too_many_lines)]
fn main() {
    if env::var(RUST_LOG_ENVIRONMENT_VARIABLE_NAME).is_err() {
        env::set_var(RUST_LOG_ENVIRONMENT_VARIABLE_NAME, LOG_LEVEL);
    }

    env_logger::init();

    let context = ZmqContext::new();
    let awaiting_requests_storage: AwaitingRequestsStorage = DeadLockSafeRwLock::default();

    let sender = context
        .socket(SocketType::DEALER)
        .expect("[SYSTEM] failed to initialize sender socket");

    log::debug!("[SYSTEM] initialized sender socket");

    sender
        .connect(BUS_ROUTER_SOCKET_ADDR.as_str())
        .expect("[SYSTEM] failed to connect to BUS router socket.");

    log::debug!("[SYSTEM] sender has connected to BUS router socket");

    let receiver = context
        .socket(SocketType::SUB)
        .expect("[SYSTEM] failed to initialize receiver socket");

    log::debug!("[SYSTEM] initialized receiver socket");

    for publisher_address in BUS_PUBLISHERS_SOCKET_ADDRS.iter() {
        receiver
            .connect(publisher_address.as_str())
            .unwrap_or_else(|error| {
                panic!(
                    "[SYSTEM] connection to BUS publisher socket '{}' failed with: {}",
                    publisher_address, error
                )
            });

        receiver.set_subscribe(b"").unwrap_or_else(|error| {
            panic!(
                "[SYSTEM] subscription to BUS publisher socket '{}' failed with: {}",
                publisher_address, error
            )
        });
    }

    log::debug!(
        "[SYSTEM] receiver has connected to all BUS publishers: {}",
        BUS_PUBLISHERS_SOCKET_ADDRS.join(", ")
    );

    let mut total_received_messages_count = 0;
    let awaiting_requests_storage_clone = awaiting_requests_storage.clone();

    log::debug!("[SYSTEM] running messages receiving loop");

    drop(thread::spawn(move || 'receive_messages: loop {
        let message_bytes = match receiver.recv_bytes(ZEROMQ_ZERO_FLAG) {
            Ok(message_bytes) => message_bytes,
            Err(error) => {
                log::error!("[RECEIVER] failed to receive message because of: {}", error);
                continue 'receive_messages;
            }
        };

        log::trace!("< {:?}", message_bytes);

        let (message_kind, message_bytes_without_kind) =
            match decode_message_kind(message_bytes) {
                Ok(message_kind_and_left_bytes) => message_kind_and_left_bytes,
                Err(error) => {
                    log::error!(
                        "[RECEIVER] failed to decode message kind because of: {}",
                        error
                    );
                    continue 'receive_messages;
                }
            };

        if !(matches!(message_kind, ZeromqMessageKind::ValueMultiplicationResponse)) {
            log::trace!(
                "[RECEIVER] ignored message with unexpected kind {:?}",
                message_kind
            );
            continue 'receive_messages;
        }

        let (uuid, message_payload_bytes) = decode_message_uuid(message_bytes_without_kind);

        match awaiting_requests_storage_clone.read(move |awaiting_requests_storage| {
            awaiting_requests_storage.get(&uuid).cloned()
        }) {
            Some(RequestData {
                expected_result, ..
            }) => {
                log::trace!("[RECEIVER] attempt to decode payload");

                let payload = match decode_message_payload::<'_, ValueMultiplicationResponse>(
                    message_payload_bytes.as_slice(),
                ) {
                    Ok(payload) => payload,
                    Err(error) => {
                        log::error!(
                            "[RECEIVER] failed to decode message payload because of: {}",
                            error
                        );
                        continue 'receive_messages;
                    }
                };

                log::trace!("[RECEIVER] compare expected and received values");

                match expected_result.cmp(&payload.result) {
                    Ordering::Greater | Ordering::Less => {
                        log::error!("[RECEIVER] received message with unexpected payload");
                    }
                    Ordering::Equal => {
                        total_received_messages_count += 1;
                    }
                }

                log::trace!("[RECEIVER] request completed, removing from storage");

                // Drop copy allowed because dropped value is not written in any variable.
                #[allow(clippy::drop_copy)]
                drop(awaiting_requests_storage_clone.write(
                    move |awaiting_requests_storage| awaiting_requests_storage.remove(&uuid),
                ));

                log::trace!(
                    "[RECEIVER] request {} completed and removed from storage",
                    uuid
                );

                if total_received_messages_count % REQUESTS_COUNT_INSIDE_ONE_GROUP == 0 {
                    log::debug!(
                        "[RECEIVER] {:?} - total received {} messages",
                        SystemTime::now(),
                        total_received_messages_count
                    );
                }
            }
            None => {
                log::error!("[RECEIVER] received message with unexpected uuid: {}", uuid);
            }
        }
    }));

    log::debug!("[SYSTEM] running messages sending loop");

    let mut total_sended_messages_count = 0;
    let sender_loop_join_handle = thread::spawn(move || {
        let mut rng = thread_rng();
        let mut last_resend_check = Instant::now();

        #[allow(unused_labels)]
        'send_messages: loop {
            let should_resend_requests = Instant::now().duration_since(last_resend_check)
                > RESEND_REQUESTS_EVERY_DURATION;
            let resend_requests: Rc<VecDeque<(Uuid, RequestData)>> =
                Rc::new(VecDeque::default());
            let mut resend_requests_clone = Rc::clone(&resend_requests);

            if should_resend_requests {
                last_resend_check = Instant::now();
                awaiting_requests_storage.write(move |awaiting_requests_storage| {
                    let resend_requests_iter = awaiting_requests_storage
                        .iter_mut()
                        .filter_map(|(uuid, request_data)| {
                            if request_data.should_resend_request() {
                                request_data.update_last_send_attempt_time();
                                Some((*uuid, request_data.clone()))
                            } else {
                                None
                            }
                        });
                    Rc::make_mut(&mut resend_requests_clone).extend(resend_requests_iter);
                });

                log::debug!("[SENDER] resend {} requests", resend_requests.len());
            }

            let mut total_messages_sent_inside_current_group = 0;

            'send_messages_group: while total_messages_sent_inside_current_group
                < REQUESTS_COUNT_INSIDE_ONE_GROUP
            {
                let mut is_resend = false;
                let mut cloned_resend_requests = Rc::clone(&resend_requests);
                let current_resend_requests = Rc::make_mut(&mut cloned_resend_requests);
                let (current_value, current_multiplier, current_request, current_uuid) =
                    if let Some((
                        uuid,
                        RequestData {
                            value, multiplier, ..
                        },
                    )) = current_resend_requests.pop_front()
                    {
                        is_resend = true;

                        (
                            value,
                            multiplier,
                            ValueMultiplicationRequest { multiplier, value },
                            uuid,
                        )
                    } else {
                        let current_value = i64::from(rng.gen::<u8>());
                        let current_multiplier = i64::from(rng.gen::<u8>());

                        (
                            current_value,
                            current_multiplier,
                            ValueMultiplicationRequest {
                                value: current_value,
                                multiplier: current_multiplier,
                            },
                            Uuid::new_v4(),
                        )
                    };

                let message_bytes = match encode_message(current_uuid, current_request) {
                    Ok(message_bytes) => message_bytes,
                    Err(error) => {
                        log::error!("[SENDER] failed to encode message because of: {}", error);
                        continue 'send_messages_group;
                    }
                };

                if let Err(error) =
                    sender.send(Message::from(message_bytes.clone()), ZEROMQ_ZERO_FLAG)
                {
                    log::error!("[SENDER] failed to send message because of: {}", error);
                    continue 'send_messages_group;
                }

                total_messages_sent_inside_current_group += 1;
                log::trace!("> {:?}", message_bytes);

                // If we resend the request, then it has already been written to the storage.
                if !is_resend {
                    // Drop copy allowed because dropped value is not written in any variable.
                    #[allow(clippy::drop_copy)]
                    drop(
                        awaiting_requests_storage.write(move |awaiting_requests_storage| {
                            awaiting_requests_storage.insert(
                                current_uuid,
                                RequestData::new(current_value, current_multiplier),
                            )
                        }),
                    );
                }

                total_sended_messages_count += 1;
            }

            if total_sended_messages_count % REQUESTS_COUNT_INSIDE_ONE_GROUP == 0 {
                log::debug!(
                    "[SENDER] {:?} - total sended {} messages",
                    SystemTime::now(),
                    total_sended_messages_count
                );
            }
        }
    });

    sender_loop_join_handle
        .join()
        .expect("[SYSTEM] failed to wait sender thread to finish");

    unreachable!("[SYSTEM] somethink gone wrong");
}
