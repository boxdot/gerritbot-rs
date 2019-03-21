use std::convert::identity;

use futures::{future, stream, Future, Stream};
use log::{error, warn};
use rusoto_core::Region;
use rusoto_sqs::{
    DeleteMessageBatchRequest, DeleteMessageBatchRequestEntry, Message, ReceiveMessageRequest,
    Sqs as _, SqsClient,
};

pub fn sqs_receiver(
    queue_url: String,
    queue_region: Region,
) -> impl Stream<Item = Message, Error = ()> {
    // set up receiver client and receive request template
    let receive_client = SqsClient::new(queue_region.clone());
    let receive_request = ReceiveMessageRequest {
        queue_url: queue_url.clone(),
        wait_time_seconds: Some(10),
        max_number_of_messages: Some(10),
        ..Default::default()
    };
    // set up deleter client and delete request template
    let delete_client = SqsClient::new(queue_region.clone());
    let delete_request = DeleteMessageBatchRequest {
        queue_url: queue_url.clone(),
        ..Default::default()
    };

    // repeatedly poll for messages
    stream::unfold((), move |()| {
        Some(
            receive_client
                .receive_message(receive_request.clone())
                .map(|receive_result| (receive_result, ())),
        )
    })
    // log the errors and skip the errors
    .map_err(|e| error!("failed to receive message: {}", e))
    .then(|result| future::ok(result.ok()))
    .filter_map(identity)
    // delete messages from the queue
    .and_then(move |receive_result| {
        let messages = receive_result.messages.unwrap_or_else(Vec::new);

        if !messages.is_empty() {
            // prepare delete request
            let delete_request = DeleteMessageBatchRequest {
                entries: messages
                    .iter()
                    .filter_map(|message| message.receipt_handle.clone())
                    .enumerate()
                    .map(|(index, receipt_handle)| DeleteMessageBatchRequestEntry {
                        id: index.to_string(),
                        receipt_handle,
                    })
                    .collect(),
                ..delete_request.clone()
            };

            // send delete request
            future::Either::A(delete_client.delete_message_batch(delete_request).then(
                |delete_request_result| {
                    // log errors, if any
                    match delete_request_result {
                        Ok(ref delete_result) if !delete_result.failed.is_empty() => {
                            warn!("failed to delete some messages: {:?}", delete_result.failed);
                        }
                        Ok(_) => (),
                        Err(e) => {
                            error!("message delete request failed: {}", e);
                        }
                    }

                    // forward messages
                    future::ok(messages)
                },
            ))
        } else {
            // timeout, no messages received
            future::Either::B(future::ok(messages))
        }
    })
    // flatten messages to return one by one
    .map(stream::iter_ok)
    .flatten()
}
