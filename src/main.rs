use anyhow::{anyhow, Context, Result};
use axum::{response::IntoResponse, routing::post, Json, Router};
use cookie_store::serde::json;
use cookie_store::CookieStore;
use reqwest::{header, Client};
use reqwest_cookie_store::CookieStoreMutex;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, io::Cursor, net::SocketAddr, sync::Arc};

const AWBW_URL: &str = "https://awbw.amarriner.com/yourgames.php?yourTurn=1";
const LOGIN_URL: &str = "https://awbw.amarriner.com/login.php";
const BASE_URL: &str = "https://awbw.amarriner.com/";

#[derive(Debug, Serialize, Deserialize, Default)]
struct State {
    sig: String,
    count: Option<u32>,
    cookies_json: Option<String>,
}

#[derive(Debug, Serialize)]
struct RunResponse {
    ok: bool,
    changed: bool,
    count: u32,
    logged_in: bool,
    posted: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let app = Router::new().route("/run", post(run_handler));

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8080);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    axum::serve(tokio::net::TcpListener::bind(addr).await?, app).await?;
    Ok(())
}

async fn run_handler() -> axum::response::Response {
    match run().await {
        Ok(resp) => (axum::http::StatusCode::OK, Json(resp)).into_response(),
        Err(e) => {
            eprintln!("[ERROR] {:#}", e);
            axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn run() -> Result<RunResponse> {
    let bucket = std::env::var("BUCKET_NAME").context("BUCKET_NAME env missing")?;
    let state_object = std::env::var("STATE_OBJECT").unwrap_or_else(|_| "state.json".to_string());

    let mut state = gcs_read_state(&bucket, &state_object)
        .await
        .unwrap_or_default();

    let jar = if let Some(cj) = &state.cookies_json {
        json::load(Cursor::new(cj.as_bytes())).unwrap_or_else(|_| CookieStore::default())
    } else {
        CookieStore::default()
    };
    let jar = Arc::new(CookieStoreMutex::new(jar));

    let client = Client::builder()
        .cookie_provider(jar.clone())
        .user_agent("awbw-turn-checker/1.0")
        .build()?;

    let mut html = fetch_awbw_page(&client).await?;
    let mut logged_in = !html.contains("You must be logged in");

    if !logged_in {
        eprintln!("[INFO] Login required");

        let username = std::env::var("AWBW_USERNAME").context("AWBW_USERNAME env missing")?;
        let password = std::env::var("AWBW_PASSWORD").context("AWBW_PASSWORD env missing")?;
        login_awbw(&client, &username, &password).await?;

        html = fetch_awbw_page(&client).await?;
        logged_in = !html.contains("You must be logged in");
        if !logged_in {
            return Err(anyhow!("Login failed: still not logged in after POST"));
        }
    }

    if !html.contains("Your Turn Games") {
        eprintln!("[WARN] This is not the Your Turn Games page !");
    }

    let (count, ids) = extract_turn_info(&html);
    let sig = make_signature(count, &ids);

    let changed = !state.sig.is_empty() && state.sig != sig;

    state.sig = sig.clone();
    state.count = Some(count);
    state.cookies_json = Some(save_cookie_store_json(jar.clone())?);

    gcs_write_state(&bucket, &state_object, &state).await?;

    let mut posted = false;
    if changed {
        let webhook =
            std::env::var("DISCORD_WEBHOOK_URL").context("DISCORD_WEBHOOK_URL env missing")?;
        let msg = build_discord_message(count, &ids);
        post_discord(&client, &webhook, &msg).await?;
        posted = true;
    }

    Ok(RunResponse {
        ok: true,
        changed,
        count,
        logged_in,
        posted,
    })
}

async fn fetch_awbw_page(client: &Client) -> Result<String> {
    let resp = client.get(AWBW_URL).send().await?;
    let body = resp.text().await?;
    Ok(body)
}

fn extract_turn_info(html: &str) -> (u32, Vec<u32>) {
    let turn_count = regex_capture_u32(html, r"Your Turn Games\s*\((\d+)\)").unwrap_or(0);
    let start_count =
        regex_capture_u32(html, r"Your Games Waiting to Start \s*\((\d+)\)").unwrap_or(0);
    let count = turn_count + start_count;

    let mut ids = Vec::new();
    let re = regex::Regex::new(r#"href\s*=\s*["']?game\.php\?games_id=(\d+)"#).unwrap();
    for cap in re.captures_iter(html) {
        if let Ok(id) = cap[1].parse::<u32>() {
            ids.push(id);
        }
    }
    ids.sort_unstable();
    ids.dedup();

    (count, ids)
}

fn make_signature(count: u32, ids: &[u32]) -> String {
    use sha2::{Digest, Sha256};
    let source = if !ids.is_empty() {
        ids.iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join(",")
    } else {
        format!("count:{count}")
    };
    let mut h = Sha256::new();
    h.update(source.as_bytes());
    format!("{:x}", h.finalize())
}

fn build_discord_message(count: u32, ids: &[u32]) -> String {
    if count == 0 && ids.len() == 0 {
        return format!("✅ **AWBW** → No pending turns");
    }
    let links = ids
        .iter()
        .take(5)
        .map(|id| format!("[{id}]({BASE_URL}game.php?games_id={id})"))
        .collect::<Vec<_>>()
        .join(" • ");

    let mut s = format!("🎮 **AWBW ({count})** → [All]({AWBW_URL})");
    if !links.is_empty() {
        s.push_str(&format!(" • {links}"));
    }
    if ids.len() > 5 {
        s.push_str(&format!(" • +{} more", ids.len() - 5));
    }
    s
}

async fn post_discord(client: &Client, webhook: &str, content: &str) -> Result<()> {
    let payload = serde_json::json!({ "content": content });
    let resp = client
        .post(webhook)
        .header(header::CONTENT_TYPE, "application/json")
        .json(&payload)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let txt = resp.text().await.unwrap_or_default();
        return Err(anyhow!("Discord webhook failed: {} {}", status, txt));
    }
    Ok(())
}

async fn login_awbw(client: &Client, username: &str, password: &str) -> Result<()> {
    let login_page = client.get(LOGIN_URL).send().await?.text().await?;

    let form = {
        let mut form = HashMap::<String, String>::new();
        let doc = Html::parse_document(&login_page);
        let input_sel = Selector::parse("form input").unwrap();

        for input in doc.select(&input_sel) {
            let v = input.value();
            let name = match v.attr("name") {
                Some(n) => n.to_string(),
                None => continue,
            };
            let value = v.attr("value").unwrap_or("").to_string();
            if v.attr("type").unwrap_or("").eq_ignore_ascii_case("hidden") {
                form.insert(name, value);
            }
        }

        form.insert("username".into(), username.into());
        form.insert("password".into(), password.into());
        form
    };

    let resp = client.post(LOGIN_URL).form(&form).send().await?;

    let body = resp.text().await.unwrap_or_default();

    if body.contains("Login")
        && body.contains("Username")
        && body.contains("Password")
        && body.contains("Forgot Password")
    {
        eprintln!("[WARN] Login response looks like login page again");
    }
    Ok(())
}

fn save_cookie_store_json(jar: Arc<CookieStoreMutex>) -> Result<String> {
    let jar = jar.lock().unwrap();
    let mut buf = Vec::new();
    cookie_store::serde::json::save(&jar, &mut buf)
        .map_err(|err| anyhow!("cookie store serialize failed: {err}"))?;
    Ok(String::from_utf8(buf)?)
}

async fn gcs_read_state(bucket: &str, object: &str) -> Result<State> {
    let token = access_token_for_gcs().await?;
    let obj = urlencoding::encode(object);
    let url = format!(
        "https://storage.googleapis.com/storage/v1/b/{}/o/{}?alt=media",
        bucket, obj
    );

    let resp = reqwest::Client::new()
        .get(url)
        .bearer_auth(token)
        .send()
        .await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        eprintln!("[INFO] state file not found");
        return Ok(State::default());
    }
    if !resp.status().is_success() {
        return Err(anyhow!("gcs read failed: {}", resp.status()));
    }
    let text = resp.text().await?;
    Ok(serde_json::from_str(&text)?)
}

async fn gcs_write_state(bucket: &str, object: &str, state: &State) -> Result<()> {
    let token = access_token_for_gcs().await?;
    let obj = urlencoding::encode(object);
    let url = format!(
        "https://storage.googleapis.com/upload/storage/v1/b/{}/o?uploadType=media&name={}",
        bucket, obj
    );

    let body = serde_json::to_string(state)?;
    let resp = reqwest::Client::new()
        .post(url)
        .bearer_auth(token)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let txt = resp.text().await.unwrap_or_default();
        return Err(anyhow!("gcs write failed: {} {}", status, txt));
    }
    Ok(())
}

async fn access_token_for_gcs() -> Result<String> {
    let scopes = &["https://www.googleapis.com/auth/devstorage.read_write"];
    let provider = gcp_auth::provider().await?;
    let token = provider.token(scopes).await?;
    Ok(token.as_str().to_string())
}

fn regex_capture_u32(text: &str, pat: &str) -> Option<u32> {
    let re = regex::Regex::new(pat).ok()?;
    let cap = re.captures(text)?;
    cap.get(1)?.as_str().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_turn_info_finds_count_and_ids() {
        let html = r#"
        <body>
            <h1>Your Turn Games (2)</h1>
            <a href="game.php?games_id=123">Game</a>
            <a href="game.php?games_id=456">Game</a>
            <a href="game.php?games_id=123">Duplicate</a>
        </body>
        "#;

        let (count, ids) = extract_turn_info(html);

        assert_eq!(count, 2);
        assert_eq!(ids, vec![123, 456]);
    }

    #[test]
    fn extract_turn_info_finds_start_count_and_ids() {
        let html = r#"
        <body>
            <h1>Your Games Waiting to Start (1)</h1>
            <table>Game 1</table>

            <h1>Your Turn Games (2)</h1>
            <a href="game.php?games_id=111">Game</a>
            <a href="game.php?games_id=222">Game</a>
            <a href="game.php?games_id=111">Duplicate</a>
        </body>
        "#;

        let (count, ids) = extract_turn_info(html);

        assert_eq!(count, 3);
        assert_eq!(ids, vec![111, 222]);
    }

    #[test]
    fn build_discord_message_handles_no_games() {
        let msg = build_discord_message(0, &[]);

        assert!(msg.contains("No pending turns"));
    }

    #[test]
    fn build_discord_message_handles_one_start_game_no_ids() {
        let msg = build_discord_message(1, &[]);

        assert_eq!(
            msg,
            "🎮 **AWBW (1)** → [All](https://awbw.amarriner.com/yourgames.php?yourTurn=1)"
        );
    }

    #[test]
    fn build_discord_message_handles_two_games() {
        let msg = build_discord_message(2, &[111, 222]);

        assert_eq!(
            msg,
            "🎮 **AWBW (2)** → [All](https://awbw.amarriner.com/yourgames.php?yourTurn=1) • [111](https://awbw.amarriner.com/game.php?games_id=111) • [222](https://awbw.amarriner.com/game.php?games_id=222)"
        );
    }

    #[test]
    fn build_discord_message_handles_6_games() {
        let msg = build_discord_message(6, &[6, 5, 4, 3, 2, 1]);

        assert_eq!(
            msg,
            "🎮 **AWBW (6)** → [All](https://awbw.amarriner.com/yourgames.php?yourTurn=1) • [6](https://awbw.amarriner.com/game.php?games_id=6) • [5](https://awbw.amarriner.com/game.php?games_id=5) • [4](https://awbw.amarriner.com/game.php?games_id=4) • [3](https://awbw.amarriner.com/game.php?games_id=3) • [2](https://awbw.amarriner.com/game.php?games_id=2) • +1 more"
        );
    }

    #[test]
    fn make_signature_changes_with_ids() {
        let sig_one = make_signature(1, &[10, 20]);
        let sig_two = make_signature(1, &[10, 30]);

        assert_ne!(sig_one, sig_two);
    }

    #[test]
    fn make_signature_equals_with_ids() {
        let sig_one = make_signature(1, &[10, 20, 30]);
        let sig_two = make_signature(1, &[10, 30, 20]);

        assert_ne!(sig_one, sig_two);
    }
}
