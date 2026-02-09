# awbw-notifications

Cloud Run service that checks Advance Wars By Web (AWBW) turns and posts updates to a Discord webhook. It persists state (signature + cookies) in Google Cloud Storage, and refreshes cookies automatically by logging in with credentials stored in Secret Manager.

## How it works

1. Load state (`state.json`) from GCS.
2. Reuse stored cookies to fetch the AWBW "Your Turn" page.
3. If not logged in, perform a programmatic login and retry.
4. Parse game IDs and build a signature.
5. If the signature changed, post a Discord message.
6. Persist updated state and cookies back to GCS.

## Configuration

Environment variables:

- `PROJECT_ID`: GCP project ID for Secret Manager access.
- `BUCKET_NAME`: GCS bucket that stores `state.json`.
- `STATE_OBJECT`: Optional object name (default: `state.json`).
- `PORT`: Port for Cloud Run (default: `8080`).

Secrets in Secret Manager:

- `AWBW_USERNAME`
- `AWBW_PASSWORD`
- `DISCORD_WEBHOOK_URL`

## Local development

```bash
cargo run
```

## Tests

```bash
cargo test
```

## Deployment (Docker)

```bash
docker build -t awbw-notifier .
```

Deploy to Cloud Run with your preferred CI/CD or `gcloud run deploy` using the built image.
