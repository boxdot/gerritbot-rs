use std::{error, fmt, thread};

use futures::{Sink, Future, Stream};
use futures::sync::mpsc::channel;
use futures::stream::BoxStream;
use rusoto_core::{self, default_tls_client, DefaultCredentialsProvider, Region};
use rusoto_sqs::{self, Sqs, SqsClient, ReceiveMessageRequest, DeleteMessageRequest};

#[derive(Debug)]
pub enum Error {
    Credentials(rusoto_core::CredentialsError),
    Tls(rusoto_core::TlsError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Error::Credentials(ref err) => fmt::Display::fmt(err, f),
            Error::Tls(ref err) => fmt::Display::fmt(err, f),
        }
    }
}

impl error::Error for Error {
    fn description(&self) -> &str {
        match *self {
            Error::Credentials(ref err) => err.description(),
            Error::Tls(ref err) => err.description(),
        }
    }

    fn cause(&self) -> Option<&error::Error> {
        match *self {
            Error::Credentials(ref err) => err.cause(),
            Error::Tls(ref err) => err.cause(),
        }
    }
}

impl From<rusoto_core::CredentialsError> for Error {
    fn from(err: rusoto_core::CredentialsError) -> Self {
        Error::Credentials(err)
    }
}

impl From<rusoto_core::TlsError> for Error {
    fn from(err: rusoto_core::TlsError) -> Self {
        Error::Tls(err)
    }
}

pub fn sqs_receiver(
    queue_url: String,
    queue_region: Region,
) -> Result<BoxStream<rusoto_sqs::Message, ()>, Error> {
    // receive messages
    let aws_credentials = DefaultCredentialsProvider::new()?;
    let sqs_client = SqsClient::new(default_tls_client()?, aws_credentials, queue_region);

    let mut receive_req = ReceiveMessageRequest::default();
    receive_req.queue_url = queue_url.clone();
    receive_req.wait_time_seconds = Some(10);

    let (tx, rx) = channel(1);
    thread::spawn(move || -> Result<(), ()> {
        loop {
            let resp = sqs_client.receive_message(&receive_req);
            match resp {
                Ok(resp) => {
                    if let Some(messages) = resp.messages {
                        let mut tx_loop = tx.clone();
                        for msg in messages.into_iter() {
                            match tx_loop.clone().send(msg.clone()).wait() {
                                Ok(s) => {
                                    tx_loop = s;
                                }
                                Err(err) => {
                                    error!("Cannot send message through channel: {:?}", err);
                                    break;
                                }
                            };
                        }
                    };
                }
                Err(err) => warn!("SQS client error: {:?}", err),
            }
        }
    });

    // delete received messages
    let aws_credentials = DefaultCredentialsProvider::new()?;
    let sqs_client = SqsClient::new(default_tls_client()?, aws_credentials, queue_region);
    let rx = rx.and_then(move |msg| {
        if let Some(ref receipt_handle) = msg.receipt_handle {
            let delete_req = DeleteMessageRequest {
                queue_url: queue_url.clone(),
                receipt_handle: receipt_handle.clone(),
            };
            if let Err(err) = sqs_client.delete_message(&delete_req) {
                error!(
                    "Could not delete message with handle {}: {:?}",
                    delete_req.receipt_handle,
                    err
                )
            };
        }
        Ok(msg)
    });

    Ok(rx.boxed())
}
