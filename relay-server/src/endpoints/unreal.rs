use actix_web::{actix::ResponseFuture, HttpRequest, HttpResponse};
use futures::Future;

use relay_general::protocol::EventId;

use crate::body::ForwardBody;
use crate::constants::UNREAL_USER_HEADER;
use crate::endpoints::common::{self, BadStoreRequest};
use crate::envelope::{ContentType, Envelope, Item, ItemType};
use crate::extractors::{RequestMeta, StartTime};
use crate::service::{ServiceApp, ServiceState};

fn extract_envelope(
    request: &HttpRequest<ServiceState>,
    meta: RequestMeta,
    max_payload_size: usize,
) -> ResponseFuture<Envelope, BadStoreRequest> {
    let user_id = request.query().get("UserID").map(String::to_owned);
    let future = ForwardBody::new(request, max_payload_size)
        .map_err(|_| BadStoreRequest::InvalidUnrealReport)
        .and_then(move |data| {
            let mut envelope = Envelope::from_request(Some(EventId::new()), meta);

            let mut item = Item::new(ItemType::UnrealReport);
            item.set_payload(ContentType::OctetStream, data);
            envelope.add_item(item);

            if let Some(user_id) = user_id {
                envelope.set_header(UNREAL_USER_HEADER, user_id);
            }

            Ok(envelope)
        });

    Box::new(future)
}

fn store_unreal(
    meta: RequestMeta,
    start_time: StartTime,
    request: HttpRequest<ServiceState>,
) -> ResponseFuture<HttpResponse, BadStoreRequest> {
    let event_size = request.state().config().max_attachment_payload_size();

    common::handle_store_like_request(
        meta,
        true,
        start_time,
        request,
        move |data, meta| extract_envelope(data, meta, event_size),
        // The return here is only useful for consistency because the UE4 crash reporter doesn't
        // care about it.
        common::create_text_event_id_response,
    )
}

pub fn configure_app(app: ServiceApp) -> ServiceApp {
    common::cors(app)
        .resource(
            &common::normpath("api/{project:\\d+}/unreal/{sentry_key:\\w+}/"),
            |r| {
                r.name("store-unreal");
                r.post().with(store_unreal);
            },
        )
        .register()
}
