use tide::http::mime;
use tide::prelude::*;
use tide::{Body, Request, StatusCode};

#[derive(Debug, Deserialize)]
struct TwistOnConfigure {
    install_id: String,
    post_data_url: String,
    user_id: String,
    user_name: String,
}

#[derive(Debug, Deserialize)]
struct GoogleNotificationWebhook {
    version: String,
    subject: String,
    group_info: GroupInfo,
    exception_info: ExceptionInfo,
    event_info: EventInfo,
}

#[derive(Debug, Deserialize)]
struct GroupInfo {
    project_id: String,
    detail_link: String,
}

#[derive(Debug, Deserialize)]
struct ExceptionInfo {
    #[serde(rename = "type")]
    exception_type: String,
    message: String,
}

#[derive(Debug, Deserialize)]
struct EventInfo {
    log_message: String,
    request_method: String,
    request_url: String,
    user_agent: String,
    service: String,
    version: String,
    response_status: String,
}

#[async_std::main]
async fn main() -> tide::Result<()> {
    let mut app = tide::new();

    tide::log::start();

    app.with(tide::utils::After(|mut res: tide::Response| async {
        if let Some(err) = res.error() {
            res.set_body(err.to_string());
            res.set_status(500);
        }
        Ok(res)
    }));

    app.at("/twist")
        .at("/on_configure")
        .get(twist_configure)
        .at("/outgoing")
        .post(twist_outgoing);
    app.at("/gcp/webhooks/:id").post(gcp_webhook);
    app.listen("0.0.0.0:9999").await?;
    Ok(())
}

async fn gcp_webhook(mut req: Request<()>) -> tide::Result {
    let webhook_id = req.param("id")?.to_string();
    let x: GoogleNotificationWebhook = req.body_json().await?;
    tide::log::info!("webhook received {}", webhook_id);
    Ok("OK".into())
}

async fn twist_outgoing(mut req: Request<()>) -> tide::Result {
    #[derive(Debug, Deserialize)]
    struct Outgoing {
        event_type: String, // message, thread, comment, uninstall, ping
        user_id: String,
        user_name: String,

        // only on message, thread or comment
        content: Option<String>,

        // only when event_type = uninstall
        install_id: Option<String>,
    }

    #[derive(Debug, Serialize)]
    struct Reply {
        content: String,
    }

    let x: Outgoing = req.body_json().await?;

    Ok(match x.event_type.as_str() {
        "ping" => {
            let pong = Reply{
                content: "pong".into(),
            };
            let mut res = tide::Response::new(StatusCode::Ok);
            res.body_json(&pong)?;
            res
        },
        "message" => {
            let mut res = tide::Response::new(200);
            res.body_json(&json!({"content": "ok!"}))?;
            res
        },
        // "uninstall" => {}
        _ => tide::Response::new(400),
    })
}

async fn twist_configure(mut req: Request<()>) -> tide::Result {
    let x: TwistOnConfigure = req.query()?;

    tide::log::info!("configure for {} on {}", x.user_name, x.post_data_url);

    #[derive(Debug, Serialize)]
    struct Comment {
        content: String,
    }

    let mut p = tide::http::Request::post(x.post_data_url.as_str());
    p.set_content_type(mime::JSON);
    p.set_body(Body::from_json(&Comment {
        content: "HELLO".into(),
    })?);

    Ok("OK".into())
}
