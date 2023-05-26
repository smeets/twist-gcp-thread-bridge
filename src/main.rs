use tide::prelude::*;
use tide::{Request, StatusCode};

trait SaveLoad {
    fn load(&mut self);
    fn save(&self);
}
trait RegisterFind {
    fn register_twist_thread(&mut self, cfg: TwistOnConfigure);
    fn find_twist_thread(&self, secret_id: String) -> Option<TwistIntegration>;
    fn unregister_twist_thread(self: &mut Self, install_id: String);
}

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
}

impl SaveLoad for FileStore {
    fn load(self: &mut Self) {
        let data = std::fs::read_to_string(&self.path).unwrap();
        self.twist_integrations = serde_json::from_str(data.as_str()).unwrap();
    }

    fn save(self: &Self) {
        let data = serde_json::to_string(&self.twist_integrations).unwrap();
        std::fs::write(&self.path, data).unwrap();
    }
}
impl RegisterFind for FileStore {
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
impl ApplicationStore for FileStore {}

trait ApplicationStore: Send + SaveLoad + RegisterFind {}

#[derive(Clone)]
struct State {
    server_name: String,
    store: std::sync::Arc<std::sync::Mutex<Box<dyn ApplicationStore>>>,
}

impl State {
    pub fn new(name: &str, store: Box<dyn ApplicationStore>) -> Self {
        Self {
            server_name: name.to_string(),
            store: std::sync::Arc::new(std::sync::Mutex::new(store)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TwistOnConfigure {
    install_id: String,
    post_data_url: String,
    user_id: String,
    user_name: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct GoogleWebhookPayload {
    incident: GoogleIncident,
}

#[derive(Debug, Serialize, Deserialize)]
struct GoogleIncident {
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
async fn main() -> tide::Result<()> {

    // let data = async_std::fs::read_to_string("data.json").await?;
    // let gcp: GoogleWebhookPayload = serde_json::from_str(&data)?;
    // println!("{}", serde_json::to_string_pretty(&gcp)?);

    tide::log::start();

    let mut file = FileStore::new("db.json");
    file.load();
    file.twist_integrations
        .iter()
        .for_each(|x| tide::log::info!("> {} {}", x.secret_id, x.configuration.user_name));
    let state = State::new("tuta.smeten.se", Box::new(file));

    let mut app = tide::with_state(state);

    app.with(tide::utils::After(|mut res: tide::Response| async {
        if let Some(err) = res.error() {
            res.set_body(err.to_string());
            res.set_status(500);
        }
        Ok(res)
    }));

    app.at("/twist/on_configure").get(twist_configure);
    app.at("/twist/outgoing").post(twist_outgoing);
    app.at("/gcp/webhooks/:id").post(gcp_webhook);
    app.listen("0.0.0.0:9999").await?;

    tide::log::info!("byee!");

    Ok(())
}

async fn twist_content(req: &mut Request<State>) -> Option<String> {
    match req.body_string().await {
        Ok(json) => match serde_json::from_str::<GoogleWebhookPayload>(&json) {
            Ok(payload) => {
                let svc = payload
                .incident
                    .resource
                    .labels
                    .as_object()
                    .and_then(|labels| labels.get("container_name"))
                    .and_then(|name_val| name_val.as_str())
                    .map_or("unknown", |name| name);

                Some(format!(
                    "🚨 {alert} on {name} [incident]({incident_url})\n\n{docs}",
                    alert = payload.incident.policy_name,
                    name = svc,
                    incident_url = payload.incident.url,
                    docs = payload.incident.documentation.content,
                ))
            }
            Err(err) => Some(format!(
                "Failed to parse due to {error}:\n\n```\n{payload}\n```",
                error = err,
                payload = json.to_string()
            )),
        },
        Err(_) => None,
    }
}

async fn gcp_webhook(mut req: Request<State>) -> tide::Result {
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

async fn twist_outgoing(mut req: Request<State>) -> tide::Result {
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

async fn twist_configure(req: Request<State>) -> tide::Result {
    let x: TwistOnConfigure = req.query()?;
    let state = req.state();

    let mut k = state.store.lock().unwrap();
    k.register_twist_thread(x.clone());

    tide::log::info!("configure for {} on {}", x.user_name, x.post_data_url);

    let res = reqwest::blocking::Client::new()
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

A hello message has been sent to your thread and will appear within 2 hours.

GCP Notifications will be relayed in hourly batches.
",
        gcp_url
    )
    .into())
}
