use async_signal::{Signal, Signals};
use async_std::io;
use async_std::prelude::FutureExt;
use async_std::stream::StreamExt;

use tide::prelude::*;
use tide::{Request, StatusCode};

use argh::FromArgs;

// commands

#[derive(FromArgs, PartialEq, Debug)]
/// GCP <-> Twist Webhook Bridge
struct RunOpts {
    #[argh(subcommand)]
    nested: BridgeSubcommand,
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand)]
enum BridgeSubcommand {
    PrintReply(BridgeCmdPrintReply),
    Serve(BridgeCmdServe),
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "print-reply")]
/// Print reply to webhook input.
struct BridgeCmdPrintReply {
    /// hostname of server
    #[argh(option)]
    input_filename: String,
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "serve")]
/// Run http server.
struct BridgeCmdServe {
    /// hostname of server
    #[argh(option)]
    server_name: String,

    /// listener bind addr
    #[argh(option, default = "\"127.0.0.1:9999\".to_string()")]
    bind_addr: String,

    /// database filename
    #[argh(option, default = "\"db.json\".to_string()")]
    db: String,
}

// application

struct FileStore {
    path: String,
    twist_integrations: std::vec::Vec<TwistIntegration>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TwistIntegration {
    secret_id: String,
    configuration: TwistOnConfigure,
}

impl FileStore {
    pub fn new(path: &str) -> Self {
        Self {
            path: path.to_string(),
            twist_integrations: std::vec::Vec::new(),
        }
    }

    fn load(self: &mut Self) {
        let data = std::fs::read_to_string(&self.path).unwrap_or("[]".to_string());

        self.twist_integrations = serde_json::from_str(data.as_str()).unwrap();
    }

    fn save(self: &Self) {
        let data = serde_json::to_string(&self.twist_integrations).unwrap();
        std::fs::write(&self.path, data).unwrap();
    }

    fn register_twist_thread(self: &mut Self, cfg: TwistOnConfigure) {
        self.twist_integrations.push(TwistIntegration {
            secret_id: cfg.install_id.clone(),
            configuration: cfg,
        });
        self.save();
    }

    fn unregister_twist_thread(self: &mut Self, install_id: String) {
        if let Some(idx) = self
            .twist_integrations
            .iter()
            .position(|x| x.secret_id == install_id)
        {
            self.twist_integrations.remove(idx);
            self.save();
        }
    }

    fn find_twist_thread(&self, secret_id: String) -> Option<TwistIntegration> {
        if let Some(twist) = self
            .twist_integrations
            .iter()
            .find(|&x| x.secret_id == secret_id)
        {
            Some(twist.clone())
        } else {
            None
        }
    }
}

// tide server state

#[derive(Clone)]
struct ServerState {
    server_name: String,
    store: std::sync::Arc<std::sync::Mutex<FileStore>>,
}

impl ServerState {
    pub fn new(name: &str, store: FileStore) -> Self {
        Self {
            server_name: name.to_string(),
            store: std::sync::Arc::new(std::sync::Mutex::new(store)),
        }
    }
}

// google webhook structs

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum GoogleWebhookPayload {
    GoogleLogAlert(GoogleLogAlert),
    GoogleUptimeAlert(GoogleUptimeAlert),
}

#[derive(Debug, Serialize, Deserialize)]
struct GoogleUptimeAlert {
    incident: GoogleUptimeIncident,
}

#[derive(Debug, Serialize, Deserialize)]
struct GoogleUptimeIncident {
    policy_name: String,
    url: String,
    summary: String,
    state: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct GoogleLogAlert {
    incident: GoogleLogIncident,
}

#[derive(Debug, Serialize, Deserialize)]
struct GoogleLogIncident {
    documentation: AlertDocumentation,
    policy_name: String,
    resource: GoogleResource,
    url: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct GoogleResource {
    labels: serde_json::Value,
    #[serde(rename = "type")]
    resource_type: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct AlertDocumentation {
    content: String,
    mime_type: String,
}

#[async_std::main]
async fn main() -> io::Result<()> {
    let opts: RunOpts = argh::from_env();
    return match opts.nested {
        BridgeSubcommand::PrintReply(cmd) => {
            let data = async_std::fs::read_to_string(cmd.input_filename).await?;
            if let Some(reply) = reply_to_json(data) {
                println!("{}", reply);
            }
            Ok(())
        }
        BridgeSubcommand::Serve(cmd) => serve(cmd).await,
    };
}

async fn serve(opts: BridgeCmdServe) -> io::Result<()> {
    tide::log::start();

    let mut file = FileStore::new(&opts.db);
    file.load();
    file.twist_integrations
        .iter()
        .for_each(|x| tide::log::info!("> {} {}", x.secret_id, x.configuration.user_name));
    let state = ServerState::new(&opts.server_name, file);

    let mut app = tide::with_state(state);

    app.with(tide::utils::After(|mut res: tide::Response| async {
        if let Some(err) = res.error() {
            res.set_body(err.to_string());
            res.set_status(500);
        }
        Ok(res)
    }));

    app.at("/ready").get(|_| async { Ok("OK") });
    app.at("/twist/on_configure").get(twist_configure);
    app.at("/twist/outgoing").post(twist_outgoing);
    app.at("/gcp/webhooks/:id").post(gcp_webhook);

    let quit = async {
        let mut signals = Signals::new([Signal::Term, Signal::Quit, Signal::Int])?;
        while let Some(sig) = signals.next().await {
            eprintln!("quitting due to received signal: {:?}", sig);
            return Ok(());
        }
        Ok(())
    };

    return app.listen(opts.bind_addr).race(quit).await;
}

fn reply_to_json(json: String) -> Option<String> {
    match serde_json::from_str::<GoogleWebhookPayload>(&json) {
        Ok(payload) => match payload {
            GoogleWebhookPayload::GoogleLogAlert(alert) => {
                let svc = alert
                    .incident
                    .resource
                    .labels
                    .as_object()
                    .and_then(|labels| labels.get("container_name"))
                    .and_then(|name_val| name_val.as_str())
                    .map_or("unknown", |name| name);

                Some(format!(
                    "🚨 {alert} on {name} [incident]({incident_url})\n\n{docs}",
                    alert = alert.incident.policy_name,
                    name = svc,
                    incident_url = alert.incident.url,
                    docs = alert.incident.documentation.content,
                ))
            }
            GoogleWebhookPayload::GoogleUptimeAlert(alert) => Some(format!(
                "{state} {alert} [incident]({incident_url})\n\n{summary}",
                alert = alert.incident.policy_name,
                incident_url = alert.incident.url,
                summary = alert.incident.summary,
                state = if alert.incident.state == "open" {
                    "🚨"
                } else {
                    "✅"
                },
            )),
        },
        Err(err) => Some(format!(
            "Failed to parse due to {error}:\n\n```\n{payload}\n```",
            error = err,
            payload = json.to_string()
        )),
    }
}

/// gcp webhook handler forwards a message to twist
async fn gcp_webhook(mut req: Request<ServerState>) -> tide::Result {
    match twist_content(&mut req).await {
        Some(reply) => {
            let webhook_id = req.param("id")?;
            let store = req.state().store.lock().unwrap();
            if let Some(twist) = store.find_twist_thread(webhook_id.to_string()) {
                reqwest::blocking::Client::new()
                    .request(reqwest::Method::POST, twist.configuration.post_data_url)
                    .body(serde_json::to_string(&json!({
                        "content": reply,
                    }))?)
                    .header("Content-Type", "application/json")
                    .send()
                    .unwrap();
            } else {
                tide::log::warn!("no twist integration found with id {}", webhook_id);
            }
        }
        None => {}
    };

    Ok("OK".into())
}

async fn twist_content(req: &mut Request<ServerState>) -> Option<String> {
    match req.body_string().await {
        Ok(json) => reply_to_json(json),
        Err(_) => None,
    }
}

/// twist outgoing webhook
async fn twist_outgoing(mut req: Request<ServerState>) -> tide::Result {
    #[derive(Debug, Deserialize)]
    struct Outgoing {
        /// message, thread, comment, uninstall, ping
        event_type: String,
        user_id: String,
        user_name: String,

        /// only on message, thread or comment
        content: Option<String>,

        /// only when event_type = uninstall
        install_id: Option<String>,
    }

    #[derive(Debug, Serialize)]
    struct Reply {
        content: String,
    }

    let x: Outgoing = req.body_json().await?;
    let mut state = req.state().store.lock().unwrap();

    Ok(match x.event_type.as_str() {
        "ping" => {
            let pong = Reply {
                content: "pong".into(),
            };
            let mut res = tide::Response::new(StatusCode::Ok);
            res.body_json(&pong)?;
            res
        }
        "message" => {
            let mut res = tide::Response::new(200);
            res.body_json(&json!({"content": ""}))?;
            res
        }
        "uninstall" => {
            state.unregister_twist_thread(x.install_id.unwrap());
            let mut res = tide::Response::new(200);
            res.body_json(&json!({"content": "uninstalled!"}))?;
            res
        }
        _ => tide::Response::new(400),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TwistOnConfigure {
    install_id: String,
    post_data_url: String,
    user_id: String,
    user_name: String,
}

/// twist configure/install integration handler
async fn twist_configure(req: Request<ServerState>) -> tide::Result {
    let x: TwistOnConfigure = req.query()?;
    let state = req.state();

    let mut k = state.store.lock().unwrap();
    k.register_twist_thread(x.clone());

    tide::log::info!("configure for {} on {}", x.user_name, x.post_data_url);

    let _res = reqwest::blocking::Client::new()
        .request(reqwest::Method::POST, x.post_data_url)
        .body(serde_json::to_vec(&json!({
            "content": "Hello from the other side.",
        }))?)
        .header("Content-Type", "application/json")
        .send()
        .unwrap();

    let gcp_url = format!(
        "https://{}/gcp/webhooks/{}",
        state.server_name, x.install_id
    );

    Ok(format!(
        "
Twist configuration successful.

# GCP Notification Channel
Webhook URL: {}

A hello message has been sent to your thread and should appear per integration settings.

GCP Notifications will be show up in the thread as per integration settings.
",
        gcp_url
    )
    .into())
}
