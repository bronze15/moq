# RTMP ingest + recording + MoQ live

A production-shaped ingest tier for a live platform. Broadcasters publish over
**RTMP** (OBS, mobile, ffmpeg); each stream is **recorded to disk automatically**
and **bridged into MoQ** for low-latency live playback.

```
OBS / phone --RTMP--> MediaMTX ──record──> fMP4 segments ──> MinIO (VOD / diferido)
                          └─runOnReady─> ffmpeg -c copy -> moq-cli publish -> MoQ relay -> viewers (live, sub-second)
```

## Why recording lives here (not on a MoQ subscriber)

MediaMTX owns the exact RTMP connection lifecycle, so a recording starts the
instant a stream goes live and is **finalized cleanly the instant it stops**.
A MoQ subscriber, by contrast, can't always tell when a publisher disconnected,
so subscriber-side recordings risk losing their tail and never finalizing. For a
large platform, ingest-side recording is the robust, low-surprise choice.

## What you get

- **RTMP in** on `:1935` — publish to `rtmp://<host>:1935/<stream-key>`.
- **Automatic recording** to `recordings/<stream-key>/...` as 6s fragmented-MP4
  segments. Each stream's recording finalizes on disconnect.
- **MoQ live bridge** — every stream is republished to your relay as
  `<stream-key>.hang`, so existing MoQ players watch it live with sub-second
  latency.
- **Optional MinIO sync** — mirror recordings to object storage as they land.

## Prerequisites

- Docker + Docker Compose.
- A **MoQ relay** reachable from the container (set `MOQ_URL`). Run your own
  (`moq-relay`) or point at an existing one.

## Run

```bash
cd deploy/rtmp
cp .env.example .env          # set MOQ_URL (and MinIO vars if you use it)
docker compose up --build     # builds the image (compiles moq-cli once) and starts
```

With MinIO mirroring:

```bash
docker compose --profile minio up --build
```

## Point a broadcaster at it

In OBS: **Settings -> Stream -> Service: Custom**

- Server: `rtmp://<host>:1935`
- Stream Key: `ana` (any key; becomes broadcast `ana.hang` in MoQ and folder
  `recordings/ana/`)

ffmpeg equivalent:

```bash
ffmpeg -re -i input.mp4 -c:v libx264 -c:a aac -f flv rtmp://<host>:1935/ana
```

## Where things end up

- **Recordings:** `deploy/rtmp/recordings/<stream-key>/<timestamp>.mp4` (fMP4
  segments). Build an HLS/DASH VOD from them, or serve directly.
- **Live (MoQ):** broadcast `<stream-key>.hang` on your relay — watch with
  `@moq/watch`, `moq-cli subscribe`, etc.

## Authentication (stream key validated by your backend)

Publishing is authenticated by default. The **stream key is the path**, so a
broadcaster connects to `rtmp://<host>:1935/<stream-key>` and MediaMTX asks your
backend whether that key may publish.

Set `RTMP_AUTH_URL` in `.env` to your endpoint. On each publish, MediaMTX sends
it a `POST` with JSON:

```json
{
  "user": "",
  "password": "",
  "ip": "203.0.113.7",
  "action": "publish",
  "path": "ana",
  "protocol": "rtmp",
  "id": "...",
  "query": ""
}
```

Your backend returns **2xx to allow**, **401/403 to deny**. Validate `path`
(the stream key / token) against the broadcaster it belongs to. Internal RTSP
reads (the MoQ bridge) and the API/metrics are excluded from auth, so they don't
hit your backend.

**Local testing without a backend:** set `RTMP_AUTH_METHOD=internal` in `.env`
to allow any key, then switch back to `http` for production.

## Auto-start on boot (systemd)

```bash
# Clone the repo to a stable path and configure:
sudo git clone https://github.com/<you>/moq.git /opt/moq
cd /opt/moq/deploy/rtmp
sudo cp .env.example .env && sudoedit .env        # set MOQ_URL + RTMP_AUTH_URL

# Install and enable the service:
sudo cp systemd/moq-rtmp-ingest.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now moq-rtmp-ingest
sudo journalctl -u moq-rtmp-ingest -f
```

The unit builds the image on first start (compiles `moq-cli` once), runs the
stack in the foreground so logs reach `journalctl`, restarts on failure, and
tears the stack down on stop. Edit `WorkingDirectory` if you cloned elsewhere.
For the MinIO sidecar, add `--profile minio` to the `ExecStart` compose line.

## Production notes

- **Scale:** run multiple ingest nodes behind your stream-key router; MediaMTX is
  stateless per stream. Recordings can write straight to a mounted object-store
  gateway, or use the MinIO sync sidecar.
- **Transcode/ABR:** add ffmpeg renditions in `runOnReady` if you need multiple
  qualities; `-c copy` (default here) is passthrough only.
- **Ports:** expose `:1935` (RTMP) to broadcasters; keep the API `:9997` and RTSP
  `:8554` internal (the Compose file does not publish them).
- **MediaMTX version:** pinned via `MEDIAMTX_VERSION` in the Dockerfile; bump as
  needed.

## Status / testing

The configuration and Compose file are validated (`docker compose config`). The
image build (which compiles `moq-cli`) and a full end-to-end run depend on your
host (Docker, a reachable relay, an RTMP source), so validate the live path in
your environment. The recording path is stock MediaMTX and needs no MoQ.
