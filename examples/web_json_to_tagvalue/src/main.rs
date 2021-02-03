//! Starts an HTTP server on any open port and listens for JSON FIX messages.

use fasters::app::{self, Version};
use fasters::codec::json;
use fasters::codec::tagvalue;
use fasters::codec::{Decoder, Encoder};
use fasters::Dictionary;

#[tokio::main]
async fn main() -> tide::Result<()> {
    server().listen("127.0.0.1:8080").await?;
    Ok(())
}

fn server() -> tide::Server<State> {
    let state = State::new();
    let mut app = tide::with_state(state);
    app.at("/").get(serve_hello_world);
    app.at("/fix-json").post(serve_json_relay);
    app
}

/// [`State`] contains any global state necessary to serve web requests. In this
/// case, JSON (en/de)coding devices.
#[derive(Clone)]
struct State {
    codec: json::Codec<app::slr::Message>,
    transmuter: json::TransPrettyPrint,
}

impl State {
    fn new() -> Self {
        Self::default()
    }
}

impl Default for State {
    fn default() -> Self {
        let dictionary = Dictionary::from_version(Version::Fix42);
        Self {
            codec: json::Codec::new(dictionary),
            transmuter: json::TransPrettyPrint,
        }
    }
}

async fn serve_hello_world(_req: tide::Request<State>) -> tide::Result {
    Ok("Hello, world!".to_string().into())
}

async fn serve_json_relay(mut req: tide::Request<State>) -> tide::Result {
    let mut decoder = (req.state().codec.clone(), req.state().transmuter.clone());
    let message = {
        let body: Vec<u8> = req.body_bytes().await?;
        decoder.decode(&body[..]).unwrap()
    };
    let mut buffer = Vec::new();
    let body_response = {
        let codec = tagvalue::Codec::with_dict(Dictionary::from_version(Version::Fix42));
        let mut encoder = codec;
        encoder.encode(&mut buffer, &message).unwrap();
        let buffer_string = std::str::from_utf8(&buffer[..]).unwrap();
        buffer_string
    };
    Ok(body_response.into())
}

#[cfg(test)]
mod test {
    use super::*;
    use fasters::app::slr;
    use tide::http::{Method, Request, Response};

    /// A simple `Heartbeat` message generated by
    /// <http://www.validfix.com/fix-analyzer.html>.
    const EXAMPLE_JSON_MESSAGE: &str = r#"
{
    "Header": {
        "BeginString": "FIX.4.2",
        "MsgType": "0",
        "MsgSeqNum": "12",
        "SenderCompID": "A",
        "TargetCompID": "B",
        "SendingTime": "20160802-21:14:38.717"
    },
    "Body": {},
    "Trailer": {}
}
"#;

    #[tokio::test]
    async fn hello_world() {
        let server = server();
        let req = Request::new(Method::Get, "http://localhost:8080/");
        let mut response: Response = server.respond(req).await.unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.body_string().await.unwrap(), "Hello, world!");
    }

    #[tokio::test]
    async fn example_heartbeat() {
        let server = server();
        let body_json = EXAMPLE_JSON_MESSAGE;
        let mut req = Request::new(Method::Post, "http://localhost:8080/fix-json");
        req.set_body(body_json);
        let mut response: Response = server.respond(req).await.unwrap();
        let body_tagvalue = response.take_body().into_string().await.unwrap();
        println!("{}", body_tagvalue);
        let mut decoder_json = (
            json::Codec::<slr::Message>::new(Dictionary::from_version(Version::Fix42)),
            json::TransPrettyPrint,
        );
        let mut decoder_tagvalue = (
            tagvalue::Codec::<slr::Message>::with_dict(Dictionary::from_version(Version::Fix42)),
            tagvalue::TransVerticalSlash,
        );
        let msg_json = decoder_json
            .decode(body_json.as_bytes())
            .unwrap();
        let msg_tagvalue = decoder_tagvalue
            .decode(body_tagvalue.as_bytes())
            .unwrap();
        assert_eq!(msg_json.get_field(8), msg_tagvalue.get_field(8));
        assert_eq!(msg_json.get_field(35), msg_tagvalue.get_field(35));
        assert_eq!(msg_json.get_field(49), msg_tagvalue.get_field(49));
        assert_eq!(msg_json.get_field(56), msg_tagvalue.get_field(56));
    }
}
