# Initial research: the NZXT GitHub organisation and the Linux ecosystem

> Research state: 30 July 2026. Sources limited to the official `NZXTCorp` GitHub organisation, its repositories, its code, its tags and its releases.
>
> Decision note: this document keeps the exploration and its initial recommendation as history. The current direction is a hardware-only GPUI application, defined in the [PRD](./tasks/prd-native-nzxt-hardware-control.md). Web Integrations are out of scope.

## Verdict

The public NZXT organisation contains no NZXT CAM code, no NZXT device driver, no USB/HID specification, no VID/PID table and no public API to drive fans, pumps, RGB, LCD screens or firmware. It publishes no authentication contract or NZXT cloud API usable for a clone either.

The genuinely interesting find is the **Web Integrations** trio. It documents a JavaScript interface that CAM injects into a Chromium browser, and the pipeline that displays a web page on the screen of a Kraken. It is not a hardware control API, but it is a good compatibility contract: a Linux CAM could reimplement `window.nzxt.v1`, then run the existing Kraken integrations without modifying them.

The few HID, USB and monitoring forks present in the organisation are old generic libraries. They hint at the historical building blocks of CAM, not at the protocol of NZXT devices.

## Verified scope

The [GitHub API of the organisation](https://api.github.com/orgs/NZXTCorp) reports 59 public repositories. The [official inventory](https://api.github.com/orgs/NZXTCorp/repos?per_page=100&type=public) contains only 7 non-forks and 52 forks. A repository search on `cam`, `kraken` and `hardware` returns, on the CAM side, only `web-integrations-docs`, `web-integrations-types` and `web-integrations-examples`.

The 7 original repositories were inspected, along with the forks whose name or content could touch HID, USB, GPU or displays. No NZXT-specific hardware control code emerged. That is a conclusion about what is published in this organisation, not proof that the protocols do not exist in private repositories or in the CAM binary.

## What is usable

### 1. `web-integrations-types`: the monitoring contract to reproduce

The [`v1/index.d.ts`](https://github.com/NZXTCorp/web-integrations-types/blob/dc41ac2fc12e2c47320d253f1130478d184f162c/v1/index.d.ts) file defines:

- `window.nzxt.v1.onMonitoringDataUpdate(data)`, called once per second by CAM;
- the display attributes `width`, `height`, `shape` (`circle` or `square`) and `targetFps`;
- a `MonitoringData` object made of `cpus`, `gpus`, `ram` and `kraken`;
- for CPU and GPU: load, temperatures, frequencies, fan speed and power;
- for RAM: size, usage, modules and frequencies;
- for the Kraken: `liquidTemperature` only.

The units and conventions are precise enough to serve as a public schema: load between 0 and 1, temperatures in Celsius, frequencies in MHz, fans in RPM, power in watts and memory in MiB. Numeric values may be `undefined`.

**Value for the project: high, but only at the UI compatibility level.** A Linux implementation can collect its own metrics, normalise them to these types and inject the same object into a web renderer. Nothing in this repository makes it possible to open a Kraken, read its HID reports, set a curve or send a frame to the LCD.

The repository is small, written in TypeScript, and under the [MIT licence](https://github.com/NZXTCorp/web-integrations-types/blob/dc41ac2fc12e2c47320d253f1130478d184f162c/LICENSE). Its last commit on `main` dates from [17 September 2024](https://github.com/NZXTCorp/web-integrations-types/commit/dc41ac2fc12e2c47320d253f1130478d184f162c). The `v0.4.1` tag points at that commit, but the most recent published GitHub release is [`v0.4.0`](https://github.com/NZXTCorp/web-integrations-types/releases/tag/v0.4.0), dated 31 August 2023.

There is one inconsistency not to propagate: the [README](https://github.com/NZXTCorp/web-integrations-types/blob/dc41ac2fc12e2c47320d253f1130478d184f162c/README.md) shows `npm install @nzxt/web-integrations`, whereas the [`package.json`](https://github.com/NZXTCorp/web-integrations-types/blob/dc41ac2fc12e2c47320d253f1130478d184f162c/package.json) and the documentation use `@nzxt/web-integrations-types`.

### 2. `web-integrations-docs`: the architecture of the LCD mode

The [development documentation](https://github.com/NZXTCorp/web-integrations-docs/blob/1f769ba5a75c65c656aeb0946ab6ca8f509075ba/pages/docs/development.md) reveals the behaviour of CAM:

1. CAM opens two Chromium browsers: a visible configuration browser and a hidden "Kraken Browser".
2. The Kraken Browser loads the same URL with `?kraken=1`.
3. Both contexts share the session state of the same origin, notably `localStorage` and the cookies.
4. CAM injects the screen geometry, the target FPS and the monitoring callback into the Kraken Browser.
5. CAM renders the content of the Kraken Browser, then sends it to the Kraken screen.

The documentation states that monitoring data is available from CAM 4.50.0 onwards. It also documents two URI schemes, `nzxt-cam://` and `nzxt-cam-beta://`, including the `action/load-web-integration?url=...` action. These are local deep links into CAM, not a cloud API or a hardware channel.

The [FAQ](https://github.com/NZXTCorp/web-integrations-docs/blob/1f769ba5a75c65c656aeb0946ab6ca8f509075ba/pages/docs/faq.md) lists the supported families: Kraken Elite, Kraken Z and Kraken. The [submission page](https://github.com/NZXTCorp/web-integrations-docs/blob/1f769ba5a75c65c656aeb0946ab6ca8f509075ba/pages/docs/submissions.md) provides the known profiles:

| Documented family | Resolution | Shape |
|---|---:|---|
| Kraken Z | 320 x 320 | circular |
| Kraken | 240 x 240 | square |
| Kraken Elite | 640 x 640 | circular |

**Value for the project: high, as the functional specification of the LCD subsystem.** The USB driver and the frame transfer format remain entirely absent.

The repository is written in TypeScript/Next.js. Its last commit dates from [25 January 2024](https://github.com/NZXTCorp/web-integrations-docs/commit/1f769ba5a75c65c656aeb0946ab6ca8f509075ba). GitHub detects no licence and the root of the repository contains no `LICENSE` file. Its code must therefore be treated as a behaviour reference, not as reusable code, as long as NZXT has not clarified the licence.

### 3. `web-integrations-examples`: LCD compatibility fixtures

The [README](https://github.com/NZXTCorp/web-integrations-examples/blob/0c3888a99005e0e2d1195aeed97a64e44124ec12/README.md) provides four examples: Google Photos, Spotify, Unsplash and YouTube. They show how to tell the Kraken renderer apart, share state between the two browsers and adapt the display.

**Value for the project: a good set of end-to-end fixtures.** A compatibility test could load these pages in the Linux renderer and check the geometry, the query parameter, the shared sessions and the `window.nzxt.v1` injection.

The repository is under the [MIT licence](https://github.com/NZXTCorp/web-integrations-examples/blob/0c3888a99005e0e2d1195aeed97a64e44124ec12/LICENSE). It looks active at the organisation level, with merges up to [23 July 2026](https://github.com/NZXTCorp/web-integrations-examples/commit/0c3888a99005e0e2d1195aeed97a64e44124ec12), but the recent changes concern the [`community.md`](https://github.com/NZXTCorp/web-integrations-examples/blob/0c3888a99005e0e2d1195aeed97a64e44124ec12/community.md) list. The files of the four official examples have not changed since their initial commit of [12 April 2023](https://github.com/NZXTCorp/web-integrations-examples/commit/fcdf05085c2155ca22fd2341faee1cd3acb7d501).

The OAuth flows present are exclusively those of Google and Spotify. For instance, the Spotify code calls `accounts.spotify.com` and `api.spotify.com`, and the Google code calls `oauth2.googleapis.com`. They document no NZXT login, CAM token or NZXT cloud endpoint. This old example code must remain a fixture, not a production dependency.

## Peripheral building blocks, interesting but not NZXT-specific

### `hidapi-rs`

The [`NZXTCorp/hidapi-rs`](https://github.com/NZXTCorp/hidapi-rs) fork is a generic Rust wrapper around HIDAPI. Its [`Cargo.toml`](https://github.com/NZXTCorp/hidapi-rs/blob/7cdbb94cd8f14ab1240ba392318c02cfd7d9b250/Cargo.toml) exposes Linux `libusb` and `hidraw` backends, and its [README](https://github.com/NZXTCorp/hidapi-rs/blob/7cdbb94cd8f14ab1240ba392318c02cfd7d9b250/README.md) only shows how to open an arbitrary VID/PID and read or write bytes.

It contains no NZXT VID/PID and no HID reports or commands specific to the Kraken, Hue, Grid or Smart Device. The last commit of the fork on its default branch dates from 11 May 2019, it announces version 0.5.2 and it points explicitly at the upstream project. MIT licence.

**Decision:** a historical hint confirming that HID is a plausible route, but a poor base to take as is. Pick a maintained Linux library once the real protocol is known.

### `periscope-usbid`

The [`periscope-usbid`](https://github.com/NZXTCorp/periscope-usbid) fork is a Python API to walk the Linux USB topology in `/sys/bus/usb/devices`. Its [README](https://github.com/NZXTCorp/periscope-usbid/blob/2a54e0e41024e068fb9d5b1553b2ab19d8d7a039/README.rst) covers buses, ports, interfaces and TTYs, not HID transfers. The last commit dates from 2 February 2016. Its [`setup.py`](https://github.com/NZXTCorp/periscope-usbid/blob/2a54e0e41024e068fb9d5b1553b2ab19d8d7a039/setup.py) declares a simplified BSD licence.

**Decision:** possibly useful as a sysfs enumeration reference, of no value for the device protocol.

### `nvapi-rs` and `rust-edid`

[`nvapi-rs`](https://github.com/NZXTCorp/nvapi-rs/blob/c8db27108f97cac5d662e0935a9346759279a819/README.md) provides NVIDIA monitoring through NVAPI, explicitly under Windows. That is incompatible with the Linux target and its last commit goes back to March 2018.

[`rust-edid`](https://github.com/NZXTCorp/rust-edid/blob/d044e9a14d07b51bb0d7d9f52070a07df697f208/README.md) is a generic EDID parser under MIT. EDID describes conventional video displays, not the USB transport of a Kraken LCD. Neither of these forks is a priority building block.

### `enunion` and `km-wrappers`

[`enunion`](https://github.com/NZXTCorp/enunion/blob/14affdad80483bb4eae6d37bffd4173bec35f6ff/README.md) converts Rust enums into TypeScript discriminated unions through N-API. It is relatively recent, under MIT or Apache 2.0, and suggests that NZXT uses a Rust/Node boundary. That can inspire an architecture, but it does not justify introducing Node or N-API into an MVP.

[`km-wrappers`](https://github.com/NZXTCorp/km-wrappers/blob/e2081135047cf847fb8f4df4b4a43708489a2f8e/README.md) only contains Rust wrappers for Windows kernel mode. It is off the Linux target and reveals no NZXT driver.

The two other original repositories, [`crucible`](https://github.com/NZXTCorp/crucible/blob/74a0287fe63add7ce23dec51f2a7e9d28ec301e0/README.md) and `obs-studio-non-fork`, concern the former Forge capture product and OBS. They are unrelated to CAM or to NZXT devices.

## Function coverage for a Linux CAM

| Target function | What the NZXT organisation provides | Coverage |
|---|---|---|
| CPU/GPU/RAM monitoring | Public schema and injection frequency, no Linux collection | Partial |
| Kraken liquid temperature | `kraken.liquidTemperature` field, no read command | Partial |
| Fan control | No protocol, no curve, no channel mapping | None |
| Pump control | No protocol and no setpoint | None |
| RGB | No protocol, effect or LED topology | None |
| Kraken LCD | Browser contract, resolutions, shape and FPS, no frame transport | Partial |
| USB detection | Two old generic libraries, no NZXT identifier | Weak |
| Firmware | No format, endpoint or update mechanism | None |
| NZXT account and cloud | No OAuth, endpoint, schema or NZXT client | None |
| Web Integrations compatibility | Official types, behaviour and examples | Good |

## Practical consequences

The official GitHub organisation provides a **top-of-stack compatibility specification**, not the hardware bottom of the stack. The best use is to:

1. adopt `window.nzxt.v1` as the public interface of the Linux LCD renderer;
2. convert Linux metrics to the official schema;
3. use the MIT examples as compatibility tests;
4. maintain a driver layer separated by family and firmware, since nothing in the organisation guarantees that Kraken, RGB and fan controllers share a protocol;
5. look for the protocols, USB identifiers and command sequences outside the NZXT organisation, or establish them through clean reverse engineering with real devices.

Do not call the project or its API "NZXT CAM" as if it were an official product. The MIT licences of the two repositories allow reuse of their code under their terms, but they do not amount to permission to use the NZXT trademark. For the forked repositories, presence in the NZXT organisation implies neither current maintenance nor product support.

## The repositories outside NZXTCorp that change the strategy

### `liquidctl/liquidctl`: the most complete hardware base

[`liquidctl`](https://github.com/liquidctl/liquidctl) is the priority starting point. The project provides Python drivers and a JSON CLI for many NZXT generations: Kraken X, Z, 2023, 2024 Elite RGB and Plus, Grid+ V3, Smart Device, HUE 2, several RGB & Fan Controllers, H1 V2, E-series power supplies and, on the development branch, the Control Hub.

The [`kraken3.py`](https://github.com/liquidctl/liquidctl/blob/main/liquidctl/driver/kraken3.py) and [`smart_device.py`](https://github.com/liquidctl/liquidctl/blob/main/liquidctl/driver/smart_device.py) drivers contain the VID/PIDs, HID report formats, temperature and RPM reads, curve commands, RGB effects and LCD transfers obtained by reverse engineering. The [Kraken X3/Z3 guide](https://github.com/liquidctl/liquidctl/blob/main/docs/kraken-x3-z3-guide.md) also documents the limits per model and firmware.

Support remains partial on recent hardware: light rings not driven on some Krakens, the Elite 2023 model marked broken in a detection table, GIF unavailable with some 2.x firmwares. These are MVP boundaries to test on the exact hardware, not details to hide behind a generic abstraction.

The code is under GPL-3.0. For a personal GPL project, the short path is to contribute to `liquidctl` or to use it as a backend. Its JSON CLI can also serve as a prototype before any rewrite.

### Kernel `hwmon` and `liquidtux`: prefer the standard Linux interfaces

[`liquidctl/liquidtux`](https://github.com/liquidctl/liquidtux) develops the `hwmon` drivers, several of which are already in Linux: `nzxt-kraken2` since Linux 5.13 and `nzxt-kraken3` since Linux 6.9, with the 2023 Krakens added in 6.10. The mainline [`nzxt-smart2`](https://github.com/torvalds/linux/blob/master/drivers/hwmon/nzxt-smart2.c) driver covers several Smart Device V2 and RGB & Fan Controllers.

That makes it possible to read and set temperatures, RPM and PWM through `/sys/class/hwmon`, without reimplementing the protocol in the application. The most recent 2024 hardware is not yet fully covered by the mainline kernel tables as of 30 July 2026, so a direct `liquidctl` fallback remains necessary.

### CoolerControl: the "Linux CAM" already largely exists

[`coolercontrol/coolercontrol`](https://gitlab.com/coolercontrol/coolercontrol) already provides a systemd daemon, a web UI, a desktop application, `hwmon`/`liquidctl`/GPU auto-detection, fan profiles, modes, alerts, RGB and LCD. It exposes a [complete REST API](https://docs.coolercontrol.org/automation/scripting.html) on local port 11987 and a gRPC API aimed mainly at plugins.

The strategic consequence is clear: redoing monitoring, profiles, persistence, resume from sleep, permissions and GPU control would be costly duplication. The differentiating project would rather be a Web Integrations compatible Kraken renderer, plugged into CoolerControl over REST or shipped as a contribution or plugin. CoolerControl is under GPLv3+.

### OpenRGB: useful if the RGB scope goes beyond NZXT

[`OpenRGB`](https://gitlab.com/CalcProgrammer1/OpenRGB) supports many NZXT devices and publishes the [current VID/PID matrix](https://openrgb.org/devices.html?search=nzxt), including HUE 2, Kraken X3, several RGB & Fan Controllers and the Kraken 2024 Elite RGB. It is a good C++ reference for effects and LED topology, but it replaces neither thermal control nor the LCD. GPL-2.0 licence.

### `AIOLCDUnchained`: the permissive lead for frame streaming

[`brokenmass/AIOLCDUnchained`](https://github.com/brokenmass/AIOLCDUnchained) is an MIT prototype targeting the Kraken Z3 (`1e71:3008`), Elite 2023 (`1e71:300c`) and Elite 2024 (`1e71:3012`). Its [`driver.py`](https://github.com/brokenmass/AIOLCDUnchained/blob/main/driver.py) documents the HID and bulk USB transfers, the RGBA buckets, as well as a Q565 mode into fast memory that makes it possible to send frames generated in real time.

The current transport uses WinUSB and the documented binaries are for Windows. The value is therefore not the application as it stands, but the protocol and the Q565 Rust encoder to port to libusb on Linux. Since the last push dates from November 2024 and the project remains experimental, every command must be validated on the targeted model and firmware.

### `KrakenZPlayground`: a Linux precedent, but old and narrow

[`ProtozeFOSS/KrakenZPlayground`](https://github.com/ProtozeFOSS/KrakenZPlayground) talks directly over USB to the Z53/Z63/Z73 and can display animations, images and QML views in real time under Linux. It proves that the dynamic pipeline works, but its last release dates from April 2022, its scope is limited to PID `1e71:3008`, and its Qt5/QML dependency is not a choice to adopt by default. GPL-3.0 licence.

## Recommended direction

Do not start with a general-purpose CAM clone. Start with a **Web Integrations runtime for the Kraken under Linux**:

1. support a single model actually owned and identified by its VID/PID and its firmware;
2. let `hwmon` and CoolerControl handle sensors, pumps, fans and thermal safety;
3. reserve direct HID/libusb access for the LCD, in a single daemon so as to avoid concurrent access;
4. load a Web Integration in an isolated Chromium renderer with `?kraken=1`;
5. inject `window.nzxt.v1` and the normalised metrics from the CoolerControl API;
6. capture the framebuffer at the device resolution, encode it, then transmit it to the Kraken;
7. use the official MIT examples as compatibility tests.

The first experiment to run before choosing the UI stack is a spike with no interface: a local animated page, 640 x 640 or 320 x 320 depending on the hardware, frame capture and stable sending for thirty minutes. The real throughput, the USB errors, the CPU cost and the behaviour after sleep will decide whether Web Integrations compatibility is viable.

If the renderer accepts arbitrary URLs, it must stay without a privileged bridge to the daemon or to the file system. The remote page is untrusted code: isolated process, minimal permissions, per-origin storage and an explicit list of the only data injected.

Finally, avoid copy-pasting between GPL-2.0, GPL-3.0 and MIT projects. For a published project, choose the licence before importing code. For a personal MVP, using the existing programs as separate processes immediately reduces the surface to write.
