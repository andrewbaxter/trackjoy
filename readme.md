# Automatic launching

Use the `trackjoy-juggler` command, with the same config you use for `trackjoy`:

```
# trackjoy-juggler config.json
```

This monitors `/dev/input/by-path` for close-by groups of trackpads and keyboards matching the configuration. When it finds groups, it'll launch `trackjoy` for them with the same config.

Note, this uses `udevadm` to check if a device is a trackpad (uses the `hid-multitouch` driver).

It relies on the path format being `PHYSPATH-TAG` where `PHYSPATH` ends with the USB path, and `TAG` is something like `kbd` or `event-kbd` or `mouse`, etc.

Both `udevadm` and `trackjoy` must be in your environment's `PATH`.
