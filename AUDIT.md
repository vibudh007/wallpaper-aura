# Code audit — 18 November 2026

The audit preserved the approved color calculations and independent zones. Changes cover input handling, event recovery, cache correctness and maintainability.

## Fixed

- Installed config/image watches before the first read. A missing or malformed startup file previously could leave the daemon waiting forever with no watch.
- Treat close-after-write notifications as relevant. Ignoring all access events could miss the final usable version of an image written in place.
- Cache keys now include device, inode, length, nanosecond modification time and change time. Atomic replacements retaining size/mtime invalidate correctly.
- Validate the source and file identity again after decoding, discarding stale work when wallpaper changes during analysis.
- Notification helper EOF now sets an explicit failure flag and wakes the main loop; systemd can restart instead of silently losing notifications. Helper output/lines are bounded to 64 KiB.
- Detect asusd owner changes and rediscover its device/cache after restart.
- Reject non-regular input files; nonblocking opens prevent a FIFO from hanging the daemon. Configuration reads are bounded to 64 KiB. Encoded files are capped at 128 MiB; decoded image buffers at 128 MiB, with decoder allocation/dimension limits retained. Release the full image before palette analysis.
- Correctly decode GSettings string escapes and RON Unicode paths; reject malformed/local-network URI inputs. Normalize relative watched paths.
- Avoid duplicate success logs on cached no-op events. Validate static-mode capability before hardware use. Allow read-only probing without a desktop environment.
- Separate desktop adapters (`sources.rs`), color math (`color.rs`), and orchestration/hardware (`main.rs`). Formatting and Clippy checks pass. `lid.rs` isolates the logind subscription and lid gate.

## Dependencies

OSV queries initially returned maintenance advisories RUSTSEC-2026-0370, RUSTSEC-2026-0371 and RUSTSEC-2026-0372 for the old zvariant/zbus_names crates. These were maintenance advisories, not evidence of an exploited memory error. Migrated to zbus 6.3.0, which incorporates those types. The locked registry dependency set decreased from 129 to 96 packages. A subsequent OSV query returned no advisories for those exact locked versions on the audit date. This is a point-in-time database result, not proof that dependencies contain no vulnerabilities.

## Verification

- Fourteen native tests plus Clippy with warnings denied.
- Atomic replacement with identical size and preserved mtime invalidated the palette correctly.
- Forty successive updates kept file-descriptor and thread counts stable; measured resident memory rose from 5120 to 5472 KiB in that short pre-migration run, without continuing per-update resource accumulation.
- Missing startup image and malformed COSMIC startup state both recovered through notifications without restart or polling.
- FIFO image and pointer inputs failed promptly.
- Approved palette bytes matched the prior executable on synthetic input and the user's neon wallpaper.

## Limits

Own Rust code forbids unsafe code; third-party/system components still contain native/platform code. No buffer overflow or unbounded resource leak was identified in the reviewed own code. Short stress checks do not prove absence of all leaks; no Valgrind/sanitizer run was available. Image decoding can allocate and consume CPU transiently. Poll-free idle operation does not imply zero battery cost during changes.

The existing best-effort asusd brightness race, unavailable independent lightbar color zones, unverified native GNOME/KDE sessions and incomplete stock slideshow signals remain documented in README.md. Notifications are monitored through the desktop's existing helper programs; systemd's control group owns their lifecycle. Hardware brightness/power controls are unchanged by the color-rendering math.

## Final dependency and lid updates

Migrated from zbus 5 to maintained zbus 6.3, removing the three deprecated crates. A fresh OSV query checked all 96 registry packages in Cargo.lock and returned no advisories. This is a known-advisory check, not proof that dependencies are vulnerability-free.

The logind `LidClosed` property on this machine reports `emits-change`, currently false. Added native D-Bus subscription, coalesced transition handling, suppression of filesystem work while closed, checks before/after decoding and between hardware zone writes, and latest-wallpaper refresh on opening. Dry-run analysis remains usable independently. Default tests include lid-state transition coalescing; a separate read-only host test initializes the actual subscription. Physical lid closure/reopening has not been triggered by the agent.

## Deployment verification

The final build passed 15 ordinary native tests, a separate live read-only lid-subscription test, and Clippy with warnings denied. The release service restarted successfully with zero restarts and four successful color writes; keyboard brightness Low and the lightbar power flags remained unchanged. The current service control group used about 6.1 MB immediately after startup. A standalone dry-run measured 4736 KiB RSS, 22.9 ms synthetic replacement latency and zero idle CPU ticks over two seconds. The final event tests also covered watched-directory replacement and notification-helper EOF.

The lid property was verified to emit change notifications on this machine. Closed-lid state suppresses filesystem wakeups, image processing and color writes; reopening queues a refresh of the current wallpaper. Physical close/reopen has not been exercised by the agent. An operation already dispatched to asusd cannot be recalled; the code checks lid state before processing and between zone writes.
