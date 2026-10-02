# VibeWatch Firmware

Firmware for the VibeWatch smartwatch (ESP32-S3-Touch-AMOLED-2.06, Rust /
esp-idf-svc): remote agent sessions over MQTT, BLE provisioning, OTA updates,
and Tailscale connectivity via the [microlink](https://github.com/L-jasmine/microlink)
component.

## Configuring Tailscale

The watch joins your tailnet with an auth key provisioned over BLE. Everything
below happens on the device — no USB or build flags required.

### 1. Generate an auth key

In the [Tailscale admin console](https://login.tailscale.com/admin/settings/keys),
generate an **auth key** (`tskey-auth-...`). A one-off key is enough: after the
first registration the watch caches its own node keys, and new keys can be
bound the same way at any time.

### 2. Provision the key over BLE

1. On the watch: **Settings → Enable BLE**. It advertises as `Watch` and shows
   a hint screen.
2. On your phone (Chrome or Edge, Web Bluetooth): open `assets/setup.html` and
   connect to the watch.
3. Fill in WiFi / MQTT broker as usual, then scroll to the **Tailscale** card,
   paste the auth key, and tap **Bind Tailscale**.

Binding wipes any previous tailnet registration on the watch, stores the key,
and switches the screen to the **Tailscale page**: your VPN IP and the list of
nodes in the tailnet (online nodes highlighted). The page refreshes once per
second; back button or right swipe exits (a *Shutting down* box shows while
the tunnel is torn down).

### 3. Open the page again later

**Settings → Tailscale** opens the same node-list page using the key stored in
NVS. If no key has been provisioned yet, the page says so and returns.

### Remote sessions over the tailnet

If the MQTT broker URL points at a tailnet address (`100.64.0.0/10`), the
watch brings the tunnel up by itself before connecting: it joins the tailnet,
moves its DERP mailbox to the broker's home region, kicks the WireGuard
handshake, and only then starts MQTT. Time is (re-)synced first because the
DERP relay speaks TLS.

### DERP region

`CONFIG_ML_DERP_REGION` in `sdkconfig.defaults` (default `3`, Singapore)
selects the watch's own relay region. Set it to the region your other devices
home on — relayed packets only reach peers connected to the same region until
a direct path is punched (the watch probes for one automatically).
