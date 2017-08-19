use std::{error, fmt, thread};

use futures::{Sink, Future};
use futures::sync::mpsc::{channel, Receiver};
use rusoto_core::{self, default_tls_client, DefaultCredentialsProvider, Region};
use rusoto_sqs::{self, Sqs, SqsClient, ReceiveMessageRequest};

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

pub fn sqs_receiver(queue_url: String) -> Result<Receiver<rusoto_sqs::Message>, Error> {
    let aws_credentials = DefaultCredentialsProvider::new()?;
    let sqs_client = SqsClient::new(default_tls_client()?, aws_credentials, Region::EuCentral1);
    let mut receive_req = ReceiveMessageRequest::default();
    receive_req.queue_url = queue_url;
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
                            match tx_loop.clone().send(msg).wait() {
                                Ok(s) => tx_loop = s,
                                Err(err) => {
                                    error!("Cannot send message through channel: {:?}", err);
                                    break;
                                }
                            };
                        }
                    }
                }
                Err(err) => warn!("SQS client error: {:?}", err),
            }
        }
    });
    Ok(rx)
}
