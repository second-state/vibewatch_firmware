# VibeWatch

> An ESP32-S3 Rust firmware that turns a round AMOLED smartwatch into a wearable agent terminal: remote Claude-agent sessions over MQTT (session list + live vt100 terminal on your wrist), voice input through a Whisper service, Tailscale connectivity, and OTA updates.

VibeWatch is a Rust firmware for the **Waveshare ESP32-S3-Touch-AMOLED-2.06** watch (1.32" round AMOLED, capacitive touch, speaker + microphone) that puts your coding agents on your wrist:

- **Remote agent sessions** — connects to the companion [vibetty](https://github.com/second-state/vibetty) bridge over MQTT and shows every running session; pick one and watch/drive its terminal live from the watch.
- **Voice input (ASR)** — hold-to-speak on the session screen; mic audio is streamed as WAV to a Whisper-compatible service you configure, and the recognized text is submitted to the session (with an inline editor for corrections).
- **Tailscale** — joins your tailnet via the [microlink](https://github.com/L-jasmine/microlink) component, so the watch can reach brokers and services living inside it (see [Configuring Tailscale](#configuring-tailscale)).
- **BLE provisioning + OTA** — configure WiFi / broker / ASR from a web page over Web Bluetooth; update firmware from the browser or straight from GitHub releases.

## Key features

- **Clock face** — large time display with battery indicator; the watch idles here between sessions.
- **Session list** — subscribes to presence for **all of your vibetty sessions at once**, one row per session with a live status dot (amber = agent working, green = idle); the list refreshes as sessions come and go.
- **Live terminal (TUI)** — vt100 terminal emulation with incremental dirty-rect rendering; a bottom action bar gives quick controls (Speak / Accept / Next / Yolo / Del / Esc), and right-swipe returns to the session list.
- **ASR (voice input)** — recognition runs against an HTTP Whisper service (set `asr_config` in `setup.html`: `uri` / `api_key` / `model`); recognized text opens an inline editor before submission.
- **Web provisioning** — one `setup.html` page configures WiFi networks, MQTT broker, ASR service, and Tailscale over Web Bluetooth; stored in NVS.
- **Dual-partition OTA** — the new image is written to the inactive OTA slot and the watch reboots into it. Two update sources: browser upload (HTTP PUT), or **download-latest** directly from GitHub releases.
- **Time sync** — timezone from IP + HTTP-Date fast check, SNTP only when the clock is actually off (needed for TLS, including the DERP relay).

## Operation

The watch boots into the **clock face**. Tap to open the **main menu** with two entries — **Remote** and **Setting**; back gestures return up one level everywhere.

### Remote mode (MQTT → vibetty)

Remote mode connects to your MQTT broker and lists every vibetty session it announces. Select a session to open its live terminal; use the action bar for quick commands, or the ASR flow to speak input. Exiting a session returns to the session list, and the settings entry inside the remote UI opens the same options as the local one.

### Setting

Options: **OTA Update**, **Sync Time**, **Enable BLE**, **Tailscale**, **Reboot**, **Power Off**. **OTA Update** enters OTA mode in-process: it connects WiFi, starts an HTTP server for browser upload, and offers a **download-latest** button (with a progress bar) to fetch the newest firmware from GitHub releases.

## Multiple WiFi (wifi_list)

The device keeps a **list of WiFi credentials** rather than a single network. On boot it scans the surroundings and **connects to the first network in the list that is currently in range** — the list order is the priority order.

- Up to **8** credentials are stored in NVS (`MAX_WIFI_CREDS`).
- Every mode shares the same list and the same priority logic, so an over-the-air update also connects from wherever you are.

## Setup (web provisioning)

Configuration (WiFi networks, MQTT broker URL, ASR service, Tailscale key, background GIF, audio prompt) lives in NVS and is written through a single web page — **`assets/setup.html`** — over **Web Bluetooth (BLE)**, no cable, no app:

1. On the watch: **Setting → Enable BLE**. It advertises as `Watch` and shows a hint screen.
2. Open `setup.html` in a Web-Bluetooth-capable browser (Chrome / Edge) and click **Connect to VibeWatch**.
3. Fill in your WiFi networks, MQTT broker URL, and ASR service, then **Save Changes**; RESET is written automatically so the config applies on reboot. Uploads for the **audio prompt** (WAV) and **watch background** (GIF) go through their own cards.

You can re-run this any time settings change.

## Configuring Tailscale

The watch joins your tailnet with an auth key provisioned over BLE. Everything below happens on the device — no USB or build flags required.

### 1. Generate an auth key

In the [Tailscale admin console](https://login.tailscale.com/admin/settings/keys), generate an **auth key** (`tskey-auth-...`). A one-off key is enough: after the first registration the watch caches its own node keys, and new keys can be bound the same way at any time.

### 2. Provision the key over BLE

In `setup.html`, scroll to the **Tailscale** card, paste the auth key, and tap **Bind Tailscale**.

Binding wipes any previous tailnet registration on the watch, stores the key, and switches the screen to the **Tailscale page**: your VPN IP and the list of nodes in the tailnet (online nodes highlighted). The page refreshes once per second; back button or right swipe exits (a *Shutting down* box shows while the tunnel is torn down).

### 3. Open the page again later

**Setting → Tailscale** opens the same node-list page using the key stored in NVS. If no key has been provisioned yet, the page says so and returns.

### Remote sessions over the tailnet

If the MQTT broker URL points at a tailnet address (`100.64.0.0/10`), the watch brings the tunnel up by itself before connecting: it joins the tailnet, moves its DERP mailbox to the broker's home region, kicks the WireGuard handshake, and only then starts MQTT. Time is (re-)synced first because the DERP relay speaks TLS.

### DERP region

`CONFIG_ML_DERP_REGION` in `sdkconfig.defaults` (default `3`, Singapore) selects the watch's own relay region. Set it to the region your other devices home on — relayed packets only reach peers connected to the same region until a direct path is punched (the watch probes for one automatically).

## Hardware

Waveshare ESP32-S3-Touch-AMOLED-2.06: ESP32-S3 + PSRAM, 1.32" round AMOLED (410×502) with capacitive touch, ES8311 speaker + ES7210 microphone (I2S).

## Building

Built on [Rust + ESP-IDF](https://github.com/esp-rs), target `xtensa-esp32s3-espidf`. Common commands:

```bash
./build.sh factory      # merged image (bootloader + partition table + app): vibewatch_factory.bin
./build.sh ota          # OTA image (app only, for OTA upload / download-latest): vibewatch_ota.bin
./build.sh all          # both
```

The OTA partition layout is symmetric (`ota_0` / `ota_1`, each 4 MB). The `factory` image includes the bootloader + partition table for first-time flashing; the bare OTA image is app-only for over-the-air updates.

> First build downloads the microlink component from its git repository and compiles ESP-IDF; subsequent builds are incremental.
