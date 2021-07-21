use actix::prelude::dev::ToEnvelope;
use actix::prelude::*;
use futures::prelude::*;

use crate::actors::envelopes::EnvelopeContext;
use crate::actors::outcome::{Outcome, OutcomeProducer};

pub trait SendWithOutcome<Msg, Item, Error, ResultError, MailboxErrorBuilder, ResultErrorBuilder>
where
    Msg: Message<Result = Result<Item, Error>>,
    MailboxErrorBuilder: FnOnce(MailboxError) -> ResultError + 'static,
    ResultErrorBuilder: FnOnce(Error) -> ResultError + 'static,
{
    fn send_with_outcome_error(
        &self,
        message: Msg,
        envelope_context: &EnvelopeContext,
        outcome_producer: Addr<OutcomeProducer>,
        outcome: Outcome,
        mailbox_error_builder: MailboxErrorBuilder,
        result_error_builder: ResultErrorBuilder,
    ) -> ResponseFuture<Item, ResultError>;
}

impl<A, Msg, Item, Error, ResultError, MailboxErrorBuilder, ResultErrorBuilder>
    SendWithOutcome<Msg, Item, Error, ResultError, MailboxErrorBuilder, ResultErrorBuilder>
    for Addr<A>
where
    A: Actor,
    A: Handler<Msg>,
    A::Context: ToEnvelope<A, Msg>,
    Msg: Message<Result = Result<Item, Error>> + Send + 'static,
    Item: Send + 'static,
    Error: Send + 'static,
    ResultError: Sized + 'static,
    MailboxErrorBuilder: FnOnce(MailboxError) -> ResultError + 'static,
    ResultErrorBuilder: FnOnce(Error) -> ResultError + 'static,
{
    fn send_with_outcome_error(
        &self,
        message: Msg,
        envelope_context: &EnvelopeContext,
        outcome_producer: Addr<OutcomeProducer>,
        outcome: Outcome,
        mailbox_error_builder: MailboxErrorBuilder,
        result_error_builder: ResultErrorBuilder,
    ) -> ResponseFuture<Item, ResultError> {
        let envelope_context = *envelope_context;
        let fut = self
            .send(message)
            .map_err(move |err| {
                envelope_context.send_outcomes(outcome, outcome_producer);
                mailbox_error_builder(err)
            })
            .and_then(|result| result.map_err(|e| result_error_builder(e)));
        Box::new(fut)
    }
}
