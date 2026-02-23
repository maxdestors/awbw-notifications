#!/usr/bin/env bash
set -euo pipefail

# ----------------------------
# User-configurable variables
# ----------------------------
PROJECT_ID="${PROJECT_ID:-games-n-co-486920}"
REGION="${REGION:-europe-west1}"

SERVICE_NAME="${SERVICE_NAME:-awbw-notifier}"
BUCKET_NAME="${BUCKET_NAME:-${PROJECT_ID}-awbw-state}"
STATE_OBJECT="${STATE_OBJECT:-state.json}"

# Service accounts
RUN_SA_NAME="${RUN_SA_NAME:-awbw-notifier-run-sa}"
SCHED_SA_NAME="${SCHED_SA_NAME:-awbw-notifier-scheduler-sa}"

# Scheduler
SCHEDULER_JOB_NAME="${SCHEDULER_JOB_NAME:-awbw-notifier-job}"
SCHEDULER_CRON="${SCHEDULER_CRON:-1-59/6 7-22 * * *}"   # Every 6 min, 07:00-22:59 Europe/Paris
SCHEDULER_TZ="${SCHEDULER_TZ:-Europe/Paris}"
SCHEDULER_PATH="${SCHEDULER_PATH:-/run}"                # Cloud Run endpoint
ALLOW_UNAUTH="${ALLOW_UNAUTH:-false}"                   # keep false (recommended)

# Secrets (names in Secret Manager)
SECRET_AWBW_USERNAME="${SECRET_AWBW_USERNAME:-AWBW_USERNAME}"
SECRET_AWBW_PASSWORD="${SECRET_AWBW_PASSWORD:-AWBW_PASSWORD}"
SECRET_DISCORD_WEBHOOK="${SECRET_DISCORD_WEBHOOK:-DISCORD_WEBHOOK_URL}"

echo "==> Using PROJECT_ID=$PROJECT_ID REGION=$REGION"
gcloud config set project "$PROJECT_ID" >/dev/null

echo "==> Enabling required APIs..."
gcloud services enable \
  run.googleapis.com \
  cloudscheduler.googleapis.com \
  secretmanager.googleapis.com \
  storage.googleapis.com \
  artifactregistry.googleapis.com \
  cloudbuild.googleapis.com

echo "==> Creating GCS bucket (if not exists): gs://$BUCKET_NAME"
if ! gsutil ls -b "gs://$BUCKET_NAME" >/dev/null 2>&1; then
  gsutil mb -p "$PROJECT_ID" -l "$REGION" "gs://$BUCKET_NAME"
else
  echo "    Bucket already exists."
fi

echo "==> Creating service account for Cloud Run (if not exists): $RUN_SA_NAME"
RUN_SA_EMAIL="${RUN_SA_NAME}@${PROJECT_ID}.iam.gserviceaccount.com"
if ! gcloud iam service-accounts describe "$RUN_SA_EMAIL" >/dev/null 2>&1; then
  gcloud iam service-accounts create "$RUN_SA_NAME" \
    --display-name="AWBW Notifier (Cloud Run)"
else
  echo "    Cloud Run SA already exists."
fi

echo "==> Creating service account for Cloud Scheduler (if not exists): $SCHED_SA_NAME"
SCHED_SA_EMAIL="${SCHED_SA_NAME}@${PROJECT_ID}.iam.gserviceaccount.com"
if ! gcloud iam service-accounts describe "$SCHED_SA_EMAIL" >/dev/null 2>&1; then
  gcloud iam service-accounts create "$SCHED_SA_NAME" \
    --display-name="AWBW Notifier (Cloud Scheduler)"
else
  echo "    Scheduler SA already exists."
fi

echo "==> Granting Cloud Run SA permissions..."
# Secret access
for SECRET in "$SECRET_AWBW_USERNAME" "$SECRET_AWBW_PASSWORD" "$SECRET_DISCORD_WEBHOOK"; do
  # Create secrets if missing (empty placeholder)
  if ! gcloud secrets describe "$SECRET" >/dev/null 2>&1; then
    echo "    Creating secret placeholder: $SECRET"
    printf "" | gcloud secrets create "$SECRET" --data-file=- >/dev/null
  else
    echo "    Secret exists: $SECRET"
  fi

  gcloud secrets add-iam-policy-binding "$SECRET" \
    --member="serviceAccount:${RUN_SA_EMAIL}" \
    --role="roles/secretmanager.secretAccessor" >/dev/null
done

# Bucket access for state.json
gsutil iam ch "serviceAccount:${RUN_SA_EMAIL}:objectAdmin" "gs://${BUCKET_NAME}" >/dev/null

echo "==> Granting Scheduler SA permission to invoke Cloud Run (set later once service exists)."
echo "    (This will be applied in the deploy script once Cloud Run URL is known.)"

echo
echo "==> NEXT STEPS:"
echo "1) Set secret values (recommended):"
echo "   printf "your username" | gcloud secrets versions add ${SECRET_AWBW_USERNAME} --data-file=-"
echo "   printf "your password" | gcloud secrets versions add ${SECRET_AWBW_PASSWORD} --data-file=-"
echo "   printf "webhook URL" | gcloud secrets versions add ${SECRET_DISCORD_WEBHOOK} --data-file=-"
echo
echo "2) Run deployment script after you put real secret values:"
echo "   ./pipeline_build_deploy.sh"
echo
echo "Done."
